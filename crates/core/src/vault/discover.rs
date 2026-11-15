//! Finds the config files of installed games.
//!
//! Three generic rules cover most games without per-game data:
//! * Unreal Engine: `<install>/<Project>/Binaries` exists and settings live in
//!   `%LOCALAPPDATA%/<Project>/Saved/Config/Windows*/`.
//! * Source 2 (CS2, Dota 2, Deadlock): Steam's per-user `userdata` folder.
//! * A small built-in table for well-known games with other locations.
//!
//! Users can add their own files or folders per game in the config.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use super::store::{TrackedFile, MAX_FILE_BYTES};
use crate::model::Game;

/// Per-user folders the rules refer to.
#[derive(Debug, Clone, Default)]
pub struct KnownDirs {
    pub local_appdata: Option<PathBuf>,
    pub appdata: Option<PathBuf>,
    pub documents: Option<PathBuf>,
    pub steam_root: Option<PathBuf>,
}

impl KnownDirs {
    pub fn detect() -> KnownDirs {
        let base = directories::BaseDirs::new();
        KnownDirs {
            local_appdata: base.as_ref().map(|b| b.data_local_dir().to_path_buf()),
            appdata: base.as_ref().map(|b| b.data_dir().to_path_buf()),
            documents: directories::UserDirs::new().and_then(|u| u.document_dir().map(Path::to_path_buf)),
            steam_root: crate::steam::find_steam_root(),
        }
    }
}

/// Known locations outside the generic rules. `*` matches within one folder.
const BUILTIN: &[(&str, &[&str])] = &[
    ("steam:1245620", &["{APPDATA}/EldenRing/GraphicsConfig.xml"]),
    ("steam:1174180", &["{DOCUMENTS}/Rockstar Games/Red Dead Redemption 2/Settings/system.xml"]),
    ("steam:271590", &["{DOCUMENTS}/Rockstar Games/GTA V/settings.xml"]),
];

/// Unreal config files that hold user settings. Others change on every
/// launch or contain no settings.
const UNREAL_FILES: &[&str] = &["GameUserSettings.ini", "Input.ini", "Engine.ini", "Game.ini", "Scalability.ini"];

