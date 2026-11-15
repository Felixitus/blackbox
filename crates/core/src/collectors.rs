//! Collectors read one aspect of the system and report it as facts.
//!
//! All collectors are strictly read-only and need no administrator rights.
//! A failing collector is skipped for that scan, so a temporary read error
//! never shows up as "everything was removed".

use crate::error::Result;
use crate::model::{Category, Fact, Timestamp};
use crate::launchers;
use crate::store::Store;
use crate::model::ChangeEvent;

pub trait Collector {
    fn name(&self) -> &'static str;
    fn category(&self) -> Category;
    fn collect(&self) -> Result<Vec<Fact>>;
}

/// Game versions of all launchers: one fact per installed game.
pub struct GameBuilds;

impl Collector for GameBuilds {
    fn name(&self) -> &'static str {
        "game_builds"
    }
    fn category(&self) -> Category {
        Category::GameBuild
    }
    fn collect(&self) -> Result<Vec<Fact>> {
        Ok(launchers::installed_games()?
            .into_iter()
            .map(|g| Fact::new(Category::GameBuild, g.id, g.name, g.build_id))
            .collect())
    }
}

/// The collectors available on this platform.
pub fn default_collectors() -> Vec<Box<dyn Collector>> {
    let mut list: Vec<Box<dyn Collector>> = Vec::new();
    #[cfg(windows)]
    {
        list.push(Box::new(windows::OsBuild));
        list.push(Box::new(windows::WindowsUpdates));
        list.push(Box::new(windows::DeviceDrivers::gpu()));
        list.push(Box::new(windows::DeviceDrivers::audio()));
        list.push(Box::new(windows::Bios));
        list.push(Box::new(windows::PowerPlan));
        list.push(Box::new(windows::SystemSettings));
        list.push(Box::new(windows::Autostart));
    }
    list.push(Box::new(GameBuilds));
    list
}

/// Outcome of one full scan.
#[derive(Debug, Default)]
pub struct ScanReport {
    pub changes: Vec<ChangeEvent>,
    /// `(collector name, error message)` for collectors that failed.
    pub failures: Vec<(&'static str, String)>,
}

/// Run all collectors and journal the differences to the previous scan.
pub fn run_scan(
    store: &mut Store,
    collectors: &[Box<dyn Collector>],
    now: Timestamp,
) -> Result<ScanReport> {
    let mut report = ScanReport::default();
    for c in collectors {
        match c.collect() {
            Ok(facts) => {
                let mut events = store.apply_scan(c.category(), &facts, now)?;
                report.changes.append(&mut events);
            }
            Err(e) => {
                tracing::warn!(collector = c.name(), error = %e, "collector failed, skipped");
                report.failures.push((c.name(), e.to_string()));
            }
        }
    }
    Ok(report)
}

#[cfg(windows)]
pub mod windows {
    use std::collections::BTreeSet;

    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ};
    use winreg::{RegKey, HKEY};

    use super::Collector;
    use crate::error::{Error, Result};
    use crate::model::{Category, Fact};

    fn open(hive: HKEY, path: &str, collector: &'static str) -> Result<RegKey> {
        RegKey::predef(hive)
            .open_subkey_with_flags(path, KEY_READ)
            .map_err(|e| Error::Collector {
                collector,
                message: format!("{path}: {e}"),
            })
    }

    fn string(key: &RegKey, name: &str) -> Option<String> {
        key.get_value::<String, _>(name).ok().map(|s| s.trim().to_string())
    }

    fn dword(key: &RegKey, name: &str) -> Option<u32> {
        key.get_value::<u32, _>(name).ok()
    }

    // ---- Windows build ------------------------------------------------------

    pub struct OsBuild;

