//! Installed games from all supported launchers.
//!
//! | Launcher  | Source                                                     | Id prefix  |
//! |-----------|------------------------------------------------------------|------------|
//! | Steam     | `appmanifest_*.acf` (see `steam`)                          | `steam:`   |
//! | Epic      | `%PROGRAMDATA%\Epic\EpicGamesLauncher\Data\Manifests\*.item` | `epic:`  |
//! | GOG       | `HKLM\SOFTWARE\WOW6432Node\GOG.com\Games`                  | `gog:`     |
//! | Battle.net, EA, Ubisoft, Riot | Windows uninstall entries by publisher  | `bnet:`, `ea:`, `ubisoft:`, `riot:` |
//!
//! All sources are read-only. A game found by several sources (e.g. an EA
//! game bought on Steam) is kept once, Steam first.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Deserialize;

use crate::error::Result;
use crate::model::Game;
use crate::steam;

/// Human readable launcher name for a game id.
pub fn launcher_name(game_id: &str) -> &'static str {
    match game_id.split(':').next().unwrap_or_default() {
        "steam" => "Steam",
        "epic" => "Epic Games",
        "gog" => "GOG",
        "bnet" => "Battle.net",
        "ea" => "EA app",
        "ubisoft" => "Ubisoft Connect",
        "riot" => "Riot",
        _ => crate::i18n::t("Sonstige", "Other"),
    }
}

fn normalize_dir(dir: &str) -> String {
    dir.replace('/', "\\").trim_end_matches('\\').to_lowercase()
}

/// Version stand-in for launchers that do not expose a build number: the
/// newest modification time of the executables below the install folder.
/// Patches nearly always replace the game executable.
pub fn exe_timestamp(dir: &Path) -> Option<String> {
    fn walk(dir: &Path, depth: u32, budget: &mut u32, newest: &mut Option<SystemTime>) {
        let Ok(entries) = fs::read_dir(dir) else { return };
        for e in entries.flatten() {
            if *budget == 0 {
                return;
            }
            *budget -= 1;
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() && depth > 0 {
                walk(&e.path(), depth - 1, budget, newest);
            } else if ft.is_file()
                && e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("exe"))
            {
                if let Ok(m) = e.metadata().and_then(|m| m.modified()) {
                    if newest.is_none_or(|n| m > n) {
                        *newest = Some(m);
                    }
                }
            }
        }
    }
    let mut newest = None;
    let mut budget = 20_000;
    walk(dir, 4, &mut budget, &mut newest);
    let t: chrono::DateTime<chrono::Utc> = newest?.into();
    // Language-neutral; `i18n::fact_value` renders "files from ...".
    Some(format!("files {}", t.format("%Y-%m-%d %H:%M")))
}

// ---- Epic Games ---------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct EpicManifest {
    display_name: String,
    install_location: String,
    app_name: String,
    #[serde(default)]
    app_version_string: String,
    #[serde(default)]
    main_game_app_name: String,
    #[serde(default)]
    app_categories: Vec<String>,
    #[serde(default, rename = "bIsIncompleteInstall")]
    incomplete: bool,
}

pub fn parse_epic_manifest(text: &str) -> Option<Game> {
    let m: EpicManifest = serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()?;
    let is_dlc = !m.main_game_app_name.is_empty() && m.main_game_app_name != m.app_name;
    let is_game = m.app_categories.is_empty() || m.app_categories.iter().any(|c| c == "games");
    if m.incomplete || is_dlc || !is_game || m.install_location.is_empty() {
        return None;
    }
    Some(Game {
        id: format!("epic:{}", m.app_name),
        name: steam::clean_display_text(&m.display_name),
        install_dir: m.install_location,
        build_id: m.app_version_string,
    })
}

fn epic_games(manifest_dir: &Path) -> Vec<Game> {
    let Ok(entries) = fs::read_dir(manifest_dir) else { return Vec::new() };
    entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("item")))
        .filter_map(|e| fs::read_to_string(e.path()).ok())
        .filter_map(|t| parse_epic_manifest(&t))
        .collect()
}

fn epic_manifest_dir() -> PathBuf {
    let program_data = std::env::var_os("PROGRAMDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
    program_data.join(r"Epic\EpicGamesLauncher\Data\Manifests")
}

// ---- Windows uninstall entries ---------------------------------------------------

/// One entry of the Windows uninstall list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UninstallEntry {
    pub key: String,
    pub name: String,
    pub publisher: String,
    pub install_location: String,
    pub version: String,
}

/// Launcher applications and runtimes that are not games.
const NOT_GAMES: &[&str] = &[
    "battle.net", "ea app", "origin", "ubisoft connect", "riot client", "epic games launcher",
    "rockstar games launcher", "rockstar games sdk", "epic online services",
];

