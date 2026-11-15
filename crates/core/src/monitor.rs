//! Detects running games and records one session per play-through.
//!
//! Detection is a plain process list poll: a game runs while any process
//! lives inside its install directory. Nothing is injected into the game,
//! which keeps Blackbox compatible with anti-cheat software.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use sysinfo::{CpuRefreshKind, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

use crate::config::Config;
use crate::error::{Error, Result};
use crate::frames;
use crate::i18n;
use crate::load::LoadTracker;
use crate::model::{Game, SessionMetrics, Timestamp};
use crate::store::Store;

/// Polls without a game process before a session is considered over. Covers
/// launchers that exit before the actual game executable starts.
const GRACE_POLLS: u32 = 3;

const ETW_SESSION_NAME: &str = "BlackboxCapture";

/// The game whose install directory contains `exe`, preferring the deepest
/// match when install directories are nested.
pub fn match_game<'a>(exe: &Path, games: &'a [Game]) -> Option<&'a Game> {
    let exe = exe.to_string_lossy().to_lowercase().replace('/', "\\");
    games
        .iter()
        .filter(|g| {
            let dir = g.install_dir.to_lowercase().replace('/', "\\");
            let dir = dir.trim_end_matches('\\');
            !dir.is_empty()
                && exe.starts_with(dir)
                && exe[dir.len()..].starts_with('\\')
        })
        .max_by_key(|g| g.install_dir.len())
}

/// Ids of the given games that have a process running right now.
pub fn running_game_ids(games: &[Game]) -> Result<Vec<String>> {
    let mut sys = System::new().map_err(|e| Error::Other(format!("process list unavailable: {e}")))?;
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_exe(UpdateKind::OnlyIfNotSet),
    );
    let mut ids: Vec<String> = sys
        .processes()
        .values()
        .filter_map(|p| p.exe())
        .filter_map(|exe| match_game(exe, games))
        .map(|g| g.id.clone())
        .collect();
    ids.sort();
    ids.dedup();
    Ok(ids)
}

/// A running PresentMon capture.
struct Capture {
    child: Child,
    presentmon: PathBuf,
    csv: PathBuf,
}

fn presentmon_command(presentmon: &Path) -> Command {
    let mut cmd = Command::new(presentmon);
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

impl Capture {
    fn start(presentmon: &Path, capture_dir: &Path, session_id: i64) -> Result<Capture> {
        if !presentmon.is_file() {
            let path = presentmon.display();
            return Err(Error::Capture(crate::tr!("PresentMon nicht gefunden: {path}", "PresentMon not found: {path}")));
        }
        std::fs::create_dir_all(capture_dir)?;
        let csv = capture_dir.join(format!("session-{session_id}.csv"));
        let child = presentmon_command(presentmon)
            .arg("--output_file")
            .arg(&csv)
            .args(["--session_name", ETW_SESSION_NAME])
            .arg("--stop_existing_session")
            .arg("--no_console_stats")
            // Blackbox only needs the time between presents. Not tracking GPU,
            // input and display metrics lowers PresentMon's own overhead.
            .args(["--no_track_gpu", "--no_track_input", "--no_track_display"])
            .spawn()
            .map_err(|e| Error::Capture(crate::tr!("PresentMon konnte nicht gestartet werden: {e}", "PresentMon could not be started: {e}")))?;
        Ok(Capture { child, presentmon: presentmon.to_path_buf(), csv })
    }

    /// Whether PresentMon is still running (it exits immediately without
    /// the rights to start an ETW session).
    fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Stop the capture and return the CSV path. PresentMon is asked to end
    /// its ETW session so it flushes the file; it is killed only if it does
    /// not exit by itself.
    fn stop(mut self) -> PathBuf {
        let csv = self.csv.clone();
        let _ = presentmon_command(&self.presentmon)
            .args(["--session_name", ETW_SESSION_NAME])
            .arg("--terminate_existing_session")
            .status();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(100));
                }
                _ => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    break;
                }
            }
        }
        csv
    }
}

