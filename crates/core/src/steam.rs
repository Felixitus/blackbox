//! Discovery of installed Steam games from Steam's own manifest files.
//! Read-only: Blackbox never writes into the Steam directory.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::model::Game;
use crate::vdf;

/// App ids that are tools/runtimes rather than games.
const NON_GAME_APP_IDS: &[&str] = &[
    "228980",  // Steamworks Common Redistributables
    "1070560", // Steam Linux Runtime
    "1391110", // Steam Linux Runtime - Soldier
    "1628350", // Steam Linux Runtime - Sniper
    "250820",  // SteamVR
];

/// Locate the Steam installation directory, if Steam is installed.
#[cfg(windows)]
pub fn find_steam_root() -> Option<PathBuf> {
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    use winreg::RegKey;

    let candidates = [
        (HKEY_CURRENT_USER, r"Software\Valve\Steam", "SteamPath"),
        (HKEY_LOCAL_MACHINE, r"SOFTWARE\WOW6432Node\Valve\Steam", "InstallPath"),
        (HKEY_LOCAL_MACHINE, r"SOFTWARE\Valve\Steam", "InstallPath"),
    ];
    for (hive, path, value) in candidates {
        if let Ok(key) = RegKey::predef(hive).open_subkey(path) {
            if let Ok(dir) = key.get_value::<String, _>(value) {
                let dir = PathBuf::from(dir.replace('/', "\\"));
                if dir.is_dir() {
                    return Some(dir);
                }
            }
        }
    }
    None
}

#[cfg(not(windows))]
pub fn find_steam_root() -> Option<PathBuf> {
    None
}

/// All library folders known to the Steam installation at `steam_root`.
pub fn library_folders(steam_root: &Path) -> Result<Vec<PathBuf>> {
    let mut libs = vec![steam_root.to_path_buf()];
    let vdf_path = steam_root.join("steamapps").join("libraryfolders.vdf");
    if vdf_path.is_file() {
        let text = fs::read_to_string(&vdf_path)?;
        let root = vdf::parse(&text).map_err(|e| {
            Error::Other(format!("{}: {e}", vdf_path.display()))
        })?;
        if let Some(folders) = root.get("libraryfolders").and_then(vdf::Value::as_block) {
            for value in folders.values() {
                let path = match value {
                    vdf::Value::Block(b) => vdf::get_str(b, "path"),
                    // Very old format: "1" "D:\\Lib"
                    vdf::Value::Str(s) => Some(s.as_str()),
                };
                if let Some(p) = path {
                    let p = PathBuf::from(p);
                    if p.is_dir() && !libs.iter().any(|l| same_path(l, &p)) {
                        libs.push(p);
                    }
                }
            }
        }
    }
    Ok(libs)
}

fn same_path(a: &Path, b: &Path) -> bool {
    a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy())
}

/// Parse one `appmanifest_*.acf`. Returns `None` for tools and broken files.
pub fn parse_manifest(text: &str, library: &Path) -> Option<Game> {
    let root = vdf::parse(text).ok()?;
    let app = root.get("appstate")?.as_block()?;
    let appid = vdf::get_str(app, "appid")?;
    if NON_GAME_APP_IDS.contains(&appid) {
        return None;
    }
    let name = vdf::get_str(app, "name")?;
    let installdir = vdf::get_str(app, "installdir")?;
    let build_id = vdf::get_str(app, "buildid").unwrap_or("0");
    let install_dir = library.join("steamapps").join("common").join(installdir);
    Some(Game {
        id: format!("steam:{appid}"),
        name: clean_display_text(name),
        install_dir: install_dir.to_string_lossy().into_owned(),
        build_id: build_id.to_string(),
    })
}

/// Installed games in the given library folders, sorted by name.
pub fn games_in_libraries(libraries: &[PathBuf]) -> Result<Vec<Game>> {
    let mut games: Vec<Game> = Vec::new();
    for lib in libraries {
        let apps = lib.join("steamapps");
        let Ok(entries) = fs::read_dir(&apps) else {
            continue; // library on a disconnected drive
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !(name.starts_with("appmanifest_") && name.ends_with(".acf")) {
                continue;
            }
            match fs::read_to_string(entry.path()) {
                Ok(text) => {
                    if let Some(game) = parse_manifest(&text, lib) {
                        if !games.iter().any(|g| g.id == game.id) {
                            games.push(game);
                        }
                    }
                }
                Err(e) => tracing::warn!(file = %entry.path().display(), error = %e, "manifest unreadable"),
            }
        }
    }
    games.sort_by_key(|g| g.name.to_lowercase());
    Ok(games)
}

