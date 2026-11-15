//! `blackbox` command line interface.
//!
//! The CLI is the headless front end of Blackbox: it runs scans, watches for
//! play sessions and prints the journal and the regression analysis.
//! Output and help are German or English (`--lang`, the config's
//! `language`, or the Windows display language).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use blackbox_core::analysis::{self, AnalysisConfig, Verdict};
use blackbox_core::collectors::{self, ScanReport};
use blackbox_core::config::{Config, Paths, PresentMonSource};
use blackbox_core::i18n::{self, t, Lang};
use blackbox_core::model::{ChangeEvent, ChangeKind, Session, Timestamp};
use blackbox_core::monitor::MonitorEvent;
use blackbox_core::service::{Service, ServiceEvent};
use blackbox_core::store::Store;
use blackbox_core::tr;
use blackbox_core::vault::{self, KnownDirs, Reason, Vault};
use blackbox_core::{frames, launchers, monitor, report};
use clap::{Command, CommandFactory, FromArgMatches, Parser, Subcommand};

#[derive(Parser)]
#[command(name = "blackbox", version, about = "Flight recorder for your gaming PC")]
struct Cli {
    /// Data directory (default: %LOCALAPPDATA%\Blackbox). Useful for tests.
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    /// Output language: de or en (default: setting or Windows language)
    #[arg(long, global = true)]
    lang: Option<String>,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Capture the system once and show changes since the last scan
    Scan,
    /// Show the captured system state
    State,
    /// Show the change journal
    Changes {
        /// Period in days
        #[arg(long, default_value_t = 30)]
        days: i64,
    },
    /// Show installed games
    Games,
    /// Keep running: scan regularly and record play sessions
    Watch,
    /// Show recorded sessions with their rating
    Sessions {
        /// Only this game (e.g. steam:730)
        #[arg(long)]
        game: Option<String>,
    },
    /// Show performance drops and their possible causes
    Analyze {
        /// Only this game (e.g. steam:730)
        #[arg(long)]
        game: Option<String>,
    },
    /// Mark a session as not representative (not used for comparison)
    Exclude { session: i64 },
    /// Use an excluded session for comparison again
    Include { session: i64 },
    /// Print a report of a session for forums or support (Markdown)
    Report {
        session: i64,
        /// Write to this file instead of the console
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Import an existing PresentMon/CapFrameX CSV as a session
    Import {
        /// Game id (e.g. steam:730)
        #[arg(long)]
        game: String,
        /// Path to the CSV file
        #[arg(long)]
        csv: PathBuf,
        /// Only use rows of this application (e.g. cs2.exe)
        #[arg(long)]
        exe: Option<String>,
    },
    /// Loadout Vault: backups of game settings
    Vault {
        #[command(subcommand)]
        command: VaultCmd,
    },
    /// Set the path to PresentMon.exe
    SetPresentmon { path: PathBuf },
    /// Set the language: auto, de or en
    SetLanguage { language: String },
    /// Show storage locations and configuration
    Info,
}

#[derive(Subcommand)]
enum VaultCmd {
    /// Show tracked games and files
    List,
    /// Show the backup history of a game
    History {
        /// Game id (e.g. steam:730)
        #[arg(long)]
        game: String,
    },
    /// Show the differences of a backup to the previous version
    Diff { snapshot: i64 },
    /// Write a backup back (the current version is backed up first)
    Restore {
        snapshot: i64,
        /// Only apply personal settings (controls, sensitivity, audio, gameplay);
        /// display and graphics stay
        #[arg(long)]
        personal: bool,
        /// Without this only shows what would happen
        #[arg(long)]
        yes: bool,
    },
    /// Export personal settings to a file for another PC
    Export {
        /// Target file (e.g. profile.json)
        file: PathBuf,
        /// Only these games (repeatable); all if omitted
        #[arg(long)]
        game: Vec<String>,
    },
    /// Apply personal settings from a profile file (display and graphics stay)
    Import {
        file: PathBuf,
        /// Without this only shows what would change
        #[arg(long)]
        yes: bool,
    },
}

/// German help texts; the English ones are the doc comments above.
fn german_help(cmd: Command) -> Command {
    let game = "Nur dieses Spiel (z. B. steam:730)";
    cmd.about("Flugschreiber für den Gaming-PC")
        .mut_arg("data_dir", |a| a.help("Datenverzeichnis (Standard: %LOCALAPPDATA%\\Blackbox). Nützlich für Tests."))
        .mut_arg("lang", |a| a.help("Sprache der Ausgabe: de oder en (Standard: Einstellung bzw. Windows-Sprache)"))
        .mut_subcommand("scan", |c| c.about("System einmal erfassen und Änderungen seit dem letzten Scan anzeigen"))
        .mut_subcommand("state", |c| c.about("Aktuell erfassten Systemzustand anzeigen"))
        .mut_subcommand("changes", |c| c.about("Änderungsjournal anzeigen").mut_arg("days", |a| a.help("Zeitraum in Tagen")))
        .mut_subcommand("games", |c| c.about("Installierte Spiele anzeigen"))
        .mut_subcommand("watch", |c| c.about("Dauerbetrieb: regelmäßig scannen und Spielsessions aufzeichnen"))
        .mut_subcommand("sessions", |c| c.about("Aufgezeichnete Sessions mit Bewertung anzeigen").mut_arg("game", |a| a.help(game)))
        .mut_subcommand("analyze", |c| c.about("Leistungseinbrüche und ihre möglichen Ursachen anzeigen").mut_arg("game", |a| a.help(game)))
        .mut_subcommand("exclude", |c| c.about("Eine Session als nicht repräsentativ markieren (zählt nicht für den Vergleich)"))
        .mut_subcommand("include", |c| c.about("Eine ausgeschlossene Session wieder einbeziehen"))
        .mut_subcommand("report", |c| {
            c.about("Bericht zu einer Session für Forum oder Support ausgeben (Markdown)")
                .mut_arg("out", |a| a.help("In diese Datei schreiben statt auf die Konsole"))
        })
        .mut_subcommand("import", |c| {
            c.about("Eine vorhandene PresentMon-/CapFrameX-CSV als Session importieren")
                .mut_arg("game", |a| a.help("Spiel-ID (z. B. steam:730)"))
                .mut_arg("csv", |a| a.help("Pfad zur CSV-Datei"))
                .mut_arg("exe", |a| a.help("Nur Zeilen dieser Anwendung auswerten (z. B. cs2.exe)"))
        })
        .mut_subcommand("vault", |c| {
            c.about("Loadout Vault: Sicherungen der Spieleinstellungen")
                .mut_subcommand("list", |c| c.about("Überwachte Spiele und Dateien anzeigen"))
                .mut_subcommand("history", |c| c.about("Sicherungsverlauf eines Spiels anzeigen").mut_arg("game", |a| a.help("Spiel-ID (z. B. steam:730)")))
                .mut_subcommand("diff", |c| c.about("Unterschiede einer Sicherung zum vorherigen Stand anzeigen"))
                .mut_subcommand("restore", |c| {
                    c.about("Eine Sicherung zurückschreiben (der aktuelle Stand wird vorher gesichert)")
                        .mut_arg("personal", |a| a.help("Nur persönliche Einstellungen (Steuerung, Empfindlichkeit, Audio, Spiel) übernehmen; Anzeige und Grafik bleiben"))
                        .mut_arg("yes", |a| a.help("Ohne diese Angabe wird nur angezeigt, was passieren würde"))
                })
                .mut_subcommand("export", |c| {
                    c.about("Persönliche Einstellungen für einen anderen PC in eine Datei exportieren")
                        .mut_arg("file", |a| a.help("Zieldatei (z. B. profil.json)"))
                        .mut_arg("game", |a| a.help("Nur diese Spiele (mehrfach angebbar); ohne Angabe alle"))
                })
                .mut_subcommand("import", |c| {
                    c.about("Persönliche Einstellungen aus einer Profildatei übernehmen (Anzeige und Grafik bleiben)")
                        .mut_arg("yes", |a| a.help("Ohne diese Angabe wird nur angezeigt, was sich ändern würde"))
                })
        })
        .mut_subcommand("set-presentmon", |c| c.about("Pfad zu PresentMon.exe festlegen"))
        .mut_subcommand("set-language", |c| c.about("Sprache festlegen: auto, de oder en"))
        .mut_subcommand("info", |c| c.about("Speicherorte und Konfiguration anzeigen"))
}

/// Value of `--name value` or `--name=value` before clap has parsed anything.
fn raw_arg(name: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    let flag = format!("--{name}");
    let prefix = format!("--{name}=");
    args.iter().enumerate().find_map(|(i, a)| {
        if *a == flag {
            args.get(i + 1).cloned()
        } else {
            a.strip_prefix(&prefix).map(String::from)
        }
    })
}

fn paths_for(data_dir: Option<&PathBuf>) -> Result<Paths> {
    Ok(match data_dir {
        Some(dir) => Paths::in_dirs(dir, dir),
        None => Paths::default_locations()?,
    })
}

/// The language is needed before parsing (for the help texts).
fn detect_lang() -> Lang {
    if let Some(l) = raw_arg("lang") {
        return i18n::resolve_setting(&l);
    }
    let data_dir = raw_arg("data-dir").map(PathBuf::from);
    paths_for(data_dir.as_ref())
        .ok()
        .and_then(|p| Config::load(&p.config_file).ok())
        .map(|c| c.lang())
        .unwrap_or_else(i18n::system_lang)
}

fn fmt_time(ts: Timestamp) -> String {
    i18n::format_time(ts)
}

fn fmt_change(c: &ChangeEvent) -> String {
    let c = i18n::display_change(c);
    let detail = match c.kind {
        ChangeKind::Added => {
            let n = c.new_value.as_deref().unwrap_or("");
            tr!("neu: {n}", "new: {n}")
        }
        ChangeKind::Removed => {
            let o = c.old_value.as_deref().unwrap_or("");
            tr!("entfernt (war: {o})", "removed (was: {o})")
        }
        ChangeKind::Modified if c.old_value.is_none() => c.new_value.clone().unwrap_or_default(),
        ChangeKind::Modified => format!(
            "{} → {}",
            c.old_value.as_deref().unwrap_or("?"),
            c.new_value.as_deref().unwrap_or("?")
        ),
    };
    format!("[{}] {}: {}", c.category.label(), c.label, detail)
}

fn fmt_pct(v: f64) -> String {
    format!("{:+.0} %", v * 100.0)
}

fn print_scan(report: &ScanReport) {
    for (name, err) in &report.failures {
        eprintln!("{}", tr!("Hinweis: Sammler `{name}` übersprungen: {err}", "Note: collector `{name}` skipped: {err}"));
    }
    if report.changes.is_empty() {
        println!("{}", t("Keine Änderungen seit dem letzten Scan.", "No changes since the last scan."));
    } else {
        let n = report.changes.len();
        println!("{}", tr!("{n} Änderung(en):", "{n} change(s):"));
        for c in &report.changes {
            println!("  {}", fmt_change(c));
        }
    }
}

fn verdict_text(v: &Verdict) -> String {
    match v {
        Verdict::NoData => t("keine Frame-Daten", "no frame data").into(),
        Verdict::Excluded => t("ausgeschlossen", "excluded").into(),
        Verdict::InsufficientHistory { have, need } => {
            tr!("Basis wird aufgebaut ({have}/{need} Sessions)", "building baseline ({have}/{need} sessions)")
        }
        Verdict::Normal { .. } => t("normal", "normal").into(),
        Verdict::Regression { avg_change, low_change, .. } => {
            let (a, l) = (fmt_pct(*avg_change), fmt_pct(*low_change));
            tr!("EINBRUCH (Ø {a}, 1%-Low {l})", "DROP (avg. {a}, 1% low {l})")
        }
        Verdict::Improvement { avg_change, low_change, .. } => {
            let (a, l) = (fmt_pct(*avg_change), fmt_pct(*low_change));
            tr!("besser (Ø {a}, 1%-Low {l})", "better (avg. {a}, 1% low {l})")
        }
    }
}

fn session_line(s: &Session, verdict: &Verdict) -> String {
    let dur = s.duration_secs().map(|d| format!("{} min", d / 60)).unwrap_or_else(|| t("läuft", "running").into());
    let perf = match &s.metrics {
        Some(m) => {
            let (avg, low, st) = (format!("{:.0}", m.avg_fps), format!("{:.0}", m.p1_low_fps), format!("{:.1}", m.stutters_per_min));
            tr!("Ø {avg} FPS, 1%-Low {low}, {st} Ruckler/min", "avg. {avg} FPS, 1% low {low}, {st} stutters/min")
        }
        None => s.capture_note.as_deref().map(i18n::capture_note).unwrap_or_else(|| t("ohne Messung", "not measured").into()),
    };
    format!("#{:<4} {}  {:>7}  {:<44} {}", s.id, fmt_time(s.started_at), dur, perf, verdict_text(verdict))
}

/// Game ids to show: the requested one or all with recorded sessions.
fn selected_games(store: &Store, game: Option<String>) -> Result<Vec<(String, String)>> {
    let all = store.games_with_sessions()?;
    match game {
        None => Ok(all),
        Some(id) => match all.into_iter().find(|(g, _)| *g == id) {
            Some(hit) => Ok(vec![hit]),
            None => bail!(tr!("Für `{id}` gibt es keine Sessions.", "There are no sessions for `{id}`.")),
        },
    }
}

fn cmd_sessions(store: &Store, game: Option<String>) -> Result<()> {
    let games = selected_games(store, game)?;
    if games.is_empty() {
        println!(
            "{}",
            t(
                "Noch keine Sessions aufgezeichnet. Starte `blackbox watch` und spiele.",
                "No sessions recorded yet. Run `blackbox watch` and play."
            )
        );
    }
    for (id, name) in games {
        let sessions = store.sessions(Some(&id))?;
        let analysis = analysis::analyze_sessions(&sessions, &AnalysisConfig::default());
        println!("\n{name} ({id})");
        for (s, a) in sessions.iter().zip(&analysis) {
            println!("  {}", session_line(s, &a.verdict));
        }
    }
    Ok(())
}

fn cmd_analyze(store: &Store, game: Option<String>) -> Result<()> {
    let mut found = false;
    for (id, name) in selected_games(store, game)? {
        let sessions = store.sessions(Some(&id))?;
        let analysis = analysis::analyze_sessions(&sessions, &AnalysisConfig::default());
        for (s, a) in sessions.iter().zip(&analysis) {
            let (Verdict::Regression { baseline, avg_change, low_change }, Some(m)) = (&a.verdict, &s.metrics) else {
                continue;
            };
            found = true;
            let (sid, when) = (s.id, fmt_time(s.started_at));
            println!("\n{}", tr!("{name}: Einbruch in Session #{sid} vom {when}", "{name}: drop in session #{sid} on {when}"));
            let (now, before, pct) = (format!("{:.0}", m.avg_fps), format!("{:.0}", baseline.avg_fps), fmt_pct(*avg_change));
            println!("{}", tr!("  Ø FPS   {now} statt {before} ({pct})", "  Avg FPS {now} instead of {before} ({pct})"));
            let (now, before, pct) = (format!("{:.0}", m.p1_low_fps), format!("{:.0}", baseline.p1_low_fps), fmt_pct(*low_change));
            println!("{}", tr!("  1%-Low  {now} statt {before} ({pct})", "  1% low  {now} instead of {before} ({pct})"));
            let since = a.last_good_at.unwrap_or(0);
            let since_text = fmt_time(since);
            let suspects = report::suspects_for(store, s, since, &a.baseline_session_ids)?;
            if suspects.is_empty() {
                println!(
                    "{}",
                    tr!(
                        "  Seit der letzten guten Session ({since_text}) wurde keine Änderung erfasst.",
                        "  No change was recorded since the last good session ({since_text})."
                    )
                );
                println!(
                    "{}",
                    t(
                        "  Mögliche Ursachen außerhalb der Erfassung: Temperatur, andere Spielszene.",
                        "  Possible causes outside of what is recorded: temperature, a different game scene."
                    )
                );
            } else {
                println!("{}", tr!("  Änderungen seit der letzten guten Session ({since_text}):", "  Changes since the last good session ({since_text}):"));
                for (i, sus) in suspects.iter().enumerate() {
                    let (n, change, at) = (i + 1, fmt_change(&sus.change), fmt_time(sus.change.detected_at));
                    println!("{}", tr!("  {n}. {change} (erkannt {at})", "  {n}. {change} (detected {at})"));
                    println!("     → {}", sus.hint);
                }
            }
        }
    }
    if !found {
        println!("{}", t("Kein Leistungseinbruch erkannt.", "No performance drop detected."));
    }
    Ok(())
}

fn cmd_watch(store: Store, config: &Config, paths: &Paths) -> Result<()> {
    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = stop.clone();
        ctrlc::set_handler(move || stop.store(true, Ordering::SeqCst))
            .context(t("Strg+C-Handler konnte nicht registriert werden", "Could not register the Ctrl+C handler"))?;
    }

