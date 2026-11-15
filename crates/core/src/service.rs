//! The background loop shared by the CLI (`blackbox watch`) and the desktop
//! app: periodic scans, config snapshots and session monitoring, driven by
//! `tick()`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::collectors::{self, Collector};
use crate::config::{Config, Paths};
use crate::error::Result;
use crate::model::{ChangeEvent, Game, Timestamp};
use crate::monitor::{Monitor, MonitorEvent};
use crate::launchers;
use crate::store::Store;
use crate::vault::{self, KnownDirs, Vault};

/// Games rewrite (or reset) their config at startup; checking the running
/// game's files once after this delay catches that during the session.
const IN_SESSION_VAULT_DELAY: Duration = Duration::from_secs(90);

#[derive(Debug, Clone, PartialEq)]
pub enum ServiceEvent {
    /// A scan or a config check found differences.
    Changes(Vec<ChangeEvent>),
    Session(MonitorEvent),
}

pub struct Service {
    store: Store,
    collectors: Vec<Box<dyn Collector>>,
    monitor: Monitor,
    games: Vec<Game>,
    vault: Option<Vault>,
    dirs: KnownDirs,
    custom_paths: BTreeMap<String, Vec<PathBuf>>,
    scan_every: Duration,
    last_scan: Option<Instant>,
    last_scan_at: Option<Timestamp>,
    /// `(due, game id)` of the pending in-session config check.
    pending_vault_check: Option<(Instant, String)>,
}

impl Service {
    pub fn new(store: Store, config: &Config, paths: &Paths) -> Result<Service> {
        let closed = store.close_dangling_sessions()?;
        if closed > 0 {
            tracing::warn!(closed, "closed sessions left open by a previous run");
        }
        let games = launchers::installed_games()?;
        let monitor = Monitor::new(config, paths.capture_dir.clone(), games.clone())?;
        let vault = if config.vault_enabled {
            match Vault::open(&paths.vault_dir) {
                Ok(v) => Some(v),
                Err(e) => {
                    // Blackbox stays useful without the vault.
                    tracing::error!(error = %e, "Loadout Vault unavailable");
                    None
                }
            }
        } else {
            None
        };
        Ok(Service {
            store,
            collectors: collectors::default_collectors(),
            monitor,
            games,
            vault,
            dirs: KnownDirs::detect(),
            custom_paths: config.vault_custom_paths.clone(),
            scan_every: Duration::from_secs(config.scan_interval_secs),
            last_scan: None,
            last_scan_at: None,
            pending_vault_check: None,
        })
    }

    pub fn active_game(&self) -> Option<&Game> {
        self.monitor.active_game()
    }

    /// Wall-clock time of the most recent completed scan.
    pub fn last_scan_at(&self) -> Option<Timestamp> {
        self.last_scan_at
    }

    /// Make the next `tick` scan regardless of the interval.
    pub fn request_scan(&mut self) {
        self.last_scan = None;
    }

    fn vault_check(&mut self, only: Option<&str>) -> Vec<ChangeEvent> {
        let Some(vault) = self.vault.as_mut() else { return Vec::new() };
        match vault::check_and_journal(vault, &self.store, &self.games, &self.dirs, &self.custom_paths, only, crate::now()) {
            Ok(changes) => changes,
            Err(e) => {
                tracing::error!(error = %e, "config check failed");
                Vec::new()
            }
        }
    }

    /// One iteration: scan if due, then check for running games.
    /// Call this every `Config::poll_interval_secs`.
    pub fn tick(&mut self) -> Result<Vec<ServiceEvent>> {
        let mut events = Vec::new();

        // No full scans while a game runs: nothing relevant changes
        // mid-session and the game should not compete for disk and CPU.
        let due = self.last_scan.is_none_or(|t| t.elapsed() >= self.scan_every);
        if due && self.monitor.active_game().is_none() {
            let report = collectors::run_scan(&mut self.store, &self.collectors, crate::now())?;
            let mut changes = report.changes;
            match launchers::installed_games() {
                Ok(games) => {
                    self.monitor.set_games(games.clone());
                    self.games = games;
                }
                Err(e) => tracing::warn!(error = %e, "game list not refreshed"),
            }
            changes.extend(self.vault_check(None));
            if !changes.is_empty() {
                events.push(ServiceEvent::Changes(changes));
            }
            self.last_scan = Some(Instant::now());
            self.last_scan_at = Some(crate::now());
        }

        if let Some((due_at, game_id)) = self.pending_vault_check.clone() {
            if Instant::now() >= due_at {
                self.pending_vault_check = None;
                let changes = self.vault_check(Some(&game_id));
                if !changes.is_empty() {
                    events.push(ServiceEvent::Changes(changes));
                }
            }
        }

        for e in self.monitor.poll(&self.store, crate::now())? {
            match &e {
                MonitorEvent::SessionStarted { game, .. } => {
                    self.pending_vault_check = Some((Instant::now() + IN_SESSION_VAULT_DELAY, game.id.clone()));
                }
                MonitorEvent::SessionEnded { .. } => {
                    // Scan right away so changes made while playing are dated
                    // as close to the session as possible.
                    self.pending_vault_check = None;
                    self.last_scan = None;
                }
            }
            events.push(ServiceEvent::Session(e));
        }
        Ok(events)
    }

    /// Close a running session (application shutdown).
    pub fn shutdown(&mut self) -> Result<()> {
        self.monitor.shutdown(&self.store, crate::now())?;
        Ok(())
    }
}
