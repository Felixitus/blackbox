//! Domain types shared by all Blackbox components.

use serde::{Deserialize, Serialize};

/// Unix timestamp in seconds (UTC).
pub type Timestamp = i64;

/// What kind of system property a fact or change belongs to.
///
/// The category decides how strongly a change is suspected when a
/// performance regression is analysed (see `analysis`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    OsBuild,
    WindowsUpdate,
    GpuDriver,
    AudioDriver,
    Bios,
    PowerPlan,
    SystemSetting,
    Autostart,
    GameBuild,
    /// Reserved for the Loadout Vault module (game config files).
    GameConfig,
    /// A program that loaded the CPU during a session. Never journaled;
    /// only produced by the analysis.
    BackgroundLoad,
}

impl Category {
    pub const ALL: [Category; 11] = [
        Category::OsBuild,
        Category::WindowsUpdate,
        Category::GpuDriver,
        Category::AudioDriver,
        Category::Bios,
        Category::PowerPlan,
        Category::SystemSetting,
        Category::Autostart,
        Category::GameBuild,
        Category::GameConfig,
        Category::BackgroundLoad,
    ];

    /// Stable identifier used in the database.
    pub fn as_str(self) -> &'static str {
        match self {
            Category::OsBuild => "os_build",
            Category::WindowsUpdate => "windows_update",
            Category::GpuDriver => "gpu_driver",
            Category::AudioDriver => "audio_driver",
            Category::Bios => "bios",
            Category::PowerPlan => "power_plan",
            Category::SystemSetting => "system_setting",
            Category::Autostart => "autostart",
            Category::GameBuild => "game_build",
            Category::GameConfig => "game_config",
            Category::BackgroundLoad => "background_load",
        }
    }

    pub fn parse(s: &str) -> Option<Category> {
        Category::ALL.into_iter().find(|c| c.as_str() == s)
    }

    /// Human readable name in the current language.
    pub fn label(self) -> &'static str {
        use crate::i18n::t;
        match self {
            Category::OsBuild => t("Windows-Build", "Windows build"),
            Category::WindowsUpdate => t("Windows-Update", "Windows update"),
            Category::GpuDriver => t("Grafiktreiber", "Graphics driver"),
            Category::AudioDriver => t("Audiotreiber", "Audio driver"),
            Category::Bios => t("BIOS/UEFI", "BIOS/UEFI"),
            Category::PowerPlan => t("Energieplan", "Power plan"),
            Category::SystemSetting => t("Systemeinstellung", "System setting"),
            Category::Autostart => t("Autostart", "Startup app"),
            Category::GameBuild => t("Spiel-Version", "Game version"),
            Category::GameConfig => t("Spieleinstellungen", "Game settings"),
            Category::BackgroundLoad => t("Hintergrundlast", "Background load"),
        }
    }
}

/// One observed property of the system at scan time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fact {
    pub category: Category,
    /// Stable identifier inside the category (e.g. a KB number or Steam app id).
    pub key: String,
    /// Human readable name of the thing the key refers to.
    pub label: String,
    pub value: String,
}

impl Fact {
    pub fn new(
        category: Category,
        key: impl Into<String>,
        label: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        Fact {
            category,
            key: key.into(),
            label: label.into(),
            value: value.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Removed,
    Modified,
}

impl ChangeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ChangeKind::Added => "added",
            ChangeKind::Removed => "removed",
            ChangeKind::Modified => "modified",
        }
    }

    pub fn parse(s: &str) -> Option<ChangeKind> {
        match s {
            "added" => Some(ChangeKind::Added),
            "removed" => Some(ChangeKind::Removed),
            "modified" => Some(ChangeKind::Modified),
            _ => None,
        }
    }
}

/// A difference between two consecutive scans.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeEvent {
    pub id: i64,
    /// Time of the scan that detected the change. The real change happened
    /// somewhere between the previous scan and this one.
    pub detected_at: Timestamp,
    pub category: Category,
    pub key: String,
    pub label: String,
    pub kind: ChangeKind,
    pub old_value: Option<String>,
    pub new_value: Option<String>,
}

/// An installed game Blackbox can recognise by its install directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Game {
    /// Namespaced id, e.g. `steam:730`.
    pub id: String,
    pub name: String,
    pub install_dir: String,
    pub build_id: String,
}

/// Frame statistics of one play session.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SessionMetrics {
    pub frames: u64,
    pub avg_fps: f64,
    /// FPS equivalent of the 99th percentile frame time ("1 % low").
    pub p1_low_fps: f64,
    /// FPS equivalent of the 99.9th percentile frame time ("0.1 % low").
    pub p01_low_fps: f64,
    pub median_frametime_ms: f64,
    /// Frames that took more than 2.5x the median frame time, per minute.
    pub stutters_per_min: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub id: i64,
    pub game_id: String,
    pub game_name: String,
    pub started_at: Timestamp,
    pub ended_at: Option<Timestamp>,
    pub metrics: Option<SessionMetrics>,
    /// Free text note why no metrics exist (e.g. capture tool missing).
    pub capture_note: Option<String>,
    /// Marked by the user as not representative; ignored by the analysis.
    #[serde(default)]
    pub excluded: bool,
    /// Number of background-load samples (`None` = not recorded).
    #[serde(default)]
    pub load_samples: Option<u32>,
}

impl Session {
    pub fn duration_secs(&self) -> Option<i64> {
        self.ended_at.map(|e| (e - self.started_at).max(0))
    }
}