    let mut service = Service::new(store, config, paths)?;
    match config.resolve_presentmon() {
        Some(src) => {
            let path = src.path().display();
            println!("{}", tr!("Frame-Messung über {path}", "Frame capture via {path}"));
        }
        None => println!(
            "{}",
            t(
                "Hinweis: Kein PresentMon gefunden – Sessions werden ohne FPS-Daten erfasst.\n         Einrichten mit: blackbox set-presentmon <Pfad zu PresentMon.exe>",
                "Note: PresentMon not found – sessions are recorded without FPS data.\n      Set it up with: blackbox set-presentmon <path to PresentMon.exe>"
            )
        ),
    }
    println!("{}", t("Blackbox läuft. Beenden mit Strg+C.", "Blackbox is running. Stop with Ctrl+C."));

    let poll_every = Duration::from_secs(config.poll_interval_secs);
    while !stop.load(Ordering::SeqCst) {
        match service.tick() {
            Ok(events) => {
                let now = fmt_time(blackbox_core::now());
                for e in events {
                    match e {
                        ServiceEvent::Changes(changes) => {
                            for c in &changes {
                                println!("{now}  {}", fmt_change(c));
                            }
                        }
                        ServiceEvent::Session(MonitorEvent::SessionStarted { game, capturing, .. }) => {
                            let name = &game.name;
                            let extra = if capturing { "" } else { t(" (ohne Frame-Messung)", " (without frame capture)") };
                            println!("{now}  {}", tr!("Session gestartet: {name}{extra}", "Session started: {name}{extra}"));
                        }
                        ServiceEvent::Session(MonitorEvent::SessionEnded { game, metrics, .. }) => {
                            let perf = metrics
                                .map(|m| {
                                    let (avg, low) = (format!("{:.0}", m.avg_fps), format!("{:.0}", m.p1_low_fps));
                                    tr!("Ø {avg} FPS, 1%-Low {low}", "avg. {avg} FPS, 1% low {low}")
                                })
                                .unwrap_or_else(|| t("ohne Messung", "not measured").into());
                            let name = &game.name;
                            println!("{now}  {}", tr!("Session beendet: {name} ({perf})", "Session ended: {name} ({perf})"));
                        }
                    }
                }
            }
            Err(e) => tracing::error!(error = %e, "service tick failed"),
        }

        // Sleep in short steps so Ctrl+C is handled promptly.
        let wake = Instant::now() + poll_every;
        while Instant::now() < wake && !stop.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    service.shutdown()?;
    println!("{}", t("Blackbox beendet.", "Blackbox stopped."));
    Ok(())
}

fn cmd_import(store: &Store, config: &Config, game: String, csv: PathBuf, exe: Option<String>) -> Result<()> {
    let name = launchers::installed_games()?
        .into_iter()
        .find(|g| g.id == game)
        .map(|g| g.name)
        .or_else(|| store.games_with_sessions().ok()?.into_iter().find(|(id, _)| *id == game).map(|(_, n)| n))
        .unwrap_or_else(|| game.clone());
    let path = csv.display();
    let file = std::fs::File::open(&csv).with_context(|| tr!("{path} nicht lesbar", "{path} cannot be read"))?;
    let filter: Vec<String> = exe.into_iter().collect();
    let ft = frames::read_presentmon_csv(file, &filter)?;
    let Some(metrics) = frames::metrics_from_frametimes(&ft, config.min_frames) else {
        let (have, need) = (ft.len(), config.min_frames);
        bail!(tr!("Zu wenige Frames ({have}) – mindestens {need} nötig.", "Too few frames ({have}) – at least {need} needed."));
    };
    // The file's modification time marks the end of the capture.
    let ended: Timestamp = std::fs::metadata(&csv)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as Timestamp)
        .unwrap_or_else(blackbox_core::now);
    let seconds = (metrics.frames as f64 / metrics.avg_fps) as Timestamp;
    let id = store.start_session(&game, &name, ended - seconds)?;
    store.finish_session(id, ended, Some(&metrics), Some(&i18n::note_imported()))?;
    let (avg, low) = (format!("{:.0}", metrics.avg_fps), format!("{:.0}", metrics.p1_low_fps));
    println!("{}", tr!("Session #{id} importiert: {name}, Ø {avg} FPS, 1%-Low {low}", "Session #{id} imported: {name}, avg. {avg} FPS, 1% low {low}"));
    Ok(())
}