impl Drop for Capture {
    /// A capture that is dropped without `stop` (e.g. after a panic) must not
    /// leave PresentMon running. A leftover ETW session is cleaned up by
    /// `--stop_existing_session` on the next start.
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

struct Active {
    session_id: i64,
    game: Game,
    exe_names: BTreeSet<String>,
    capture: Option<Capture>,
    capture_note: Option<String>,
    missing_polls: u32,
    load: LoadTracker,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MonitorEvent {
    SessionStarted { session_id: i64, game: Game, capturing: bool },
    SessionEnded { session_id: i64, game: Game, metrics: Option<SessionMetrics> },
}

pub struct Monitor {
    sys: System,
    games: Vec<Game>,
    active: Option<Active>,
    presentmon: Option<PathBuf>,
    capture_dir: PathBuf,
    min_frames: usize,
    ignored: Vec<String>,
}

impl Monitor {
    pub fn new(config: &Config, capture_dir: PathBuf, games: Vec<Game>) -> Result<Monitor> {
        let presentmon = config.resolve_presentmon().map(|s| s.path().to_path_buf());
        Self::with_presentmon(config, capture_dir, games, presentmon)
    }

    /// Like `new`, with an explicit PresentMon (`None` = no frame capture).
    pub fn with_presentmon(
        config: &Config,
        capture_dir: PathBuf,
        games: Vec<Game>,
        presentmon: Option<PathBuf>,
    ) -> Result<Monitor> {
        let mut sys = System::new()
            .map_err(|e| Error::Other(format!("process list unavailable: {e}")))?;
        // sysinfo scales process CPU usage by the number of known CPUs; an
        // empty CPU list would make every process read as 0 %.
        sys.refresh_cpu_list(CpuRefreshKind::nothing());
        let mut monitor = Monitor {
            sys,
            games: Vec::new(),
            active: None,
            presentmon,
            capture_dir,
            min_frames: config.min_frames,
            ignored: config.ignored_game_ids.clone(),
        };
        monitor.set_games(games);
        Ok(monitor)
    }

    /// Replace the list of known games (after a scan found new installs).
    pub fn set_games(&mut self, mut games: Vec<Game>) {
        games.retain(|g| !self.ignored.contains(&g.id));
        self.games = games;
    }

    pub fn active_game(&self) -> Option<&Game> {
        self.active.as_ref().map(|a| &a.game)
    }

    /// Executables of known games that are running right now.
    fn running_game_exes(&mut self) -> Vec<(Game, String)> {
        // CPU usage is computed between two refreshes, so it is refreshed on
        // every poll; sampling it costs next to nothing.
        self.sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing().with_exe(UpdateKind::OnlyIfNotSet).with_cpu(),
        );
        let mut found = Vec::new();
        for process in self.sys.processes().values() {
            let Some(exe) = process.exe() else { continue };
            if let Some(game) = match_game(exe, &self.games) {
                if let Some(name) = exe.file_name() {
                    found.push((game.clone(), name.to_string_lossy().into_owned()));
                }
            }
        }
        found
    }

    /// Check the process list once and update session state.
    pub fn poll(&mut self, store: &Store, now: Timestamp) -> Result<Vec<MonitorEvent>> {
        let running = self.running_game_exes();
        let mut events = Vec::new();

        if let Some(active) = self.active.as_mut() {
            // sysinfo reports percent of one core; convert to percent of the whole CPU.
            let cpus = self.sys.cpus().len().max(1) as f64;
            let excluded: Vec<String> = active.exe_names.iter().cloned().collect();
            let names_cpu: Vec<(String, f64)> = self
                .sys
                .processes()
                .values()
                .map(|p| (p.name().to_string_lossy().into_owned(), f64::from(p.cpu_usage()) / cpus))
                .collect();
            active.load.add_sample(names_cpu.iter().map(|(n, c)| (n.as_str(), *c)), &excluded);
            let names: Vec<&String> = running
                .iter()
                .filter(|(g, _)| g.id == active.game.id)
                .map(|(_, n)| n)
                .collect();
            if names.is_empty() {
                active.missing_polls += 1;
            } else {
                active.missing_polls = 0;
                active.exe_names.extend(names.into_iter().cloned());
            }
            if let Some(capture) = active.capture.as_mut() {
                if !capture.is_alive() {
                    tracing::warn!("PresentMon exited early (administrator rights missing?)");
                    active.capture = None;
                    active.capture_note = Some(i18n::note_presentmon_exited());
                }
            }
            if active.missing_polls >= GRACE_POLLS {
                events.push(self.end_session(store, now)?);
            }
        }

        if self.active.is_none() {
            if let Some((game, exe)) = running.into_iter().next() {
                events.push(self.begin_session(store, game, exe, now)?);
            }
        }
        Ok(events)
    }

