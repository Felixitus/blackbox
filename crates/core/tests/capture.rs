//! End-to-end test of a recorded session with a PresentMon stand-in
//! (`examples/fake_presentmon.rs`): game detected -> capture started ->
//! game exits -> capture stopped -> metrics computed and stored.
//!
//! The test process itself plays the "game": its executable lives in the
//! folder declared as the install directory.

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use blackbox_core::config::Config;
use blackbox_core::i18n::{capture_note, with_lang, Lang};
use blackbox_core::model::Game;
use blackbox_core::monitor::{Monitor, MonitorEvent};
use blackbox_core::store::Store;

fn fake_presentmon() -> PathBuf {
    // target/<profile>/deps/capture-xxxx.exe -> target/<profile>/examples/
    let exe = std::env::current_exe().unwrap();
    let profile_dir = exe.parent().unwrap().parent().unwrap().to_path_buf();
    let path = profile_dir.join("examples").join("fake_presentmon.exe");
    if !path.is_file() {
        // A full `cargo test` builds examples; a filtered run may not.
        let mut cmd = std::process::Command::new(env!("CARGO"));
        cmd.args(["build", "-p", "blackbox-core", "--example", "fake_presentmon"]);
        if profile_dir.ends_with("release") {
            cmd.arg("--release");
        }
        assert!(cmd.status().unwrap().success(), "fake_presentmon could not be built");
    }
    assert!(path.is_file(), "{} fehlt", path.display());
    path
}

struct Scenario {
    _dir: tempfile::TempDir,
    captures: PathBuf,
    monitor: Monitor,
    store: Store,
}

fn scenario(cfg_lines: &str) -> Scenario {
    let exe = std::env::current_exe().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let captures = dir.path().join("captures");
    fs::create_dir_all(&captures).unwrap();
    fs::write(captures.join("fake.cfg"), cfg_lines.replace("{exe}", &exe.file_name().unwrap().to_string_lossy())).unwrap();
    let config = Config { presentmon_path: Some(fake_presentmon()), min_frames: 200, ..Config::default() };
    let game = Game {
        id: "test:self".into(),
        name: "Testspiel".into(),
        install_dir: exe.parent().unwrap().to_string_lossy().into_owned(),
        build_id: "1".into(),
    };
    let monitor = Monitor::new(&config, captures.clone(), vec![game]).unwrap();
    Scenario { _dir: dir, captures, monitor, store: Store::open_in_memory().unwrap() }
}

/// Let the "game" disappear and poll through the grace period.
fn end(s: &mut Scenario, t0: i64) -> MonitorEvent {
    s.monitor.set_games(Vec::new());
    let mut last = Vec::new();
    for i in 0..3 {
        last = s.monitor.poll(&s.store, t0 + i).unwrap();
    }
    assert_eq!(last.len(), 1, "session should end after the grace period");
    last.remove(0)
}

// Both scenarios share the fixed ETW session name, so they run one after
// the other inside a single test.
#[test]
fn capture_pipeline_with_presentmon_stand_in() {
    // --- normal session: 4 ms per frame = 250 FPS ---
    // A busy background program runs meanwhile; it must show up as load.
    #[allow(clippy::zombie_processes)] // killed below once the session is measured
    let mut burner = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", "$e=(Get-Date).AddSeconds(15); while((Get-Date) -lt $e){}"])
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(1200));
    let mut s = scenario("app={exe}\nft=4.0\n");
    let started = s.monitor.poll(&s.store, 1000).unwrap();
    assert!(
        matches!(&started[..], [MonitorEvent::SessionStarted { capturing: true, .. }]),
        "capture must start: {started:?}"
    );
    // Poll like the app does (it waits seconds between polls); CPU usage is
    // only computed when enough time passed since the previous refresh.
    for i in 1..=4 {
        std::thread::sleep(Duration::from_millis(350));
        assert!(s.monitor.poll(&s.store, 1000 + i).unwrap().is_empty(), "still playing");
    }

    let MonitorEvent::SessionEnded { metrics: Some(m), session_id, .. } = end(&mut s, 1010) else {
        panic!("session must end with metrics");
    };
    assert!((m.avg_fps - 250.0).abs() < 0.5, "avg {}", m.avg_fps);
    assert!((m.p1_low_fps - 250.0).abs() < 0.5, "dwm.exe frames must be ignored");
    assert!(m.frames >= 200);

    let stored = s.store.session(session_id).unwrap().unwrap();
    assert_eq!(stored.metrics, Some(m));
    assert_eq!(stored.capture_note, None);
    assert!(stored.load_samples.is_some(), "background load is sampled during the session");
    let load = s.store.session_load(session_id).unwrap();
    let _ = burner.kill();
    let ps = load.iter().find(|p| p.name.eq_ignore_ascii_case("powershell.exe"));
    assert!(ps.is_some_and(|p| p.avg_cpu >= 1.0), "busy background program not measured: {load:?}");
    assert!(load.iter().all(|p| !p.name.to_lowercase().starts_with("capture")), "the game itself is never background load");

    // The raw capture is deleted, the arguments match the real tool.
    assert!(!s.captures.join(format!("session-{session_id}.csv")).exists());
    let args = fs::read_to_string(s.captures.join("args.txt")).unwrap();
    for expected in ["--output_file", "--session_name", "BlackboxCapture", "--stop_existing_session", "--no_track_gpu"] {
        assert!(args.lines().any(|l| l == expected), "missing argument {expected}: {args}");
    }

    // --- PresentMon without rights: exits at once ---
    let mut s = scenario("fail=1\n");
    let started = s.monitor.poll(&s.store, 2000).unwrap();
    assert!(matches!(&started[..], [MonitorEvent::SessionStarted { capturing: true, .. }]));
    std::thread::sleep(Duration::from_millis(500));
    s.monitor.poll(&s.store, 2005).unwrap();
    let MonitorEvent::SessionEnded { metrics: None, session_id, .. } = end(&mut s, 2010) else {
        panic!("session must end without metrics");
    };
    let note = s.store.session(session_id).unwrap().unwrap().capture_note.unwrap();
    let note = with_lang(Lang::De, || capture_note(&note));
    assert!(note.contains("vorzeitig beendet"), "{note}");

    // --- too short to judge ---
    let mut s = scenario("app={exe}\nft=4.0\n");
    let cfg = Config { presentmon_path: Some(fake_presentmon()), min_frames: 1_000_000, ..Config::default() };
    let game = Game {
        id: "test:self".into(),
        name: "Testspiel".into(),
        install_dir: std::env::current_exe().unwrap().parent().unwrap().to_string_lossy().into_owned(),
        build_id: "1".into(),
    };
    s.monitor = Monitor::new(&cfg, s.captures.clone(), vec![game]).unwrap();
    s.monitor.poll(&s.store, 3000).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let MonitorEvent::SessionEnded { metrics: None, session_id, .. } = end(&mut s, 3010) else {
        panic!("too few frames must give no metrics");
    };
    let note = s.store.session(session_id).unwrap().unwrap().capture_note.unwrap();
    assert!(with_lang(Lang::En, || capture_note(&note)).starts_with("Too few frames"), "{note}");
    assert!(with_lang(Lang::De, || capture_note(&note)).contains("Zu wenige Frames"), "{note}");
}