/// Map an uninstall entry to a game of a supported launcher.
pub fn game_from_uninstall(e: &UninstallEntry) -> Option<Game> {
    let name = steam::clean_display_text(&e.name);
    let lower = name.to_lowercase();
    if e.install_location.trim().is_empty()
        || NOT_GAMES.contains(&lower.as_str())
        || lower.contains("prerequisites")
        || lower.contains("redistributable")
    {
        return None;
    }
    let publisher = e.publisher.to_lowercase();
    let prefix = if let Some(id) = e.key.strip_prefix("Uplay Install ") {
        return Some(Game {
            id: format!("ubisoft:{id}"),
            name,
            install_dir: e.install_location.trim().to_string(),
            build_id: e.version.clone(),
        });
    } else if let Some(product) = e.key.strip_prefix("Riot Game ") {
        if product.to_lowercase().starts_with("riot_client") {
            return None;
        }
        return Some(Game {
            id: format!("riot:{product}"),
            name,
            install_dir: e.install_location.trim().to_string(),
            build_id: e.version.clone(),
        });
    } else if publisher.contains("blizzard") {
        "bnet"
    } else if publisher.contains("electronic arts") {
        "ea"
    } else {
        return None;
    };
    let key = e.key.trim_matches(|c| c == '{' || c == '}');
    Some(Game {
        id: format!("{prefix}:{key}"),
        name,
        install_dir: e.install_location.trim().to_string(),
        build_id: e.version.clone(),
    })
}

#[cfg(windows)]
fn uninstall_entries() -> Vec<UninstallEntry> {
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ};
    use winreg::RegKey;

    let roots = [
        (HKEY_LOCAL_MACHINE, r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall"),
        (HKEY_LOCAL_MACHINE, r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall"),
        (HKEY_CURRENT_USER, r"Software\Microsoft\Windows\CurrentVersion\Uninstall"),
    ];
    let mut out = Vec::new();
    for (hive, path) in roots {
        let Ok(root) = RegKey::predef(hive).open_subkey_with_flags(path, KEY_READ) else { continue };
        for key in root.enum_keys().flatten() {
            let Ok(k) = root.open_subkey_with_flags(&key, KEY_READ) else { continue };
            let get = |n: &str| k.get_value::<String, _>(n).unwrap_or_default();
            out.push(UninstallEntry {
                key,
                name: get("DisplayName"),
                publisher: get("Publisher"),
                install_location: get("InstallLocation"),
                version: get("DisplayVersion"),
            });
        }
    }
    out
}

#[cfg(not(windows))]
fn uninstall_entries() -> Vec<UninstallEntry> {
    Vec::new()
}

// ---- GOG ---------------------------------------------------------------------

#[cfg(windows)]
fn gog_games() -> Vec<Game> {
    use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ};
    use winreg::RegKey;

    let Ok(root) = RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey_with_flags(r"SOFTWARE\WOW6432Node\GOG.com\Games", KEY_READ)
    else {
        return Vec::new();
    };
    root.enum_keys()
        .flatten()
        .filter_map(|id| {
            let k = root.open_subkey_with_flags(&id, KEY_READ).ok()?;
            let get = |n: &str| k.get_value::<String, _>(n).ok().filter(|s| !s.is_empty());
            // DLCs carry the id of their base game in `dependsOn`.
            if get("dependsOn").is_some() {
                return None;
            }
            Some(Game {
                id: format!("gog:{id}"),
                name: steam::clean_display_text(&get("gameName")?),
                install_dir: get("path")?,
                build_id: get("buildId").or_else(|| get("ver")).unwrap_or_default(),
            })
        })
        .collect()
}

#[cfg(not(windows))]
fn gog_games() -> Vec<Game> {
    Vec::new()
}

// ---- combined ------------------------------------------------------------------

/// Merge game lists, dropping later duplicates of an install folder, and
/// fill missing versions from the executables' timestamps.
pub fn merge(sources: Vec<Vec<Game>>) -> Vec<Game> {
    let mut out: Vec<Game> = Vec::new();
    for list in sources {
        for mut g in list {
            let dir = normalize_dir(&g.install_dir);
            if dir.is_empty() || out.iter().any(|o| o.id == g.id || normalize_dir(&o.install_dir) == dir) {
                continue;
            }
            if g.build_id.trim().is_empty() {
                g.build_id = exe_timestamp(Path::new(&g.install_dir)).unwrap_or_default();
            }
            out.push(g);
        }
    }
    out.sort_by_key(|g| g.name.to_lowercase());
    out
}

