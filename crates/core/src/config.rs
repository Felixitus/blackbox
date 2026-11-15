//! User configuration and data locations.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Full path to `PresentMon.exe`. Without it sessions are recorded
    /// without frame data.
    pub presentmon_path: Option<PathBuf>,
    /// Seconds between two system scans while watching.
    pub scan_interval_secs: u64,
    /// Seconds between two checks for running games.
    pub poll_interval_secs: u64,
    /// Sessions with fewer captured frames get no metrics.
    pub min_frames: usize,
    /// Library entries that are not games and must not open sessions
    /// (they often run permanently in the background).
    pub ignored_game_ids: Vec<String>,
    /// Loadout Vault: snapshot game config files.
    pub vault_enabled: bool,
    /// Extra config files or folders per game id, e.g.
    /// `{ "steam:1086940": ["C:\\Users\\me\\AppData\\Local\\Larian Studios\\Baldur's Gate 3\\PlayerProfiles"] }`.
    pub vault_custom_paths: BTreeMap<String, Vec<PathBuf>>,
    /// Windows notifications for regressions and possible settings resets.
    pub notifications: bool,
    /// `auto` (Windows display language), `de` or `en`.
    pub language: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            presentmon_path: None,
            scan_interval_secs: 900,
            poll_interval_secs: 5,
            min_frames: 3000,
            ignored_game_ids: vec![
                "steam:431960".into(), // Wallpaper Engine
                "steam:629520".into(), // Soundpad
            ],
            vault_enabled: true,
            vault_custom_paths: BTreeMap::new(),
            notifications: true,
            language: "auto".into(),
        }
    }
}

/// File name of the PresentMon copy the installer places next to Blackbox.
pub const BUNDLED_PRESENTMON: &str = "PresentMon.exe";

/// Where PresentMon comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PresentMonSource {
    /// Path set by the user (takes precedence).
    Configured(PathBuf),
    /// Copy shipped with the installer.
    Bundled(PathBuf),
}

impl PresentMonSource {
    pub fn path(&self) -> &Path {
        match self {
            PresentMonSource::Configured(p) | PresentMonSource::Bundled(p) => p,
        }
    }
}

/// The PresentMon shipped with the installer: next to the app, or one level
/// up for the command line tool in the `cli` subfolder.
pub fn bundled_presentmon() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    bundled_presentmon_near(exe.parent()?)
}

fn bundled_presentmon_near(dir: &Path) -> Option<PathBuf> {
    [Some(dir), dir.parent()]
        .into_iter()
        .flatten()
        .map(|d| d.join(BUNDLED_PRESENTMON))
        .find(|p| p.is_file())
}

impl Config {
    /// The language to use for this configuration.
    pub fn lang(&self) -> crate::i18n::Lang {
        crate::i18n::resolve_setting(&self.language)
    }

    /// PresentMon to use: the configured file if it exists, otherwise the
    /// bundled copy.
    pub fn resolve_presentmon(&self) -> Option<PresentMonSource> {
        match &self.presentmon_path {
            Some(p) if p.is_file() => Some(PresentMonSource::Configured(p.clone())),
            _ => bundled_presentmon().map(PresentMonSource::Bundled),
        }
    }

    /// Load the configuration; a missing file yields the defaults.
    pub fn load(path: &Path) -> Result<Config> {
        match fs::read_to_string(path) {
            Ok(text) => {
                let cfg: Config = serde_json::from_str(text.trim_start_matches('\u{feff}'))
                    .map_err(|e| Error::Config(format!("{}: {e}", path.display())))?;
                cfg.validate()?;
                Ok(cfg)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| Error::Config(e.to_string()))?;
        fs::write(path, text)?;
        Ok(())
    }

    fn validate(&self) -> Result<()> {
        if self.poll_interval_secs == 0 || self.scan_interval_secs == 0 {
            return Err(Error::Config("intervals must be greater than zero".into()));
        }
        Ok(())
    }
}

/// Where Blackbox keeps its files.
#[derive(Debug, Clone)]
pub struct Paths {
    pub config_file: PathBuf,
    pub database: PathBuf,
    pub log_dir: PathBuf,
    pub capture_dir: PathBuf,
    pub vault_dir: PathBuf,
}

impl Paths {
    /// Standard per-user locations (`%APPDATA%\Blackbox`, `%LOCALAPPDATA%\Blackbox`).
    pub fn default_locations() -> Result<Paths> {
        let dirs = directories::ProjectDirs::from("", "", "Blackbox")
            .ok_or_else(|| Error::Config("no user profile directory found".into()))?;
        Ok(Paths::in_dirs(dirs.config_dir(), dirs.data_local_dir()))
    }

    pub fn in_dirs(config_dir: &Path, data_dir: &Path) -> Paths {
        Paths {
            config_file: config_dir.join("config.json"),
            database: data_dir.join("blackbox.db"),
            log_dir: data_dir.join("logs"),
            capture_dir: data_dir.join("captures"),
            vault_dir: data_dir.join("vault"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_gives_defaults_and_roundtrip_works() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/config.json");
        assert_eq!(Config::load(&path).unwrap(), Config::default());

        let cfg = Config {
            presentmon_path: Some(PathBuf::from(r"C:\Tools\PresentMon.exe")),
            ..Config::default()
        };
        cfg.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), cfg);
    }

    #[test]
    fn bundled_presentmon_is_found_next_to_or_above_the_executable() {
        let dir = tempfile::tempdir().unwrap();
        let cli = dir.path().join("cli");
        fs::create_dir_all(&cli).unwrap();
        assert_eq!(bundled_presentmon_near(&cli), None);
        fs::write(dir.path().join(BUNDLED_PRESENTMON), b"x").unwrap();
        assert_eq!(bundled_presentmon_near(&cli), Some(dir.path().join(BUNDLED_PRESENTMON)));
        assert_eq!(bundled_presentmon_near(dir.path()), Some(dir.path().join(BUNDLED_PRESENTMON)));
    }

    #[test]
    fn configured_presentmon_wins_if_it_exists() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("PresentMon-custom.exe");
        fs::write(&exe, b"x").unwrap();
        let cfg = Config { presentmon_path: Some(exe.clone()), ..Config::default() };
        assert_eq!(cfg.resolve_presentmon(), Some(PresentMonSource::Configured(exe)));
        // A missing configured file falls back to the bundled copy (none in tests).
        let gone = Config { presentmon_path: Some(dir.path().join("nope.exe")), ..Config::default() };
        assert_eq!(gone.resolve_presentmon().map(|s| matches!(s, PresentMonSource::Bundled(_))), bundled_presentmon().map(|_| true));
    }

    #[test]
    fn partial_and_invalid_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(&path, r#"{ "min_frames": 10 }"#).unwrap();
        let cfg = Config::load(&path).unwrap();
        assert_eq!(cfg.min_frames, 10);
        assert_eq!(cfg.poll_interval_secs, 5);

        fs::write(&path, r#"{ "poll_interval_secs": 0 }"#).unwrap();
        assert!(Config::load(&path).is_err());
        fs::write(&path, "not json").unwrap();
        assert!(Config::load(&path).is_err());
    }
}