    fn begin_session(
        &mut self,
        store: &Store,
        game: Game,
        exe: String,
        now: Timestamp,
    ) -> Result<MonitorEvent> {
        let session_id = store.start_session(&game.id, &game.name, now)?;
        let (capture, capture_note) = match &self.presentmon {
            None => (None, Some(i18n::note_no_presentmon())),
            Some(path) if !path.is_file() => (None, Some(i18n::note_no_presentmon())),
            Some(path) => match Capture::start(path, &self.capture_dir, session_id) {
                Ok(c) => (Some(c), None),
                Err(e) => {
                    tracing::warn!(error = %e, "frame capture unavailable");
                    let msg = match &e {
                        Error::Capture(m) => m.clone(),
                        other => other.to_string(),
                    };
                    (None, Some(i18n::note_capture_failed(&msg)))
                }
            },
        };
        tracing::info!(game = %game.name, session_id, capturing = capture.is_some(), "session started");
        let event = MonitorEvent::SessionStarted {
            session_id,
            game: game.clone(),
            capturing: capture.is_some(),
        };
        self.active = Some(Active {
            session_id,
            game,
            exe_names: BTreeSet::from([exe]),
            capture,
            capture_note,
            missing_polls: 0,
            load: LoadTracker::default(),
        });
        Ok(event)
    }

    fn end_session(&mut self, store: &Store, now: Timestamp) -> Result<MonitorEvent> {
        let active = self.active.take().expect("end_session without active session");
        let mut note = active.capture_note;
        let mut metrics = None;

        if let Some(capture) = active.capture {
            let csv = capture.stop();
            let exe_names: Vec<String> = active.exe_names.iter().cloned().collect();
            match std::fs::File::open(&csv)
                .map_err(Error::from)
                .and_then(|f| frames::read_presentmon_csv(f, &exe_names))
            {
                Ok(ft) => {
                    metrics = frames::metrics_from_frametimes(&ft, self.min_frames);
                    if metrics.is_none() {
                        note = Some(i18n::note_too_few_frames(ft.len(), self.min_frames));
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "capture file unusable");
                    note = Some(i18n::note_unreadable(&e.to_string()));
                }
            }
            // Raw captures can be hundreds of MB; only the metrics are kept.
            let _ = std::fs::remove_file(&csv);
        }

        store.finish_session(active.session_id, now, metrics.as_ref(), note.as_deref())?;
        // Two samples are the minimum for a CPU share; shorter sessions say nothing.
        if active.load.samples() >= 2 {
            store.save_session_load(active.session_id, active.load.samples(), &active.load.finish())?;
        }
        tracing::info!(game = %active.game.name, session_id = active.session_id, has_metrics = metrics.is_some(), "session ended");
        Ok(MonitorEvent::SessionEnded {
            session_id: active.session_id,
            game: active.game,
            metrics,
        })
    }