/// Installed games of all supported launchers, sorted by name.
pub fn installed_games() -> Result<Vec<Game>> {
    let steam = steam::installed_games().unwrap_or_else(|e| {
        tracing::warn!(error = %e, "Steam library unreadable");
        Vec::new()
    });
    let uninstall: Vec<Game> = uninstall_entries().iter().filter_map(game_from_uninstall).collect();
    // Uninstall entries of games whose folder no longer exists are stale.
    let uninstall = uninstall.into_iter().filter(|g| Path::new(&g.install_dir).is_dir()).collect();
    Ok(merge(vec![steam, epic_games(&epic_manifest_dir()), gog_games(), uninstall]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(key: &str, name: &str, publisher: &str, dir: &str) -> UninstallEntry {
        UninstallEntry {
            key: key.into(),
            name: name.into(),
            publisher: publisher.into(),
            install_location: dir.into(),
            version: String::new(),
        }
    }

    #[test]
    fn epic_manifests() {
        let game = r#"{"DisplayName":"Rocket League®","InstallLocation":"C:\\Epic\\rocketleague","AppName":"Sugar",
            "AppVersionString":"++Prime+Update60","MainGameAppName":"Sugar","AppCategories":["public","games","applications"]}"#;
        let g = parse_epic_manifest(game).unwrap();
        assert_eq!(g.id, "epic:Sugar");
        assert_eq!(g.name, "Rocket League®");
        assert_eq!(g.build_id, "++Prime+Update60");

        let dlc = r#"{"DisplayName":"Pack","InstallLocation":"C:\\x","AppName":"Pack1","MainGameAppName":"Sugar","AppCategories":["addons"]}"#;
        assert!(parse_epic_manifest(dlc).is_none());
        let partial = r#"{"DisplayName":"A","InstallLocation":"C:\\a","AppName":"A","bIsIncompleteInstall":true}"#;
        assert!(parse_epic_manifest(partial).is_none());
        let tool = r#"{"DisplayName":"UE","InstallLocation":"C:\\ue","AppName":"UE_5","AppCategories":["engines"]}"#;
        assert!(parse_epic_manifest(tool).is_none());
        assert!(parse_epic_manifest("not json").is_none());
    }

    #[test]
    fn uninstall_entries_map_to_launchers() {
        let cases = [
            (entry("Uplay Install 635", "Tom Clancy's Rainbow Six Siege X", "Ubisoft Montreal", "E:/ubi/R6/"), Some("ubisoft:635")),
            (entry("Riot Game valorant.live", "VALORANT", "Riot Games, Inc", "C:/Riot Games/VALORANT/live"), Some("riot:valorant.live")),
            (entry("Riot Game Riot_Client.", "Riot Client ", "Riot Games, Inc", "C:/Riot Games/Riot Client"), None),
            (entry("{335B50BC-6130}", "Battlefield™ 1", "Electronic Arts", r"E:\Battlefield 1\"), Some("ea:335B50BC-6130")),
            (entry("{C2622085}", "EA app", "Electronic Arts", r"C:\EA"), None),
            (entry("Overwatch", "Overwatch", "Blizzard Entertainment", r"C:\Games\Overwatch"), Some("bnet:Overwatch")),
            (entry("Battle.net", "Battle.net", "Blizzard Entertainment", r"C:\Battle.net"), None),
            (entry("{X}", "UE Prerequisites (x64)", "Epic Games, Inc.", r"C:\x"), None),
            (entry("{Y}", "Some Game", "Electronic Arts", ""), None),
            (entry("{Z}", "Photoshop", "Adobe", r"C:\Adobe"), None),
        ];
        for (e, expected) in cases {
            assert_eq!(game_from_uninstall(&e).map(|g| g.id), expected.map(String::from), "{}", e.name);
        }
    }

    #[test]
    fn merge_dedupes_by_folder_and_fills_versions() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("bin")).unwrap();
        fs::write(dir.path().join("bin/game.exe"), b"x").unwrap();
        let path = dir.path().to_string_lossy().into_owned();
        let steam = vec![Game { id: "steam:1".into(), name: "FC 26".into(), install_dir: format!("{path}\\"), build_id: "7".into() }];
        let ea = vec![
            Game { id: "ea:a".into(), name: "FC 26".into(), install_dir: path.replace('\\', "/"), build_id: String::new() },
            Game { id: "ea:b".into(), name: "Other".into(), install_dir: path.clone() + "\\bin", build_id: String::new() },
        ];
        let merged = merge(vec![steam, ea]);
        let ids: Vec<_> = merged.iter().map(|g| g.id.as_str()).collect();
        assert_eq!(ids, vec!["steam:1", "ea:b"]);
        assert!(merged[1].build_id.starts_with("files "));
        assert_eq!(launcher_name("ea:b"), "EA app");
    }
}