/// Case-insensitive match with `*` wildcards.
fn wildcard(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let n: Vec<char> = name.to_lowercase().chars().collect();
    let (mut pi, mut ni, mut star, mut mark) = (0, 0, None, 0);
    while ni < n.len() {
        if pi < p.len() && (p[pi] == n[ni] && p[pi] != '*') {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ni;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ni = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

fn expand(pattern: &str, dirs: &KnownDirs) -> Vec<PathBuf> {
    let mut segments = pattern.split('/');
    let base = match segments.next() {
        Some("{APPDATA}") => dirs.appdata.clone(),
        Some("{LOCALAPPDATA}") => dirs.local_appdata.clone(),
        Some("{DOCUMENTS}") => dirs.documents.clone(),
        _ => None,
    };
    let Some(base) = base else { return Vec::new() };
    let mut current = vec![base];
    for seg in segments {
        let mut next = Vec::new();
        for dir in &current {
            if seg.contains('*') {
                if let Ok(entries) = fs::read_dir(dir) {
                    for e in entries.flatten() {
                        if wildcard(seg, &e.file_name().to_string_lossy()) {
                            next.push(e.path());
                        }
                    }
                }
            } else {
                next.push(dir.join(seg));
            }
        }
        current = next;
    }
    current.into_iter().filter(|p| p.is_file()).collect()
}

fn files_in(dir: &Path, keep: impl Fn(&str) -> bool) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter(|e| keep(&e.file_name().to_string_lossy()))
        .filter(|e| e.metadata().is_ok_and(|m| m.len() <= MAX_FILE_BYTES))
        .map(|e| e.path())
        .collect();
    out.sort();
    out
}

fn unreal_files(game: &Game, dirs: &KnownDirs) -> Vec<PathBuf> {
    let Some(local) = &dirs.local_appdata else { return Vec::new() };
    let Ok(entries) = fs::read_dir(&game.install_dir) else { return Vec::new() };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let project = e.file_name().to_string_lossy().into_owned();
        // "Engine" is the shared engine folder, never a project.
        if project.eq_ignore_ascii_case("Engine") || !e.path().join("Binaries").is_dir() {
            continue;
        }
        let config = local.join(&project).join("Saved").join("Config");
        let Ok(platforms) = fs::read_dir(&config) else { continue };
        for p in platforms.flatten() {
            let name = p.file_name().to_string_lossy().to_lowercase();
            if p.path().is_dir() && (name.starts_with("windows") || name.starts_with("wingdk")) {
                out.extend(files_in(&p.path(), |f| UNREAL_FILES.iter().any(|u| u.eq_ignore_ascii_case(f))));
            }
        }
    }
    out
}

/// Steam keeps Source 2 settings per Steam account:
/// `userdata/<account>/<appid>/local/cfg` (video, convars, keys) and
/// `.../remote/cfg` (cloud-synced key bindings). With several accounts on
/// one PC, each file is labelled with the account name.
fn steam_userdata_files(game: &Game, dirs: &KnownDirs) -> Vec<(PathBuf, Option<String>)> {
    let (Some(root), Some(appid)) = (&dirs.steam_root, game.id.strip_prefix("steam:")) else {
        return Vec::new();
    };
    let Ok(accounts) = fs::read_dir(root.join("userdata")) else { return Vec::new() };
    let mut per_account: Vec<(String, Vec<PathBuf>)> = Vec::new();
    for account in accounts.flatten() {
        let app = account.path().join(appid);
        let mut files = files_in(&app.join("local").join("cfg"), |f| {
            let lower = f.to_lowercase();
            // `.vcfg_lastclouded` and `.bak` are copies, `_tools` is for mod tools.
            (lower.ends_with(".vcfg") || lower.ends_with(".txt")) && !lower.contains("_tools")
        });
        files.extend(files_in(&app.join("remote").join("cfg"), |f| f.to_lowercase().contains("keys")));
        if !files.is_empty() {
            per_account.push((account.file_name().to_string_lossy().into_owned(), files));
        }
    }
    let names = if per_account.len() > 1 { crate::steam::account_names(root) } else { Default::default() };
    let several = per_account.len() > 1;
    per_account
        .into_iter()
        .flat_map(|(account, files)| {
            let label = several.then(|| names.get(&account).cloned().unwrap_or_else(|| format!("Konto {account}")));
            files.into_iter().map(move |f| (f, label.clone()))
        })
        .collect()
}
fn custom_files(entries: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for p in entries {
        if p.is_dir() {
            out.extend(files_in(p, |_| true));
        } else if p.is_file() {
            out.push(p.clone());
        }
    }
    out
}

/// All config files of the given games that exist right now.
pub fn tracked_files(
    games: &[Game],
    dirs: &KnownDirs,
    custom: &BTreeMap<String, Vec<PathBuf>>,
) -> Vec<TrackedFile> {
    let mut out: Vec<TrackedFile> = Vec::new();
    for game in games {
        let mut paths: Vec<(PathBuf, Option<String>)> =
            unreal_files(game, dirs).into_iter().map(|p| (p, None)).collect();
        paths.extend(steam_userdata_files(game, dirs));
        for (id, patterns) in BUILTIN {
            if *id == game.id {
                for p in *patterns {
                    paths.extend(expand(p, dirs).into_iter().map(|p| (p, None)));
                }
            }
        }
        if let Some(extra) = custom.get(&game.id) {
            paths.extend(custom_files(extra).into_iter().map(|p| (p, None)));
        }
        for (path, context) in paths {
            let key = path.to_string_lossy().to_lowercase();
            if out.iter().any(|t| t.path.to_string_lossy().to_lowercase() == key) {
                continue;
            }
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let label = match context {
                Some(c) => format!("{name} · {c}"),
                None => name,
            };
            out.push(TrackedFile { game_id: game.id.clone(), game_name: game.name.clone(), path, label });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(p: &Path) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, "x=1\n").unwrap();
    }

    fn game(id: &str, dir: &Path) -> Game {
        Game { id: id.into(), name: id.into(), install_dir: dir.to_string_lossy().into_owned(), build_id: "1".into() }
    }

    fn names(files: &[TrackedFile]) -> Vec<String> {
        let mut v: Vec<String> = files.iter().map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned()).collect();
        v.sort();
        v
    }

    #[test]
    fn wildcard_matching() {
        assert!(wildcard("*.ini", "Game.INI"));
        assert!(wildcard("Windows*", "WindowsClient"));
        assert!(wildcard("a*b*c", "aXXbYYc"));
        assert!(!wildcard("a*b", "ac"));
        assert!(wildcard("*", ""));
    }

    #[test]
    fn finds_unreal_project_configs() {
        let root = tempfile::tempdir().unwrap();
        let install = root.path().join("common/THE FINALS");
        fs::create_dir_all(install.join("Discovery/Binaries/Win64")).unwrap();
        fs::create_dir_all(install.join("Engine/Binaries")).unwrap();
        let local = root.path().join("Local");
        touch(&local.join("Discovery/Saved/Config/WindowsClient/GameUserSettings.ini"));
        touch(&local.join("Discovery/Saved/Config/WindowsClient/Input.ini"));
        touch(&local.join("Discovery/Saved/Config/WindowsClient/Hardware.ini"));
        touch(&local.join("Discovery/Saved/Config/CrashReportClient/GameUserSettings.ini"));
        touch(&local.join("Engine/Saved/Config/Windows/Engine.ini"));
        let dirs = KnownDirs { local_appdata: Some(local), ..Default::default() };
        let files = tracked_files(&[game("steam:2073850", &install)], &dirs, &BTreeMap::new());
        assert_eq!(names(&files), vec!["GameUserSettings.ini", "Input.ini"]);
    }

    #[test]
    fn finds_source2_userdata_and_skips_copies() {
        let root = tempfile::tempdir().unwrap();
        let cfg = root.path().join("userdata/123/730/local/cfg");
        for f in ["cs2_video.txt", "cs2_video.txt.bak", "cs2_user_keys_0_slot0.vcfg",
                  "cs2_user_keys_0_slot0.vcfg_lastclouded", "cs2_video_tools.txt", "trustedlaunch.cfg"] {
            touch(&cfg.join(f));
        }
        let remote = root.path().join("userdata/123/730/remote/cfg");
        touch(&remote.join("cs2_preferred_items.txt"));
        touch(&remote.join("dotakeys_personal.lst"));
        let dirs = KnownDirs { steam_root: Some(root.path().to_path_buf()), ..Default::default() };
        let files = tracked_files(&[game("steam:730", root.path())], &dirs, &BTreeMap::new());
        assert_eq!(names(&files), vec!["cs2_user_keys_0_slot0.vcfg", "cs2_video.txt", "dotakeys_personal.lst"]);
        assert_eq!(files[0].label, files[0].path.file_name().unwrap().to_string_lossy());

        // A second account: labels name the account, from loginusers.vdf if known.
        touch(&root.path().join("userdata/456/730/local/cfg/cs2_video.txt"));
        fs::create_dir_all(root.path().join("config")).unwrap();
        fs::write(
            root.path().join("config/loginusers.vdf"),
            "\"users\" { \"76561197960265851\" { \"PersonaName\" \"Main\" } }",
        )
        .unwrap();
        let files = tracked_files(&[game("steam:730", root.path())], &dirs, &BTreeMap::new());
        let mut labels: Vec<_> = files.iter().filter(|f| f.label.starts_with("cs2_video")).map(|f| f.label.clone()).collect();
        labels.sort();
        assert_eq!(labels, vec!["cs2_video.txt · Konto 456", "cs2_video.txt · Main"]);
    }

    #[test]
    fn builtin_table_and_custom_paths() {
        let root = tempfile::tempdir().unwrap();
        let docs = root.path().join("Docs");
        touch(&docs.join("Rockstar Games/Red Dead Redemption 2/Settings/system.xml"));
        let extra_dir = root.path().join("mygame");
        touch(&extra_dir.join("a.json"));
        touch(&extra_dir.join("b.ini"));
        let single = root.path().join("single.cfg");
        touch(&single);
        let dirs = KnownDirs { documents: Some(docs), ..Default::default() };
        let custom = BTreeMap::from([("steam:9".to_string(), vec![extra_dir, single.clone(), single])]);
        let games = [game("steam:1174180", root.path()), game("steam:9", root.path())];
        let files = tracked_files(&games, &dirs, &custom);
        assert_eq!(names(&files), vec!["a.json", "b.ini", "single.cfg", "system.xml"]);
        assert!(files.iter().filter(|f| f.game_id == "steam:9").count() == 3);
    }
}
