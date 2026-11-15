//! Runs the real `blackbox` binary against a temporary data directory.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime};

struct Env {
    dir: tempfile::TempDir,
}

impl Env {
    fn new() -> Env {
        Env { dir: tempfile::tempdir().unwrap() }
    }

    fn data(&self) -> PathBuf {
        self.dir.path().join("data")
    }

    /// German by default, so the tests do not depend on the Windows language.
    fn run(&self, args: &[&str]) -> Output {
        self.run_raw(&[&["--lang", "de"], args].concat())
    }

    fn run_raw(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_blackbox"))
            .arg("--data-dir")
            .arg(self.data())
            .args(args)
            .output()
            .expect("blackbox runs")
    }

    fn ok_raw(&self, args: &[&str]) -> String {
        let out = self.run_raw(args);
        assert!(out.status.success(), "blackbox {args:?} failed:\n{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Run and expect success; returns stdout.
    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "blackbox {args:?} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Run and expect failure; returns stderr.
    fn fails(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(!out.status.success(), "blackbox {args:?} should fail");
        String::from_utf8_lossy(&out.stderr).into_owned()
    }

    /// A PresentMon-style CSV whose modification time marks the session end.
    fn csv(&self, name: &str, frametime_ms: f64, stutter_every: usize, hours_ago: u64) -> PathBuf {
        let path = self.dir.path().join(name);
        let mut text = String::from("Application,ProcessID,MsBetweenPresents\n");
        for i in 0..6000 {
            let ft = if stutter_every > 0 && i % stutter_every == 0 { frametime_ms * 4.0 } else { frametime_ms };
            text.push_str(&format!("cs2.exe,1,{ft}\n"));
        }
        text.push_str("chrome.exe,2,16.6\n");
        fs::write(&path, text).unwrap();
        let f = fs::File::options().write(true).open(&path).unwrap();
        f.set_modified(SystemTime::now() - Duration::from_secs(hours_ago * 3600)).unwrap();
        path
    }
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

#[test]
fn info_and_empty_views() {
    let env = Env::new();
    let info = env.ok(&["info"]);
    assert!(info.contains("Datenbank:"));
    assert!(info.contains(env.data().to_str().unwrap()));
    assert!(env.ok(&["sessions"]).contains("Noch keine Sessions"));
    assert!(env.ok(&["analyze"]).contains("Kein Leistungseinbruch"));
    assert!(env.ok(&["changes"]).contains("Keine Änderungen"));
    assert!(env.ok(&["state"]).contains("Noch kein Scan"));
}

#[test]
fn import_analyze_exclude_and_report() {
    let env = Env::new();
    for (i, hours) in [50u64, 40, 30, 20].iter().enumerate() {
        let csv = env.csv(&format!("good{i}.csv"), 4.0, 0, *hours);
        assert!(env.ok(&["import", "--game", "steam:730", "--csv", s(&csv), "--exe", "cs2.exe"]).contains("importiert"));
    }
    let bad = env.csv("bad.csv", 5.6, 30, 10);
    env.ok(&["import", "--game", "steam:730", "--csv", s(&bad), "--exe", "cs2.exe"]);

    let sessions = env.ok(&["sessions", "--game", "steam:730"]);
    assert!(sessions.contains("EINBRUCH"), "{sessions}");
    let analyze = env.ok(&["analyze"]);
    assert!(analyze.contains("Einbruch in Session #5"), "{analyze}");

    // Report: Markdown with the comparison table, no personal paths.
    let report = env.ok(&["report", "5"]);
    assert!(report.starts_with("## Blackbox-Bericht:"), "{report}");
    assert!(report.contains("| Ø FPS |"));
    assert!(report.contains("−"), "negative change shown: {report}");
    assert!(!report.contains("C:\\Users"));
    let file = env.dir.path().join("bericht.md");
    env.ok(&["report", "5", "--out", s(&file)]);
    assert_eq!(fs::read_to_string(&file).unwrap(), report);

    // Excluding the slow session removes the regression, including it brings it back.
    assert!(env.ok(&["exclude", "5"]).contains("zählt nicht mehr"));
    assert!(env.ok(&["sessions"]).contains("ausgeschlossen"));
    assert!(env.ok(&["analyze"]).contains("Kein Leistungseinbruch"));
    env.ok(&["include", "5"]);
    assert!(env.ok(&["analyze"]).contains("Einbruch in Session #5"));
}

#[test]
fn errors_are_reported_with_exit_code() {
    let env = Env::new();
    assert!(env.fails(&["exclude", "999"]).contains("existiert nicht"));
    assert!(env.fails(&["report", "999"]).contains("existiert nicht"));
    assert!(env.fails(&["vault", "diff", "999"]).contains("existiert nicht"));
    assert!(env.fails(&["sessions", "--game", "steam:1"]).contains("keine Sessions"));
    let missing = env.dir.path().join("nope.csv");
    assert!(env.fails(&["import", "--game", "steam:730", "--csv", s(&missing)]).contains("nicht lesbar"));
    assert!(env.fails(&["set-presentmon", s(&missing)]).contains("keine Datei"));

    let few = env.dir.path().join("few.csv");
    fs::write(&few, "Application,ProcessID,MsBetweenPresents\ncs2.exe,1,4.0\n").unwrap();
    assert!(env.fails(&["import", "--game", "steam:730", "--csv", s(&few)]).contains("Zu wenige Frames"));
}

#[test]
fn vault_profile_roundtrip_on_an_empty_vault() {
    let env = Env::new();
    let profile = env.dir.path().join("profil.json");
    // Nothing tracked yet in a fresh data dir with no matching games: export refuses.
    fs::write(
        &profile,
        r#"{"format":"blackbox-profile","version":1,"exported_at":0,"games":[{"game_id":"steam:1","game_name":"X","files":[]}]}"#,
    )
    .unwrap();
    let preview = env.ok(&["vault", "import", s(&profile)]);
    assert!(preview.contains("Keine passenden"), "{preview}");
    fs::write(&profile, "{\"format\":\"something-else\"}").unwrap();
    assert!(env.fails(&["vault", "import", s(&profile)]).contains("Blackbox-Profil"));
}

#[test]
fn english_output_help_and_saved_language() {
    let env = Env::new();
    let info = env.ok_raw(&["--lang", "en", "info"]);
    assert!(info.contains("Database:"), "{info}");
    assert!(env.ok_raw(&["--lang", "en", "sessions"]).contains("No sessions recorded yet"));
    assert!(env.ok_raw(&["--lang", "en", "analyze"]).contains("No performance drop detected"));
    let help = env.ok_raw(&["--lang", "en", "--help"]);
    assert!(help.contains("Flight recorder for your gaming PC"), "{help}");
    assert!(help.contains("Show performance drops"), "{help}");
    let help_de = env.ok_raw(&["--lang", "de", "--help"]);
    assert!(help_de.contains("Leistungseinbrüche"), "{help_de}");

    for (i, hours) in [50u64, 40, 30, 20].iter().enumerate() {
        let csv = env.csv(&format!("g{i}.csv"), 4.0, 0, *hours);
        env.ok_raw(&["--lang", "en", "import", "--game", "steam:730", "--csv", s(&csv), "--exe", "cs2.exe"]);
    }
    let bad = env.csv("bad.csv", 5.6, 30, 10);
    assert!(env.ok_raw(&["--lang", "en", "import", "--game", "steam:730", "--csv", s(&bad), "--exe", "cs2.exe"]).contains("imported"));
    assert!(env.ok_raw(&["--lang", "en", "sessions"]).contains("DROP"));
    let report = env.ok_raw(&["--lang", "en", "report", "5"]);
    assert!(report.starts_with("## Blackbox report:"), "{report}");
    assert!(report.contains("| Avg. FPS |"));

    // The saved language applies without --lang.
    assert!(env.ok(&["set-language", "en"]).contains("Language: en"));
    assert!(env.ok_raw(&["analyze"]).contains("drop in session #5"), "saved language is used");
    let out = env.run_raw(&["exclude", "999"]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("does not exist"));
    assert!(env.fails(&["set-language", "fr"]).contains("Unbekannte Sprache"));
}