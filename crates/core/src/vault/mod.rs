//! Loadout Vault: versioning and reset protection for game settings.
//!
//! Tracked config files are snapshotted whenever they change. Changes are
//! also written to the Blackbox change journal (category `GameConfig`), so a
//! settings reset shows up as a suspect when performance drops.

pub mod discover;
pub mod parse;
pub mod selective;
pub mod store;

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

pub use discover::{tracked_files, KnownDirs};
pub use store::{
    DiffSummary, FileDiff, FileInfo, GameVault, ImportItem, Profile, Reason, RestoreOutcome, SnapshotInfo,
    TrackedFile, Vault, VaultEvent,
};

use crate::error::Result;
use crate::i18n;
use crate::model::{Category, ChangeEvent, ChangeKind, Game, Timestamp};
use crate::store::Store;

/// Check the config files of `games` and journal every change.
/// With `only` set, just that game is checked (used during a session).
pub fn check_and_journal(
    vault: &mut Vault,
    store: &Store,
    games: &[Game],
    dirs: &KnownDirs,
    custom: &BTreeMap<String, Vec<PathBuf>>,
    only: Option<&str>,
    now: Timestamp,
) -> Result<Vec<ChangeEvent>> {
    let selected: Vec<Game> = games.iter().filter(|g| only.is_none_or(|id| g.id == id)).cloned().collect();
    let files = tracked_files(&selected, dirs, custom);
    let builds: HashMap<String, String> = selected.iter().map(|g| (g.id.clone(), g.build_id.clone())).collect();
    let events = vault.check(&files, &builds, now)?;
    events
        .into_iter()
        .map(|e| {
            store.record_change(
                Category::GameConfig,
                &e.game_id,
                &format!("{}: {}", e.game_name, e.file_name),
                ChangeKind::Modified,
                None,
                Some(&e.summary.code()),
                now,
            )
        })
        .collect()
}

/// Restore a snapshot and note it in the change journal. With
/// `personal_only`, display and graphics settings of the current file stay.
pub fn restore_and_journal(
    vault: &mut Vault,
    store: &Store,
    snapshot_id: i64,
    personal_only: bool,
    now: Timestamp,
) -> Result<RestoreOutcome> {
    let out = if personal_only { vault.restore_personal(snapshot_id, now)? } else { vault.restore(snapshot_id, now)? };
    store.record_change(
        Category::GameConfig,
        &out.game_id,
        &format!("{}: {}", out.game_name, out.label),
        ChangeKind::Modified,
        None,
        Some(&match out.personal_keys {
            Some(n) => i18n::vault_personal_code(n, out.restored_from),
            None => i18n::vault_restored_code(out.restored_from),
        }),
        now,
    )?;
    Ok(out)
}

/// Import a profile and note every changed file in the change journal.
pub fn import_and_journal(vault: &mut Vault, store: &Store, profile: &Profile, now: Timestamp) -> Result<Vec<ImportItem>> {
    let items = vault.import_profile(profile, true, now)?;
    for item in items.iter().filter(|i| i.applied) {
        store.record_change(
            Category::GameConfig,
            &item.game_id,
            &format!("{}: {}", item.game_name, item.label),
            ChangeKind::Modified,
            None,
            Some(&i18n::vault_imported_code(item.changed.len())),
            now,
        )?;
    }
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn changes_reach_the_journal() {
        let root = tempfile::tempdir().unwrap();
        let install = root.path().join("game");
        fs::create_dir_all(install.join("Proj/Binaries")).unwrap();
        let ini = root.path().join("Local/Proj/Saved/Config/Windows/GameUserSettings.ini");
        fs::create_dir_all(ini.parent().unwrap()).unwrap();
        fs::write(&ini, "a=1\nb=1\nc=1\n").unwrap();
        let dirs = KnownDirs { local_appdata: Some(root.path().join("Local")), ..Default::default() };
        let mut game = Game {
            id: "steam:5".into(),
            name: "Proj Game".into(),
            install_dir: install.to_string_lossy().into_owned(),
            build_id: "1".into(),
        };
        let store = Store::open_in_memory().unwrap();
        let mut vault = Vault::open(&root.path().join("vault")).unwrap();
        let none = BTreeMap::new();

        assert!(check_and_journal(&mut vault, &store, &[game.clone()], &dirs, &none, None, 10).unwrap().is_empty());
        fs::write(&ini, "a=0\nb=0\nc=0\n").unwrap();
        game.build_id = "2".into();
        // Restricting to another game finds nothing.
        assert!(check_and_journal(&mut vault, &store, &[game.clone()], &dirs, &none, Some("steam:6"), 20).unwrap().is_empty());
        let ev = check_and_journal(&mut vault, &store, &[game.clone()], &dirs, &none, Some("steam:5"), 20).unwrap();
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].category, Category::GameConfig);
        assert_eq!(ev[0].key, "steam:5");
        assert_eq!(ev[0].label, "Proj Game: GameUserSettings.ini");
        assert!(i18n::is_reset_value(ev[0].new_value.as_deref().unwrap()));
        let shown = i18n::with_lang(i18n::Lang::De, || i18n::display_change(&ev[0]));
        assert!(shown.new_value.unwrap().contains("Reset nach Spiel-Update"));
        assert_eq!(store.changes_between(0, 100).unwrap().len(), 1);

        let first = vault.overview().unwrap()[0].files[0].snapshots[1].id;
        restore_and_journal(&mut vault, &store, first, false, 30).unwrap();
        assert_eq!(fs::read_to_string(&ini).unwrap(), "a=1\nb=1\nc=1\n");
        let journal = store.changes_between(0, 100).unwrap();
        let shown = i18n::with_lang(i18n::Lang::En, || i18n::display_change(&journal[1]));
        assert!(shown.new_value.unwrap().starts_with("Version from"), "restore entry rendered in English");
    }
}