    /// End a running session immediately (application shutdown).
    pub fn shutdown(&mut self, store: &Store, now: Timestamp) -> Result<Option<MonitorEvent>> {
        if self.active.is_some() {
            return self.end_session(store, now).map(Some);
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game(id: &str, dir: &str) -> Game {
        Game {
            id: id.into(),
            name: id.into(),
            install_dir: dir.into(),
            build_id: "1".into(),
        }
    }

    #[test]
    fn matches_by_install_dir_case_insensitively() {
        let games = vec![
            game("a", r"D:\Steam\steamapps\common\Valheim"),
            game("b", r"D:\Steam\steamapps\common\Valheim Plus"),
        ];
        let hit = |p: &str| match_game(Path::new(p), &games).map(|g| g.id.as_str());
        assert_eq!(hit(r"d:\steam\steamapps\common\valheim\valheim.exe"), Some("a"));
        assert_eq!(hit(r"D:\Steam\steamapps\common\Valheim Plus\bin\game.exe"), Some("b"));
        assert_eq!(hit(r"D:/Steam/steamapps/common/Valheim/sub/x.exe"), Some("a"));
        // A sibling directory sharing the prefix must not match.
        assert_eq!(hit(r"D:\Steam\steamapps\common\ValheimTools\x.exe"), None);
        assert_eq!(hit(r"C:\Windows\explorer.exe"), None);
    }

    #[test]
    fn nested_install_dirs_prefer_the_deepest() {
        let games = vec![game("outer", r"D:\Games"), game("inner", r"D:\Games\Inner")];
        let g = match_game(Path::new(r"D:\Games\Inner\run.exe"), &games).unwrap();
        assert_eq!(g.id, "inner");
    }

    #[test]
    fn empty_install_dir_never_matches() {
        let games = vec![game("x", "")];
        assert!(match_game(Path::new(r"C:\a.exe"), &games).is_none());
    }

    /// Uses the test process itself as the "game": its executable lives in
    /// the directory we declare as install dir.
    #[test]
    fn detects_running_process_and_closes_session_after_grace() {
        let exe = std::env::current_exe().unwrap();
        let dir = exe.parent().unwrap().to_string_lossy().into_owned();
        let me = game("test:self", &dir);
        let ignored = game("test:ignored", &dir);
        let store = Store::open_in_memory().unwrap();
        let cfg = Config {
            ignored_game_ids: vec!["test:ignored".into()],
            ..Config::default()
        };
        let tmp = tempfile::tempdir().unwrap();

        // An ignored entry alone never opens a session. No PresentMon here,
        // whatever happens to lie next to the test binary.
        let mut m = Monitor::with_presentmon(&cfg, tmp.path().to_path_buf(), vec![ignored], None).unwrap();
        assert!(m.poll(&store, 100).unwrap().is_empty());

        m.set_games(vec![me.clone()]);
        let events = m.poll(&store, 100).unwrap();
        assert!(matches!(
            &events[..],
            [MonitorEvent::SessionStarted { game, capturing: false, .. }] if game.id == "test:self"
        ));
        assert!(m.poll(&store, 105).unwrap().is_empty()); // still running
        assert_eq!(m.active_game().map(|g| g.id.as_str()), Some("test:self"));

        // The "game" disappears: the session survives the grace period only.
        m.set_games(Vec::new());
        assert!(m.poll(&store, 110).unwrap().is_empty());
        assert!(m.poll(&store, 115).unwrap().is_empty());
        let events = m.poll(&store, 120).unwrap();
        assert!(matches!(&events[..], [MonitorEvent::SessionEnded { metrics: None, .. }]));
        assert!(m.active_game().is_none());

        let sessions = store.sessions(Some("test:self")).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].duration_secs(), Some(20));
        assert_eq!(sessions[0].capture_note.as_deref(), Some("note:no_presentmon"));
        assert_eq!(i18n::with_lang(i18n::Lang::De, || i18n::capture_note("note:no_presentmon")), "Kein PresentMon gefunden");
    }

    #[test]
    fn missing_presentmon_is_reported_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let err = Capture::start(&dir.path().join("nope.exe"), dir.path(), 1).err().unwrap();
        assert!(err.to_string().contains("PresentMon"));
    }
}