/// Remove control and invisible formatting characters (bidi overrides,
/// zero-width spaces). User-chosen names may contain them, and they can
/// reorder or hide the surrounding text.
pub fn clean_display_text(s: &str) -> String {
    s.chars()
        .filter(|c| {
            !c.is_control()
                && !matches!(*c as u32, 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x2069 | 0xFEFF)
        })
        .collect::<String>()
        .trim()
        .to_string()
}

/// Persona names of Steam accounts that signed in on this PC, keyed by the
/// 32-bit account id Steam uses for its `userdata` folders.
pub fn account_names(steam_root: &Path) -> std::collections::HashMap<String, String> {
    const STEAM_ID64_BASE: u64 = 76_561_197_960_265_728;
    let path = steam_root.join("config").join("loginusers.vdf");
    let Some(root) = fs::read_to_string(path).ok().and_then(|t| vdf::parse(&t).ok()) else {
        return Default::default();
    };
    let Some(users) = root.get("users").and_then(vdf::Value::as_block) else {
        return Default::default();
    };
    users
        .iter()
        .filter_map(|(id64, v)| {
            let account = id64.parse::<u64>().ok()?.checked_sub(STEAM_ID64_BASE)?;
            let name = clean_display_text(vdf::get_str(v.as_block()?, "personaname")?);
            (!name.is_empty()).then(|| (account.to_string(), name))
        })
        .collect()
}

/// Installed Steam games on this machine. Empty if Steam is not installed.
pub fn installed_games() -> Result<Vec<Game>> {
    match find_steam_root() {
        Some(root) => games_in_libraries(&library_folders(&root)?),
        None => Ok(Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(appid: &str, name: &str, dir: &str, build: &str) -> String {
        format!(
            "\"AppState\"\n{{\n\t\"appid\"\t\"{appid}\"\n\t\"name\"\t\"{name}\"\n\t\"installdir\"\t\"{dir}\"\n\t\"buildid\"\t\"{build}\"\n}}\n"
        )
    }

    #[test]
    fn display_text_is_cleaned() {
        assert_eq!(clean_display_text("Player:\u{2067}\u{2067} One\u{200B}Two "), "Player: OneTwo");
        assert_eq!(clean_display_text("\u{202E}\u{0007}"), "");
    }

    #[test]
    fn finds_games_across_libraries() {
        let root = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        for lib in [root.path(), second.path()] {
            fs::create_dir_all(lib.join("steamapps")).unwrap();
        }
        let escaped = second.path().to_string_lossy().replace('\\', "\\\\");
        fs::write(
            root.path().join("steamapps/libraryfolders.vdf"),
            format!("\"libraryfolders\" {{ \"0\" {{ \"path\" \"{escaped}\" }} }}"),
        )
        .unwrap();
        fs::write(
            root.path().join("steamapps/appmanifest_730.acf"),
            manifest("730", "Counter-Strike 2", "CS2", "111"),
        )
        .unwrap();
        fs::write(
            second.path().join("steamapps/appmanifest_892970.acf"),
            manifest("892970", "Valheim", "Valheim", "222"),
        )
        .unwrap();
        fs::write(
            second.path().join("steamapps/appmanifest_228980.acf"),
            manifest("228980", "Steamworks Common Redistributables", "Steamworks Shared", "1"),
        )
        .unwrap();
        fs::write(second.path().join("steamapps/appmanifest_9.acf"), "garbage {").unwrap();

        let libs = library_folders(root.path()).unwrap();
        assert_eq!(libs.len(), 2);
        let games = games_in_libraries(&libs).unwrap();
        let ids: Vec<_> = games.iter().map(|g| g.id.as_str()).collect();
        assert_eq!(ids, vec!["steam:730", "steam:892970"]);
        assert_eq!(games[1].build_id, "222");
        assert!(games[1].install_dir.ends_with("Valheim"));
    }
}