fn reason_text(r: Reason) -> &'static str {
    match r {
        Reason::Baseline => t("Ausgangsstand", "initial version"),
        Reason::Changed => t("Änderung", "change"),
        Reason::BeforeRestore => t("vor Wiederherstellung gesichert", "backed up before a restore"),
        Reason::Restored => t("wiederhergestellt", "restored"),
    }
}

fn cmd_vault(store: &Store, config: &Config, paths: &Paths, cmd: VaultCmd) -> Result<()> {
    let mut vault = Vault::open(&paths.vault_dir)?;
    match cmd {
        VaultCmd::List => {
            let games = launchers::installed_games()?;
            let changes = vault::check_and_journal(
                &mut vault,
                store,
                &games,
                &KnownDirs::detect(),
                &config.vault_custom_paths,
                None,
                blackbox_core::now(),
            )?;
            for c in &changes {
                println!("{} {}", t("Neu erkannt:", "Newly detected:"), fmt_change(c));
            }
            let overview = vault.overview()?;
            if overview.is_empty() {
                println!(
                    "{}",
                    t(
                        "Keine Konfigurationsdateien gefunden. Eigene Pfade lassen sich in config.json unter `vault_custom_paths` eintragen.",
                        "No settings files found. Own paths can be added in config.json under `vault_custom_paths`."
                    )
                );
            }
            for g in overview {
                let flag = if g.reset_suspected { t("  ⚠ möglicher Reset", "  ⚠ possible reset") } else { "" };
                println!("\n{} ({}){flag}", g.game_name, g.game_id);
                for f in &g.files {
                    let missing = if f.exists { "" } else { t(" (fehlt)", " (missing)") };
                    let n = f.snapshots.len();
                    println!("  {:<48} {}{missing}", f.label, tr!("{n} Stand/Stände", "{n} version(s)"));
                }
            }
        }
        VaultCmd::History { game } => {
            let overview = vault.overview()?;
            let Some(g) = overview.into_iter().find(|g| g.game_id == game) else {
                bail!(tr!(
                    "Für `{game}` gibt es keine Sicherungen. `blackbox vault list` zeigt die überwachten Spiele.",
                    "There are no backups for `{game}`. `blackbox vault list` shows the tracked games."
                ));
            };
            println!("{}", g.game_name);
            for f in &g.files {
                println!("\n  {}\n  {}", f.label, f.path);
                for s in &f.snapshots {
                    let detail = match s.reason {
                        Reason::Changed => s.summary.describe(),
                        other => reason_text(other).to_string(),
                    };
                    let current = if s.current { t("  ← aktuell", "  ← current") } else { "" };
                    println!("    #{:<5} {}  {detail}{current}", s.id, fmt_time(s.taken_at));
                }
            }
        }
        VaultCmd::Diff { snapshot } => {
            let d = vault.diff_with_previous(snapshot)?;
            let file = &d.file_name;
            match d.previous_id {
                Some(prev) => println!("{}", tr!("{file}: Sicherung #{snapshot} gegenüber #{prev}", "{file}: backup #{snapshot} compared with #{prev}")),
                None => println!("{}", tr!("{file}: Ausgangsstand #{snapshot}", "{file}: initial version #{snapshot}")),
            }
            if d.binary {
                println!("{}", t("  Binärdatei – Unterschiede können nicht einzeln angezeigt werden.", "  Binary file – differences cannot be shown individually."));
            } else if d.changes.is_empty() {
                println!("{}", t("  Keine inhaltlichen Unterschiede.", "  No differences in content."));
            }
            for c in &d.changes {
                let class = match c.class {
                    vault::selective::KeyClass::Display => t("Anzeige", "Display"),
                    vault::selective::KeyClass::Graphics => t("Grafik ", "Graphics"),
                    vault::selective::KeyClass::Personal => t("Persönl", "Personal"),
                };
                match (&c.old, &c.new) {
                    (Some(o), Some(n)) => println!("  [{class}] {}: {o} → {n}", c.key),
                    (None, Some(n)) => println!("  [{class}] + {}: {n}", c.key),
                    (Some(o), None) => println!("  [{class}] - {}: {o}", c.key),
                    (None, None) => {}
                }
            }
        }
        VaultCmd::Restore { snapshot, personal, yes } => {
            let (game_id, game_name) = vault.snapshot_owner(snapshot)?;
            let games = launchers::installed_games()?;
            if monitor::running_game_ids(&games)?.contains(&game_id) {
                bail!(tr!(
                    "{game_name} läuft gerade. Bitte das Spiel zuerst beenden – es würde die Datei beim Beenden wieder überschreiben.",
                    "{game_name} is running. Please close the game first – it would overwrite the file again when it exits."
                ));
            }
            if !yes {
                let flag = if personal { " --personal" } else { "" };
                if personal {
                    println!("{}", tr!(
                        "Die persönlichen Einstellungen aus Sicherung #{snapshot} von {game_name} würden übernommen; der aktuelle Stand wird vorher gesichert.",
                        "The personal settings from backup #{snapshot} of {game_name} would be applied; the current version is backed up first."
                    ));
                } else {
                    println!("{}", tr!(
                        "Sicherung #{snapshot} von {game_name} würde zurückgeschrieben; der aktuelle Stand wird vorher gesichert.",
                        "Backup #{snapshot} of {game_name} would be written back; the current version is backed up first."
                    ));
                }
                println!("{}", tr!("Ausführen mit: blackbox vault restore {snapshot}{flag} --yes", "Run with: blackbox vault restore {snapshot}{flag} --yes"));
                return Ok(());
            }
            let out = vault::restore_and_journal(&mut vault, store, snapshot, personal, blackbox_core::now())?;
            let path = &out.path;
            match out.personal_keys {
                Some(n) => println!("{}", tr!("{n} persönliche Einstellung(en) übernommen: {path}", "{n} personal setting(s) applied: {path}")),
                None => println!("{}", tr!("Wiederhergestellt: {path}", "Restored: {path}")),
            }
            if out.backed_up {
                let id = &out.game_id;
                println!("{}", tr!(
                    "Der vorherige Stand wurde gesichert und lässt sich ebenso zurückholen (`blackbox vault history --game {id}`).",
                    "The previous version was backed up and can be restored the same way (`blackbox vault history --game {id}`)."
                ));
            }
        }
        VaultCmd::Export { file, game } => {
            let profile = vault.export_profile(&game, blackbox_core::now())?;
            if profile.games.is_empty() {
                bail!(t(
                    "Keine exportierbaren Einstellungen gefunden. `blackbox vault list` zeigt die überwachten Spiele.",
                    "No exportable settings found. `blackbox vault list` shows the tracked games."
                ));
            }
            let json = serde_json::to_string_pretty(&profile)?;
            let path = file.display();
            std::fs::write(&file, json).with_context(|| tr!("{path} kann nicht geschrieben werden", "{path} cannot be written"))?;
            let files: usize = profile.games.iter().map(|g| g.files.len()).sum();
            let games = profile.games.len();
            println!("{}", tr!("{games} Spiel(e), {files} Datei(en) exportiert nach {path}", "{games} game(s), {files} file(s) exported to {path}"));
        }
        VaultCmd::Import { file, yes } => {
            let path = file.display();
            let text = std::fs::read_to_string(&file).with_context(|| tr!("{path} nicht lesbar", "{path} cannot be read"))?;
            let profile: vault::Profile = serde_json::from_str(&text)
                .context(t("Die Datei ist kein gültiges Blackbox-Profil", "The file is not a valid Blackbox profile"))?;
            // Make sure the vault knows the local files and has their current state.
            let games = launchers::installed_games()?;
            vault::check_and_journal(&mut vault, store, &games, &KnownDirs::detect(), &config.vault_custom_paths, None, blackbox_core::now())?;
            let running = monitor::running_game_ids(&games)?;
            if let Some(g) = profile.games.iter().find(|g| running.contains(&g.game_id)) {
                let name = &g.game_name;
                bail!(tr!("{name} läuft gerade. Bitte das Spiel zuerst beenden.", "{name} is running. Please close the game first."));
            }
            let items = if yes {
                vault::import_and_journal(&mut vault, store, &profile, blackbox_core::now())?
            } else {
                vault.import_profile(&profile, false, blackbox_core::now())?
            };
            if items.is_empty() {
                println!("{}", t("Keine passenden Spiele oder Dateien auf diesem PC gefunden.", "No matching games or files found on this PC."));
            }
            for i in &items {
                let (game, label) = (&i.game_name, &i.label);
                if let Some(reason) = &i.skipped {
                    println!("{}", tr!("{game} – {label}: übersprungen ({reason})", "{game} – {label}: skipped ({reason})"));
                    continue;
                }
                let n = i.changed.len();
                let line = if i.applied {
                    tr!("{game} – {label}: {n} Einstellung(en) übernommen", "{game} – {label}: {n} setting(s) applied")
                } else if i.changed.is_empty() {
                    tr!("{game} – {label}: bereits gleich", "{game} – {label}: already the same")
                } else {
                    tr!("{game} – {label}: {n} Einstellung(en) würden übernommen", "{game} – {label}: {n} setting(s) would be applied")
                };
                println!("{line}");
                if !yes {
                    for k in &i.changed {
                        println!("    {k}");
                    }
                }
            }
            if !yes && items.iter().any(|i| !i.changed.is_empty()) {
                println!("\n{}", tr!(
                    "Anzeige- und Grafikeinstellungen bleiben unverändert. Ausführen mit: blackbox vault import {path} --yes",
                    "Display and graphics settings stay unchanged. Run with: blackbox vault import {path} --yes"
                ));
            }
        }
    }
    Ok(())
}