    impl Collector for OsBuild {
        fn name(&self) -> &'static str {
            "os_build"
        }
        fn category(&self) -> Category {
            Category::OsBuild
        }
        fn collect(&self) -> Result<Vec<Fact>> {
            let key = open(
                HKEY_LOCAL_MACHINE,
                r"SOFTWARE\Microsoft\Windows NT\CurrentVersion",
                self.name(),
            )?;
            let build = string(&key, "CurrentBuild").unwrap_or_default();
            let ubr = dword(&key, "UBR").unwrap_or(0);
            let display = string(&key, "DisplayVersion").unwrap_or_default();
            // UBR rises with every cumulative update, so this single fact
            // already catches monthly patches even if the KB list is unreadable.
            Ok(vec![Fact::new(
                Category::OsBuild,
                "windows",
                "Windows",
                format!("{display} (Build {build}.{ubr})").trim().to_string(),
            )])
        }
    }

    // ---- Installed updates (KB numbers) -------------------------------------

    pub struct WindowsUpdates;

    /// Extract `KB1234567` from a servicing package name.
    pub(crate) fn kb_from_package_name(name: &str) -> Option<String> {
        let upper = name.to_ascii_uppercase();
        let start = upper.find("KB")?;
        let digits: String = upper[start + 2..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        (digits.len() >= 6).then(|| format!("KB{digits}"))
    }

    impl Collector for WindowsUpdates {
        fn name(&self) -> &'static str {
            "windows_updates"
        }
        fn category(&self) -> Category {
            Category::WindowsUpdate
        }
        fn collect(&self) -> Result<Vec<Fact>> {
            let key = open(
                HKEY_LOCAL_MACHINE,
                r"SOFTWARE\Microsoft\Windows\CurrentVersion\Component Based Servicing\Packages",
                self.name(),
            )?;
            let kbs: BTreeSet<String> = key
                .enum_keys()
                .flatten()
                .filter_map(|name| kb_from_package_name(&name))
                .collect();
            Ok(kbs
                .into_iter()
                .map(|kb| Fact::new(Category::WindowsUpdate, kb.clone(), kb, "installed"))
                .collect())
        }
    }

    // ---- Device drivers by setup class --------------------------------------

    pub struct DeviceDrivers {
        name: &'static str,
        category: Category,
        class_guid: &'static str,
    }

    impl DeviceDrivers {
        pub fn gpu() -> Self {
            DeviceDrivers {
                name: "gpu_driver",
                category: Category::GpuDriver,
                class_guid: "{4d36e968-e325-11ce-bfc1-08002be10318}",
            }
        }
        pub fn audio() -> Self {
            DeviceDrivers {
                name: "audio_driver",
                category: Category::AudioDriver,
                class_guid: "{4d36e96c-e325-11ce-bfc1-08002be10318}",
            }
        }
    }

    impl Collector for DeviceDrivers {
        fn name(&self) -> &'static str {
            self.name
        }
        fn category(&self) -> Category {
            self.category
        }
        fn collect(&self) -> Result<Vec<Fact>> {
            let path = format!(r"SYSTEM\CurrentControlSet\Control\Class\{}", self.class_guid);
            let class = open(HKEY_LOCAL_MACHINE, &path, self.name)?;
            let mut facts: Vec<Fact> = Vec::new();
            for sub in class.enum_keys().flatten() {
                // Device instances are "0000", "0001", ...; skip "Properties" etc.
                if sub.len() != 4 || !sub.chars().all(|c| c.is_ascii_digit()) {
                    continue;
                }
                let Ok(dev) = class.open_subkey_with_flags(&sub, KEY_READ) else {
                    continue;
                };
                let (Some(desc), Some(version)) =
                    (string(&dev, "DriverDesc"), string(&dev, "DriverVersion"))
                else {
                    continue;
                };
                let date = string(&dev, "DriverDate").unwrap_or_default();
                let value = if date.is_empty() {
                    version
                } else {
                    format!("{version} ({date})")
                };
                // Several instances of the same device share one driver.
                if !facts.iter().any(|f| f.key == desc) {
                    facts.push(Fact::new(self.category, desc.clone(), desc, value));
                }
            }
            Ok(facts)
        }
    }

    // ---- BIOS ---------------------------------------------------------------

    pub struct Bios;

    impl Collector for Bios {
        fn name(&self) -> &'static str {
            "bios"
        }
        fn category(&self) -> Category {
            Category::Bios
        }
        fn collect(&self) -> Result<Vec<Fact>> {
            let key = open(HKEY_LOCAL_MACHINE, r"HARDWARE\DESCRIPTION\System\BIOS", self.name())?;
            let version = string(&key, "BIOSVersion").unwrap_or_default();
            let date = string(&key, "BIOSReleaseDate").unwrap_or_default();
            Ok(vec![Fact::new(
                Category::Bios,
                "bios",
                "BIOS/UEFI firmware",
                format!("{version} ({date})"),
            )])
        }
    }

    // ---- Power plan ---------------------------------------------------------

    pub struct PowerPlan;

    impl Collector for PowerPlan {
        fn name(&self) -> &'static str {
            "power_plan"
        }
        fn category(&self) -> Category {
            Category::PowerPlan
        }
        fn collect(&self) -> Result<Vec<Fact>> {
            let base = r"SYSTEM\CurrentControlSet\Control\Power\User\PowerSchemes";
            let key = open(HKEY_LOCAL_MACHINE, base, self.name())?;
            let guid = string(&key, "ActivePowerScheme").unwrap_or_default();
            let friendly = key
                .open_subkey_with_flags(&guid, KEY_READ)
                .ok()
                .and_then(|k| string(&k, "FriendlyName"))
                // Stored as "@%SystemRoot%\system32\powrprof.dll,-13,Balanced"
                .map(|s| s.rsplit(',').next().unwrap_or(&s).trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| guid.clone());
            Ok(vec![Fact::new(
                Category::PowerPlan,
                "active",
                "Active power plan",
                friendly,
            )])
        }
    }

    // ---- Gaming related Windows settings ------------------------------------

    pub struct SystemSettings;

    struct Setting {
        hive: HKEY,
        path: &'static str,
        value: &'static str,
        key: &'static str,
        label: &'static str,
        /// Maps the raw DWORD to display text.
        describe: fn(u32) -> String,
    }

    // Values are language-neutral codes; `i18n::fact_value` renders them.
    fn on_off(v: u32) -> String {
        if v != 0 { "on".into() } else { "off".into() }
    }

    fn hags(v: u32) -> String {
        match v {
            2 => "on".into(),
            1 => "off".into(),
            other => format!("unknown:{other}"),
        }
    }

    const SETTINGS: &[Setting] = &[
        Setting {
            hive: HKEY_CURRENT_USER,
            path: r"Software\Microsoft\GameBar",
            value: "AutoGameModeEnabled",
            key: "game_mode",
            label: "Game Mode",
            describe: on_off,
        },
        Setting {
            hive: HKEY_CURRENT_USER,
            path: r"System\GameConfigStore",
            value: "GameDVR_Enabled",
            key: "game_dvr",
            label: "Background recording (Game DVR)",
            describe: on_off,
        },
        Setting {
            hive: HKEY_LOCAL_MACHINE,
            path: r"SYSTEM\CurrentControlSet\Control\GraphicsDrivers",
            value: "HwSchMode",
            key: "hags",
            label: "Hardware-accelerated GPU scheduling",
            describe: hags,
        },
        Setting {
            hive: HKEY_LOCAL_MACHINE,
            path: r"SYSTEM\CurrentControlSet\Control\DeviceGuard\Scenarios\HypervisorEnforcedCodeIntegrity",
            value: "Enabled",
            key: "memory_integrity",
            label: "Memory integrity (core isolation)",
            describe: on_off,
        },
        Setting {
            hive: HKEY_LOCAL_MACHINE,
            path: r"SYSTEM\CurrentControlSet\Control\DeviceGuard",
            value: "EnableVirtualizationBasedSecurity",
            key: "vbs",
            label: "Virtualization-based security (VBS)",
            describe: on_off,
        },
    ];

    impl Collector for SystemSettings {
        fn name(&self) -> &'static str {
            "system_settings"
        }
        fn category(&self) -> Category {
            Category::SystemSetting
        }
        fn collect(&self) -> Result<Vec<Fact>> {
            // A missing key or value means "Windows default" and is reported as
            // such, so that a later explicit value shows up as a change.
            Ok(SETTINGS
                .iter()
                .map(|s| {
                    let raw = RegKey::predef(s.hive)
                        .open_subkey_with_flags(s.path, KEY_READ)
                        .ok()
                        .and_then(|k| dword(&k, s.value));
                    let text = raw.map(s.describe).unwrap_or_else(|| "default".into());
                    Fact::new(Category::SystemSetting, s.key, s.label, text)
                })
                .collect())
        }
    }

    // ---- Autostart entries --------------------------------------------------

    pub struct Autostart;

    impl Collector for Autostart {
        fn name(&self) -> &'static str {
            "autostart"
        }
        fn category(&self) -> Category {
            Category::Autostart
        }
        fn collect(&self) -> Result<Vec<Fact>> {
            let sources = [
                (HKEY_CURRENT_USER, "HKCU", r"Software\Microsoft\Windows\CurrentVersion\Run"),
                (HKEY_LOCAL_MACHINE, "HKLM", r"SOFTWARE\Microsoft\Windows\CurrentVersion\Run"),
                (
                    HKEY_LOCAL_MACHINE,
                    "HKLM32",
                    r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Run",
                ),
            ];
            let mut facts = Vec::new();
            for (hive, tag, path) in sources {
                let Ok(key) = RegKey::predef(hive).open_subkey_with_flags(path, KEY_READ) else {
                    continue;
                };
                for (name, _) in key.enum_values().flatten() {
                    let command = string(&key, &name).unwrap_or_default();
                    facts.push(Fact::new(
                        Category::Autostart,
                        format!("{tag}:{name}"),
                        name,
                        command,
                    ));
                }
            }
            Ok(facts)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn extracts_kb_numbers() {
            assert_eq!(
                kb_from_package_name("Package_for_KB5066835~31bf3856ad364e35~amd64~~26100.1.2"),
                Some("KB5066835".to_string())
            );
            assert_eq!(
                kb_from_package_name("Package_1_for_KB5043080~31bf3856ad364e35~amd64~~10.0.1.3"),
                Some("KB5043080".to_string())
            );
            assert_eq!(kb_from_package_name("Package_for_RollupFix~31bf~amd64~~26100.6899"), None);
            assert_eq!(kb_from_package_name("Microsoft-Windows-KBase-Package"), None);
        }

        /// Smoke test against the real registry: every collector must either
        /// succeed or fail cleanly, and the basic ones must report something.
        #[test]
        fn collectors_run_on_this_machine() {
            assert_eq!(OsBuild.collect().unwrap().len(), 1);
            assert_eq!(SystemSettings.collect().unwrap().len(), SETTINGS.len());
            assert_eq!(Bios.collect().unwrap().len(), 1);
            for c in super::super::default_collectors() {
                if let Ok(facts) = c.collect() {
                    assert!(facts.iter().all(|f| f.category == c.category()));
                    let mut keys: Vec<_> = facts.iter().map(|f| &f.key).collect();
                    keys.sort();
                    keys.dedup();
                    assert_eq!(keys.len(), facts.len(), "{} has duplicate keys", c.name());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;

    struct Fixed(Vec<Fact>);
    impl Collector for Fixed {
        fn name(&self) -> &'static str {
            "fixed"
        }
        fn category(&self) -> Category {
            Category::Bios
        }
        fn collect(&self) -> Result<Vec<Fact>> {
            Ok(self.0.clone())
        }
    }

    struct Broken;
    impl Collector for Broken {
        fn name(&self) -> &'static str {
            "broken"
        }
        fn category(&self) -> Category {
            Category::Bios
        }
        fn collect(&self) -> Result<Vec<Fact>> {
            Err(Error::Other("boom".into()))
        }
    }

    #[test]
    fn failing_collector_does_not_wipe_state() {
        let mut store = Store::open_in_memory().unwrap();
        let good: Vec<Box<dyn Collector>> =
            vec![Box::new(Fixed(vec![Fact::new(Category::Bios, "bios", "BIOS", "1.0")]))];
        run_scan(&mut store, &good, 100).unwrap();

        let bad: Vec<Box<dyn Collector>> = vec![Box::new(Broken)];
        let report = run_scan(&mut store, &bad, 200).unwrap();
        assert!(report.changes.is_empty());
        assert_eq!(report.failures.len(), 1);
        assert_eq!(store.current_state().unwrap().len(), 1);

        let newer: Vec<Box<dyn Collector>> =
            vec![Box::new(Fixed(vec![Fact::new(Category::Bios, "bios", "BIOS", "1.1")]))];
        let report = run_scan(&mut store, &newer, 300).unwrap();
        assert_eq!(report.changes.len(), 1);
    }
}