fn init_logging(paths: &Paths) -> tracing_appender::non_blocking::WorkerGuard {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::Layer;

    let file = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("blackbox")
        .filename_suffix("log")
        .max_log_files(7)
        .build(&paths.log_dir)
        .expect("log directory must be writable");
    let (writer, guard) = tracing_appender::non_blocking(file);
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(writer)
                .with_ansi(false)
                .with_filter(tracing_subscriber::filter::LevelFilter::INFO),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .without_time()
                .with_target(false)
                .with_filter(tracing_subscriber::filter::LevelFilter::WARN),
        )
        .init();
    guard
}

fn main() -> Result<()> {
    i18n::set_lang(detect_lang());
    let mut cmd = Cli::command();
    if i18n::lang() == Lang::De {
        cmd = german_help(cmd);
    }
    let cli = Cli::from_arg_matches(&cmd.get_matches()).unwrap_or_else(|e| e.exit());

    let paths = paths_for(cli.data_dir.as_ref())?;
    let log_dir = paths.log_dir.display();
    std::fs::create_dir_all(&paths.log_dir).with_context(|| tr!("{log_dir} kann nicht angelegt werden", "{log_dir} cannot be created"))?;
    let _log_guard = init_logging(&paths);

    let mut config = Config::load(&paths.config_file)?;
    let mut store = Store::open(&paths.database)?;

    match cli.command {
        Cmd::Scan => {
            let collectors = collectors::default_collectors();
            let now = blackbox_core::now();
            let mut report = collectors::run_scan(&mut store, &collectors, now)?;
            if config.vault_enabled {
                let mut v = Vault::open(&paths.vault_dir)?;
                let games = launchers::installed_games()?;
                let dirs = KnownDirs::detect();
                report.changes.extend(vault::check_and_journal(&mut v, &store, &games, &dirs, &config.vault_custom_paths, None, now)?);
            }
            print_scan(&report);
        }
        Cmd::State => {
            let mut last = None;
            for f in store.current_state()?.iter().map(i18n::display_fact) {
                if last != Some(f.category) {
                    println!("\n{}", f.category.label());
                    last = Some(f.category);
                }
                println!("  {}: {}", f.label, f.value);
            }
            if last.is_none() {
                println!("{}", t("Noch kein Scan vorhanden. Starte mit `blackbox scan`.", "No scan yet. Start with `blackbox scan`."));
            }
        }
        Cmd::Changes { days } => {
            let now = blackbox_core::now();
            let changes = store.changes_between(now - days.max(0) * 86_400, now)?;
            if changes.is_empty() {
                println!("{}", tr!("Keine Änderungen in den letzten {days} Tagen.", "No changes in the last {days} days."));
            }
            for c in changes {
                println!("{}  {}", fmt_time(c.detected_at), fmt_change(&c));
            }
        }
        Cmd::Games => {
            let games = launchers::installed_games()?;
            if games.is_empty() {
                println!("{}", t("Keine Spiele gefunden.", "No games found."));
            }
            for g in games {
                let version = i18n::fact_value(blackbox_core::model::Category::GameBuild, &g.build_id);
                println!("{:<40} {:<16} {} ({version})", g.id, launchers::launcher_name(&g.id), g.name);
            }
        }
        Cmd::Watch => cmd_watch(store, &config, &paths)?,
        Cmd::Sessions { game } => cmd_sessions(&store, game)?,
        Cmd::Analyze { game } => cmd_analyze(&store, game)?,
        Cmd::Import { game, csv, exe } => cmd_import(&store, &config, game, csv, exe)?,
        Cmd::Vault { command } => cmd_vault(&store, &config, &paths, command)?,
        Cmd::Exclude { session } | Cmd::Include { session } => {
            let exclude = matches!(cli.command, Cmd::Exclude { .. });
            if !store.set_excluded(session, exclude)? {
                bail!(tr!(
                    "Session #{session} existiert nicht. `blackbox sessions` zeigt alle Sessions.",
                    "Session #{session} does not exist. `blackbox sessions` shows all sessions."
                ));
            }
            if exclude {
                println!("{}", tr!("Session #{session} zählt nicht mehr für den Vergleich.", "Session #{session} is no longer used for comparison."));
            } else {
                println!("{}", tr!("Session #{session} zählt wieder für den Vergleich.", "Session #{session} is used for comparison again."));
            }
        }
        Cmd::Report { session, out } => {
            let text = report::session_report_text(&store, session, &AnalysisConfig::default())?;
            match out {
                Some(path) => {
                    let shown = path.display();
                    std::fs::write(&path, &text).with_context(|| tr!("{shown} kann nicht geschrieben werden", "{shown} cannot be written"))?;
                    println!("{}", tr!("Bericht gespeichert: {shown}", "Report saved: {shown}"));
                }
                None => print!("{text}"),
            }
        }
        Cmd::SetPresentmon { path } => {
            if !path.is_file() {
                let shown = path.display();
                bail!(tr!("{shown} ist keine Datei.", "{shown} is not a file."));
            }
            let path = path.canonicalize().unwrap_or(path);
            config.presentmon_path = Some(path.clone());
            config.save(&paths.config_file)?;
            let shown = path.display();
            println!("{}", tr!("PresentMon gesetzt: {shown}", "PresentMon set: {shown}"));
        }
        Cmd::SetLanguage { language } => {
            let language = language.trim().to_ascii_lowercase();
            if !["auto", "de", "en"].contains(&language.as_str()) {
                bail!(tr!("Unbekannte Sprache `{language}` – erlaubt sind auto, de und en.", "Unknown language `{language}` – use auto, de or en."));
            }
            config.language = language;
            config.save(&paths.config_file)?;
            i18n::set_lang(config.lang());
            let code = config.lang().code();
            println!("{}", tr!("Sprache: {code}", "Language: {code}"));
        }
        Cmd::Info => {
            let row = |label: &str, value: String| println!("{label:<15}{value}");
            row(t("Konfiguration:", "Configuration:"), paths.config_file.display().to_string());
            row(t("Datenbank:", "Database:"), paths.database.display().to_string());
            row(t("Protokolle:", "Logs:"), paths.log_dir.display().to_string());
            let disabled = if config.vault_enabled { "" } else { t(" (deaktiviert)", " (disabled)") };
            row("Loadout Vault:", format!("{}{disabled}", paths.vault_dir.display()));
            let pm = match config.resolve_presentmon() {
                Some(PresentMonSource::Configured(p)) => format!("{} {}", p.display(), t("(eigener Pfad)", "(own path)")),
                Some(PresentMonSource::Bundled(p)) => format!("{} {}", p.display(), t("(mitgeliefert)", "(bundled)")),
                None => t("nicht gefunden", "not found").to_string(),
            };
            row("PresentMon:", pm);
            row(t("Sprache:", "Language:"), format!("{} ({})", i18n::lang().code(), config.language));
            let (scan, poll) = (config.scan_interval_secs, config.poll_interval_secs);
            println!("{}", tr!("Scan-Intervall: {scan} s, Spielerkennung alle {poll} s", "Scan interval: {scan} s, game detection every {poll} s"));
        }
    }
    Ok(())
}
