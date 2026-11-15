//! Snapshot storage: SQLite index plus content-addressed blobs.
//!
//! Every distinct version of a tracked config file is kept once under its
//! SHA-256 hash. Restoring always backs up the current file first, so every
//! restore can itself be undone.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::parse::{self, Content, KeyChange};
use super::selective;
use crate::error::{Error, Result};
use crate::model::Timestamp;

/// Larger files are not config files and are skipped.
pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
/// Snapshots kept per file; older ones are pruned.
pub const KEEP_SNAPSHOTS: i64 = 50;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS files (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    game_id   TEXT NOT NULL,
    game_name TEXT NOT NULL,
    label     TEXT NOT NULL,
    path      TEXT NOT NULL UNIQUE COLLATE NOCASE
);
CREATE TABLE IF NOT EXISTS snapshots (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    file_id       INTEGER NOT NULL REFERENCES files(id),
    taken_at      INTEGER NOT NULL,
    hash          TEXT NOT NULL,
    size          INTEGER NOT NULL,
    game_build    TEXT,
    reason        TEXT NOT NULL,
    changed       INTEGER NOT NULL DEFAULT 0,
    total         INTEGER NOT NULL DEFAULT 0,
    is_binary     INTEGER NOT NULL DEFAULT 0,
    build_changed INTEGER NOT NULL DEFAULT 0,
    reset_likely  INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_snapshots_file ON snapshots(file_id, id);
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// First time the file was seen.
    Baseline,
    /// The file changed since the previous snapshot.
    Changed,
    /// Automatic backup of the current file right before a restore.
    BeforeRestore,
    /// Written back by Blackbox.
    Restored,
}

impl Reason {
    fn as_str(self) -> &'static str {
        match self {
            Reason::Baseline => "baseline",
            Reason::Changed => "changed",
            Reason::BeforeRestore => "before_restore",
            Reason::Restored => "restored",
        }
    }

    fn parse(s: &str) -> Reason {
        match s {
            "baseline" => Reason::Baseline,
            "before_restore" => Reason::BeforeRestore,
            "restored" => Reason::Restored,
            _ => Reason::Changed,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct DiffSummary {
    pub changed: usize,
    pub total: usize,
    pub binary: bool,
    /// The game was updated between the two versions.
    pub build_changed: bool,
    pub reset_likely: bool,
}

impl DiffSummary {
    /// Text in the current language.
    pub fn describe(&self) -> String {
        crate::i18n::changed_text(self.changed as i64, self.total as i64, self.reset_likely, self.build_changed, self.binary)
    }

    /// Language-neutral code for the change journal.
    pub fn code(&self) -> String {
        crate::i18n::vault_changed_code(self.changed, self.total, self.reset_likely, self.build_changed, self.binary)
    }
}

/// Compare two versions of a file and judge whether it looks like a reset.
pub fn summarize(file_name: &str, old: &[u8], new: &[u8], build_changed: bool) -> DiffSummary {
    let (Content::Text(a), Content::Text(b)) = (parse::parse(old, file_name), parse::parse(new, file_name))
    else {
        return DiffSummary { changed: 1, total: 1, binary: true, build_changed, reset_likely: false };
    };
    let changed = parse::diff(&a, &b).len();
    let total = parse::union_len(&a, &b);
    // Many settings changing at once is unusual for a user tweaking one
    // option; together with a game update it is a strong reset signal.
    let reset_likely = (changed >= 5 && changed * 10 >= total * 3) || (build_changed && changed >= 3);
    DiffSummary { changed, total, binary: false, build_changed, reset_likely }
}

/// A config file Blackbox watches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackedFile {
    pub game_id: String,
    pub game_name: String,
    pub path: PathBuf,
    /// Display name: the file name, plus context where names repeat.
    pub label: String,
}

/// A detected change of a tracked file.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VaultEvent {
    pub game_id: String,
    pub game_name: String,
    pub file_name: String,
    pub snapshot_id: i64,
    pub summary: DiffSummary,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SnapshotInfo {
    pub id: i64,
    pub taken_at: Timestamp,
    pub reason: Reason,
    pub size: u64,
    pub game_build: Option<String>,
    pub summary: DiffSummary,
    /// Whether this is the version currently on disk (as of the last check).
    pub current: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FileInfo {
    pub file_id: i64,
    pub path: String,
    pub file_name: String,
    pub label: String,
    pub exists: bool,
    /// Newest first.
    pub snapshots: Vec<SnapshotInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GameVault {
    pub game_id: String,
    pub game_name: String,
    pub files: Vec<FileInfo>,
    pub last_change_at: Option<Timestamp>,
    /// The current version of at least one file looks like a reset.
    pub reset_suspected: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FileDiff {
    pub snapshot_id: i64,
    pub previous_id: Option<i64>,
    pub file_name: String,
    pub binary: bool,
    pub changes: Vec<KeyChange>,
    pub total: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RestoreOutcome {
    pub game_id: String,
    pub game_name: String,
    pub path: String,
    pub label: String,
    pub restored_from: Timestamp,
    /// Whether the previous on-disk version was saved first.
    pub backed_up: bool,
    /// For personal-only restores: number of settings taken over.
    pub personal_keys: Option<usize>,
}

pub fn file_name_of(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

struct Latest {
    hash: String,
    build: Option<String>,
}

pub struct Vault {
    conn: Connection,
    blobs: PathBuf,
}

impl Vault {
    pub fn open(dir: &Path) -> Result<Vault> {
        let blobs = dir.join("blobs");
        fs::create_dir_all(&blobs)?;
        let conn = Connection::open(dir.join("vault.db"))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(SCHEMA)?;
        Ok(Vault { conn, blobs })
    }

    fn blob_path(&self, hash: &str) -> PathBuf {
        self.blobs.join(&hash[..2]).join(hash)
    }

    fn put_blob(&self, hash: &str, bytes: &[u8]) -> Result<()> {
        let path = self.blob_path(hash);
        if path.is_file() {
            return Ok(());
        }
        let parent = path.parent().expect("blob path has a parent");
        fs::create_dir_all(parent)?;
        let tmp = parent.join(format!("{hash}.tmp"));
        fs::write(&tmp, bytes)?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }

    fn read_blob(&self, hash: &str) -> Result<Vec<u8>> {
        fs::read(self.blob_path(hash)).map_err(|e| Error::Other(crate::tr!("Sicherung {hash} fehlt: {e}", "Backup {hash} is missing: {e}")))
    }

    fn file_id(&self, f: &TrackedFile) -> Result<i64> {
        let path = f.path.to_string_lossy();
        self.conn.execute(
            "INSERT INTO files (game_id, game_name, label, path) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(path) DO UPDATE SET game_name = excluded.game_name, label = excluded.label",
            params![f.game_id, f.game_name, f.label, path],
        )?;
        Ok(self.conn.query_row("SELECT id FROM files WHERE path = ?1", [path], |r| r.get(0))?)
    }

    fn latest(&self, file_id: i64) -> Result<Option<Latest>> {
        Ok(self
            .conn
            .query_row(
                "SELECT hash, game_build FROM snapshots WHERE file_id = ?1 ORDER BY id DESC LIMIT 1",
                [file_id],
                |r| Ok(Latest { hash: r.get(0)?, build: r.get(1)? }),
            )
            .optional()?)
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_snapshot(
        &self,
        file_id: i64,
        now: Timestamp,
        hash: &str,
        size: usize,
        build: Option<&str>,
        reason: Reason,
        s: &DiffSummary,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO snapshots (file_id, taken_at, hash, size, game_build, reason,
                                    changed, total, is_binary, build_changed, reset_likely)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                file_id,
                now,
                hash,
                size as i64,
                build,
                reason.as_str(),
                s.changed as i64,
                s.total as i64,
                s.binary,
                s.build_changed,
                s.reset_likely
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    fn prune(&self, file_id: i64) -> Result<()> {
        let keep = "SELECT id FROM snapshots WHERE file_id = ?1 ORDER BY id DESC LIMIT ?2";
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT DISTINCT hash FROM snapshots WHERE file_id = ?1 AND id NOT IN ({keep})"))?;
        let hashes: Vec<String> = stmt
            .query_map(params![file_id, KEEP_SNAPSHOTS], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        if hashes.is_empty() {
            return Ok(());
        }
        self.conn.execute(
            &format!("DELETE FROM snapshots WHERE file_id = ?1 AND id NOT IN ({keep})"),
            params![file_id, KEEP_SNAPSHOTS],
        )?;
        for hash in hashes {
            let used: bool = self
                .conn
                .query_row("SELECT 1 FROM snapshots WHERE hash = ?1 LIMIT 1", [&hash], |_| Ok(()))
                .optional()?
                .is_some();
            if !used {
                let _ = fs::remove_file(self.blob_path(&hash));
            }
        }
        Ok(())
    }

    /// Snapshot every tracked file that changed since the last check.
    /// `builds` maps game ids to their current build id.
    pub fn check(
        &mut self,
        files: &[TrackedFile],
        builds: &HashMap<String, String>,
        now: Timestamp,
    ) -> Result<Vec<VaultEvent>> {
        let mut events = Vec::new();
        for f in files {
            let size = match fs::metadata(&f.path) {
                Ok(m) if m.is_file() => m.len(),
                _ => continue, // deleted or not yet created: keep history as is
            };
            if size > MAX_FILE_BYTES {
                tracing::debug!(file = %f.path.display(), "too large for the vault");
                continue;
            }
            let bytes = match fs::read(&f.path) {
                Ok(b) => b,
                Err(e) => {
                    // Some games keep their config locked while running.
                    tracing::warn!(file = %f.path.display(), error = %e, "config file unreadable");
                    continue;
                }
            };
            let hash = sha256_hex(&bytes);
            let file_id = self.file_id(f)?;
            let build = builds.get(&f.game_id).map(String::as_str);
            match self.latest(file_id)? {
                None => {
                    self.put_blob(&hash, &bytes)?;
                    self.insert_snapshot(file_id, now, &hash, bytes.len(), build, Reason::Baseline, &DiffSummary::default())?;
                }
                Some(last) if last.hash == hash => {}
                Some(last) => {
                    self.put_blob(&hash, &bytes)?;
                    let old = self.read_blob(&last.hash).unwrap_or_default();
                    let build_changed = matches!((last.build.as_deref(), build), (Some(a), Some(b)) if a != b);
                    let file_name = file_name_of(&f.path.to_string_lossy());
                    let summary = summarize(&file_name, &old, &bytes, build_changed);
                    let id = self.insert_snapshot(file_id, now, &hash, bytes.len(), build, Reason::Changed, &summary)?;
                    self.prune(file_id)?;
                    // Pure formatting changes (same settings) are stored silently.
                    if summary.binary || summary.changed > 0 {
                        events.push(VaultEvent {
                            game_id: f.game_id.clone(),
                            game_name: f.game_name.clone(),
                            file_name: f.label.clone(),
                            snapshot_id: id,
                            summary,
                        });
                    }
                }
            }
        }
        Ok(events)
    }

    fn snapshots_of(&self, file_id: i64) -> Result<Vec<SnapshotInfo>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, taken_at, reason, size, game_build, changed, total, is_binary, build_changed, reset_likely
             FROM snapshots WHERE file_id = ?1 ORDER BY id DESC",
        )?;
        let mut list: Vec<SnapshotInfo> = stmt
            .query_map([file_id], |r| {
                Ok(SnapshotInfo {
                    id: r.get(0)?,
                    taken_at: r.get(1)?,
                    reason: Reason::parse(&r.get::<_, String>(2)?),
                    size: r.get::<_, i64>(3)? as u64,
                    game_build: r.get(4)?,
                    summary: DiffSummary {
                        changed: r.get::<_, i64>(5)? as usize,
                        total: r.get::<_, i64>(6)? as usize,
                        binary: r.get(7)?,
                        build_changed: r.get(8)?,
                        reset_likely: r.get(9)?,
                    },
                    current: false,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        if let Some(first) = list.first_mut() {
            first.current = true;
        }
        Ok(list)
    }

    /// Everything the vault knows, grouped by game and sorted by name.
    pub fn overview(&self) -> Result<Vec<GameVault>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, game_id, game_name, path, label FROM files ORDER BY game_name COLLATE NOCASE, label, path")?;
        let rows: Vec<(i64, String, String, String, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let mut games: Vec<GameVault> = Vec::new();
        for (file_id, game_id, game_name, path, label) in rows {
            let snapshots = self.snapshots_of(file_id)?;
            let file = FileInfo {
                file_id,
                label,
                file_name: file_name_of(&path),
                exists: Path::new(&path).is_file(),
                path,
                snapshots,
            };
            let idx = match games.iter().position(|g| g.game_id == game_id) {
                Some(i) => i,
                None => {
                    games.push(GameVault { game_id, game_name, files: Vec::new(), last_change_at: None, reset_suspected: false });
                    games.len() - 1
                }
            };
            let game = &mut games[idx];
            if let Some(latest) = file.snapshots.first() {
                if latest.reason == Reason::Changed {
                    game.last_change_at = game.last_change_at.max(Some(latest.taken_at));
                    game.reset_suspected |= latest.summary.reset_likely;
                }
            }
            game.files.push(file);
        }
        Ok(games)
    }

    /// Setting-level differences between a snapshot and its predecessor.
    pub fn diff_with_previous(&self, snapshot_id: i64) -> Result<FileDiff> {
        let (file_id, hash, path): (i64, String, String) = self
            .conn
            .query_row(
                "SELECT s.file_id, s.hash, f.path FROM snapshots s JOIN files f ON f.id = s.file_id WHERE s.id = ?1",
                [snapshot_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?
            .ok_or_else(|| Error::Other(crate::tr!("Sicherung #{snapshot_id} existiert nicht", "Backup #{snapshot_id} does not exist")))?;
        let previous: Option<(i64, String)> = self
            .conn
            .query_row(
                "SELECT id, hash FROM snapshots WHERE file_id = ?1 AND id < ?2 ORDER BY id DESC LIMIT 1",
                params![file_id, snapshot_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let file_name = file_name_of(&path);
        let new = parse::parse(&self.read_blob(&hash)?, &file_name);
        let old = match &previous {
            Some((_, h)) => parse::parse(&self.read_blob(h)?, &file_name),
            None => Content::Text(Default::default()),
        };
        let (changes, total, binary) = match (old, new) {
            (Content::Text(a), Content::Text(b)) => (parse::diff(&a, &b), parse::union_len(&a, &b), false),
            _ => (Vec::new(), 0, true),
        };
        Ok(FileDiff { snapshot_id, previous_id: previous.map(|p| p.0), file_name, binary, changes, total })
    }

    /// Game id and name a snapshot belongs to.
    pub fn snapshot_owner(&self, snapshot_id: i64) -> Result<(String, String)> {
        self.conn
            .query_row(
                "SELECT f.game_id, f.game_name FROM snapshots s JOIN files f ON f.id = s.file_id WHERE s.id = ?1",
                [snapshot_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| Error::Other(crate::tr!("Sicherung #{snapshot_id} existiert nicht", "Backup #{snapshot_id} does not exist")))
    }

    fn snapshot_row(&self, snapshot_id: i64) -> Result<SnapRow> {
        self.conn
            .query_row(
                "SELECT s.file_id, s.hash, s.taken_at, f.game_id, f.game_name, f.path, f.label
                 FROM snapshots s JOIN files f ON f.id = s.file_id WHERE s.id = ?1",
                [snapshot_id],
                |r| {
                    Ok(SnapRow {
                        file_id: r.get(0)?,
                        hash: r.get(1)?,
                        taken_at: r.get(2)?,
                        game_id: r.get(3)?,
                        game_name: r.get(4)?,
                        path: r.get(5)?,
                        label: r.get(6)?,
                    })
                },
            )
            .optional()?
            .ok_or_else(|| Error::Other(crate::tr!("Sicherung #{snapshot_id} existiert nicht", "Backup #{snapshot_id} does not exist")))
    }

    /// Write new content to a tracked file. The version currently on disk is
    /// saved first if the vault does not have it yet. Returns whether such a
    /// backup was made.
    fn write_version(&mut self, file_id: i64, target: &Path, bytes: &[u8], now: Timestamp) -> Result<bool> {
        if let Ok(meta) = fs::metadata(target) {
            if meta.permissions().readonly() {
                let path = target.display();
                return Err(Error::Other(crate::tr!(
                    "{path} ist schreibgeschützt. Den Schreibschutz in den Dateieigenschaften entfernen und erneut versuchen.",
                    "{path} is read-only. Remove the read-only attribute in the file properties and try again."
                )));
            }
        }
        let latest = self.latest(file_id)?;
        let build = latest.as_ref().and_then(|l| l.build.clone());
        let mut backed_up = false;
        if let Ok(current) = fs::read(target) {
            let current_hash = sha256_hex(&current);
            if latest.as_ref().is_none_or(|l| l.hash != current_hash) {
                self.put_blob(&current_hash, &current)?;
                self.insert_snapshot(file_id, now, &current_hash, current.len(), build.as_deref(), Reason::BeforeRestore, &DiffSummary::default())?;
                backed_up = true;
            }
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut tmp = target.to_path_buf().into_os_string();
        tmp.push(".blackbox-tmp");
        let tmp = PathBuf::from(tmp);
        fs::write(&tmp, bytes)?;
        if let Err(e) = fs::rename(&tmp, target) {
            let _ = fs::remove_file(&tmp);
            return Err(e.into());
        }
        let hash = sha256_hex(bytes);
        self.put_blob(&hash, bytes)?;
        self.insert_snapshot(file_id, now, &hash, bytes.len(), build.as_deref(), Reason::Restored, &DiffSummary::default())?;
        self.prune(file_id)?;
        Ok(backed_up)
    }

    /// Write a snapshot back to its file. The caller must make sure the game
    /// is not running (it would overwrite the file again on exit).
    pub fn restore(&mut self, snapshot_id: i64, now: Timestamp) -> Result<RestoreOutcome> {
        let row = self.snapshot_row(snapshot_id)?;
        let bytes = self.read_blob(&row.hash)?;
        let target = PathBuf::from(&row.path);
        let backed_up = self.write_version(row.file_id, &target, &bytes, now)?;
        tracing::info!(file = %target.display(), snapshot_id, "config restored");
        Ok(row.outcome(backed_up, None))
    }

    /// Take only the personal settings (controls, sensitivity, audio,
    /// gameplay) from a snapshot; display and graphics settings of the
    /// current file stay as they are.
    pub fn restore_personal(&mut self, snapshot_id: i64, now: Timestamp) -> Result<RestoreOutcome> {
        let row = self.snapshot_row(snapshot_id)?;
        let label = row.label.clone();
        if !selective::supports_selective(&file_name_of(&row.path)) {
            return Err(Error::Other(crate::tr!("Für {label} ist nur die vollständige Wiederherstellung möglich.", "{label} can only be restored completely.")));
        }
        let target = PathBuf::from(&row.path);
        let current = fs::read(&target)
            .map_err(|e| {
                let path = target.display();
                Error::Other(crate::tr!("{path} ist nicht lesbar: {e}", "{path} cannot be read: {e}"))
            })?;
        let old = self.read_blob(&row.hash)?;
        let merged = selective::merge_personal(&current, &old, &file_name_of(&row.path))
            .ok_or_else(|| Error::Other(crate::tr!("{label} konnte nicht ausgewertet werden.", "{label} could not be evaluated.")))?;
        if merged.changed.is_empty() {
            return Err(Error::Other(
                crate::i18n::t("Die persönlichen Einstellungen stimmen bereits überein.", "The personal settings already match.").into(),
            ));
        }
        let backed_up = self.write_version(row.file_id, &target, &merged.bytes, now)?;
        tracing::info!(file = %target.display(), snapshot_id, keys = merged.changed.len(), "personal settings restored");
        Ok(row.outcome(backed_up, Some(merged.changed.len())))
    }

    /// `(file id, game id, game name, path, label)` of all tracked files.
    #[allow(clippy::type_complexity)]
    fn file_rows(&self) -> Result<Vec<(i64, String, String, String, String)>> {
        let mut stmt = self.conn.prepare("SELECT id, game_id, game_name, path, label FROM files ORDER BY game_name, label")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// Current settings of the given games (all if empty) for moving them to
    /// another PC. Only text formats that can be merged are included.
    pub fn export_profile(&self, game_ids: &[String], now: Timestamp) -> Result<Profile> {
        let mut games: Vec<ProfileGame> = Vec::new();
        for (_, game_id, game_name, path, label) in self.file_rows()? {
            if !game_ids.is_empty() && !game_ids.contains(&game_id) {
                continue;
            }
            let file_name = file_name_of(&path);
            if !selective::supports_selective(&file_name) {
                continue;
            }
            let Some(content) = fs::read(&path).ok().and_then(|b| parse::decode(&b)) else { continue };
            let file = ProfileFile { file_name, label, content };
            match games.iter_mut().find(|g| g.game_id == game_id) {
                // With several Steam accounts the same file exists more than
                // once; the first one is exported.
                Some(g) if g.files.iter().any(|f| f.file_name.eq_ignore_ascii_case(&file.file_name)) => {}
                Some(g) => g.files.push(file),
                None => games.push(ProfileGame { game_id, game_name, files: vec![file] }),
            }
        }
        Ok(Profile { format: PROFILE_FORMAT.into(), version: 1, exported_at: now, games })
    }

    /// Apply the personal settings of a profile to the matching local files
    /// (same game, same file name). With `apply == false` nothing is written
    /// and the result is a preview.
    pub fn import_profile(&mut self, profile: &Profile, apply: bool, now: Timestamp) -> Result<Vec<ImportItem>> {
        if profile.format != PROFILE_FORMAT {
            return Err(Error::Other(crate::i18n::t("Die Datei ist kein Blackbox-Profil.", "The file is not a Blackbox profile.").into()));
        }
        let rows = self.file_rows()?;
        let mut items = Vec::new();
        for game in &profile.games {
            for pf in &game.files {
                let candidates: Vec<_> = rows
                    .iter()
                    .filter(|r| r.1 == game.game_id && file_name_of(&r.3).eq_ignore_ascii_case(&pf.file_name))
                    .collect();
                // Same account (same label) first; otherwise only an
                // unambiguous single file. With several Steam accounts the
                // settings must never land in another account's files.
                let same_label: Vec<_> = candidates.iter().filter(|r| r.4 == pf.label).copied().collect();
                let target = match (same_label.as_slice(), candidates.as_slice()) {
                    ([one], _) => Some(*one),
                    ([], [one]) => Some(*one),
                    _ => None,
                };
                let Some((file_id, game_id, game_name, path, label)) = target else {
                    if candidates.len() > 1 {
                        items.push(ImportItem {
                            game_id: game.game_id.clone(),
                            game_name: game.game_name.clone(),
                            label: pf.label.clone(),
                            changed: Vec::new(),
                            applied: false,
                            skipped: Some({
                                let n = candidates.len();
                                crate::tr!(
                                    "{n} passende Dateien mehrerer Steam-Konten – Ziel nicht eindeutig",
                                    "{n} matching files of several Steam accounts – target is ambiguous"
                                )
                            }),
                        });
                    }
                    continue;
                };
                let Ok(current) = fs::read(path) else { continue };
                let Some(merged) = selective::merge_personal(&current, pf.content.as_bytes(), &pf.file_name) else {
                    continue;
                };
                let applied = apply && !merged.changed.is_empty();
                if applied {
                    self.write_version(*file_id, Path::new(path), &merged.bytes, now)?;
                }
                items.push(ImportItem {
                    game_id: game_id.clone(),
                    game_name: game_name.clone(),
                    label: label.clone(),
                    changed: merged.changed,
                    applied,
                    skipped: None,
                });
            }
        }        Ok(items)
    }
}

struct SnapRow {
    file_id: i64,
    hash: String,
    taken_at: Timestamp,
    game_id: String,
    game_name: String,
    path: String,
    label: String,
}

impl SnapRow {
    fn outcome(self, backed_up: bool, personal_keys: Option<usize>) -> RestoreOutcome {
        RestoreOutcome {
            game_id: self.game_id,
            game_name: self.game_name,
            path: self.path,
            label: self.label,
            restored_from: self.taken_at,
            backed_up,
            personal_keys,
        }
    }
}

pub const PROFILE_FORMAT: &str = "blackbox-profile";

/// Portable settings for moving to another PC.
#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
pub struct Profile {
    pub format: String,
    pub version: u32,
    pub exported_at: Timestamp,
    pub games: Vec<ProfileGame>,
}

#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
pub struct ProfileGame {
    pub game_id: String,
    pub game_name: String,
    pub files: Vec<ProfileFile>,
}

#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
pub struct ProfileFile {
    pub file_name: String,
    /// Label on the exporting PC (names the Steam account where needed).
    #[serde(default)]
    pub label: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ImportItem {
    pub game_id: String,
    pub game_name: String,
    pub label: String,
    /// Settings that differ and are (or would be) taken over.
    pub changed: Vec<String>,
    pub applied: bool,
    /// Why the file was left alone, if it was.
    pub skipped: Option<String>,
}
#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        _dir: tempfile::TempDir,
        vault: Vault,
        file: TrackedFile,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::open(&dir.path().join("vault")).unwrap();
        let file = TrackedFile {
            game_id: "steam:1".into(),
            game_name: "Game".into(),
            path: dir.path().join("cfg").join("GameUserSettings.ini"),
            label: "GameUserSettings.ini".into(),
        };
        fs::create_dir_all(file.path.parent().unwrap()).unwrap();
        Fixture { _dir: dir, vault, file }
    }

    fn builds(b: &str) -> HashMap<String, String> {
        HashMap::from([("steam:1".to_string(), b.to_string())])
    }

    fn settings(values: &[u32]) -> String {
        values.iter().enumerate().map(|(i, v)| format!("k{i}={v}\n")).collect()
    }

    #[test]
    fn baseline_then_change_then_unchanged() {
        let mut f = fixture();
        fs::write(&f.file.path, settings(&[1; 10])).unwrap();
        assert!(f.vault.check(&[f.file.clone()], &builds("1"), 100).unwrap().is_empty());

        let mut v = [1; 10];
        v[3] = 2;
        fs::write(&f.file.path, settings(&v)).unwrap();
        let ev = f.vault.check(&[f.file.clone()], &builds("1"), 200).unwrap();
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].summary.changed, 1);
        assert!(!ev[0].summary.reset_likely);
        assert_eq!(ev[0].summary.describe(), "1 of 10 settings changed");

        assert!(f.vault.check(&[f.file.clone()], &builds("1"), 300).unwrap().is_empty());

        let overview = f.vault.overview().unwrap();
        assert_eq!(overview.len(), 1);
        let snaps = &overview[0].files[0].snapshots;
        assert_eq!(snaps.len(), 2);
        assert!(snaps[0].current);
        assert_eq!(snaps[1].reason, Reason::Baseline);

        let d = f.vault.diff_with_previous(snaps[0].id).unwrap();
        assert_eq!(d.changes.len(), 1);
        assert_eq!(d.changes[0].key, "k3");
        assert_eq!(d.changes[0].class, crate::vault::selective::KeyClass::Personal);
        assert_eq!(d.previous_id, Some(snaps[1].id));
    }

    #[test]
    fn mass_change_after_update_is_flagged_as_reset() {
        let mut f = fixture();
        fs::write(&f.file.path, settings(&[5; 10])).unwrap();
        f.vault.check(&[f.file.clone()], &builds("1"), 100).unwrap();
        fs::write(&f.file.path, settings(&[1, 1, 1, 5, 5, 5, 5, 5, 5, 5])).unwrap();
        let ev = f.vault.check(&[f.file.clone()], &builds("2"), 200).unwrap();
        assert!(ev[0].summary.build_changed);
        assert!(ev[0].summary.reset_likely);
        assert!(ev[0].summary.describe().contains("reset after a game update"));
        assert!(f.vault.overview().unwrap()[0].reset_suspected);
    }

    #[test]
    fn formatting_only_change_is_silent() {
        let mut f = fixture();
        fs::write(&f.file.path, "a=1\nb=2\n").unwrap();
        f.vault.check(&[f.file.clone()], &builds("1"), 100).unwrap();
        fs::write(&f.file.path, "a = 1\r\n\r\nb = 2\r\n").unwrap();
        assert!(f.vault.check(&[f.file.clone()], &builds("1"), 200).unwrap().is_empty());
    }

    #[test]
    fn restore_backs_up_current_version_and_is_undoable() {
        let mut f = fixture();
        fs::write(&f.file.path, "quality=epic\n").unwrap();
        f.vault.check(&[f.file.clone()], &builds("1"), 100).unwrap();
        let good = f.vault.overview().unwrap()[0].files[0].snapshots[0].id;

        // The game resets the file, Blackbox has not checked yet.
        fs::write(&f.file.path, "quality=low\n").unwrap();
        let out = f.vault.restore(good, 200).unwrap();
        assert!(out.backed_up);
        assert_eq!(fs::read_to_string(&f.file.path).unwrap(), "quality=epic\n");

        let snaps = f.vault.overview().unwrap()[0].files[0].snapshots.clone();
        let reasons: Vec<_> = snaps.iter().map(|s| s.reason).collect();
        assert_eq!(reasons, vec![Reason::Restored, Reason::BeforeRestore, Reason::Baseline]);
        // Restoring is silent for the next check.
        assert!(f.vault.check(&[f.file.clone()], &builds("1"), 300).unwrap().is_empty());

        // Undo: restore the automatic backup.
        f.vault.restore(snaps[1].id, 400).unwrap();
        assert_eq!(fs::read_to_string(&f.file.path).unwrap(), "quality=low\n");
    }

    #[test]
    fn personal_restore_keeps_display_and_graphics() {
        let mut f = fixture();
        fs::write(&f.file.path, "[S]\nMouseSensitivity=1.5\nResolutionSizeX=1920\nsg.ShadowQuality=1\n").unwrap();
        f.vault.check(&[f.file.clone()], &builds("1"), 100).unwrap();
        let old = f.vault.overview().unwrap()[0].files[0].snapshots[0].id;
        // New PC / patch: everything differs now.
        fs::write(&f.file.path, "[S]\nMouseSensitivity=0.4\nResolutionSizeX=3840\nsg.ShadowQuality=3\n").unwrap();
        f.vault.check(&[f.file.clone()], &builds("2"), 200).unwrap();

        let out = f.vault.restore_personal(old, 300).unwrap();
        assert_eq!(out.personal_keys, Some(1));
        assert!(!out.backed_up, "current version was already in the vault");
        assert_eq!(
            fs::read_to_string(&f.file.path).unwrap(),
            "[S]\nMouseSensitivity=1.5\nResolutionSizeX=3840\nsg.ShadowQuality=3\n"
        );
        let err = f.vault.restore_personal(old, 400).unwrap_err();
        assert!(err.to_string().contains("already match"));
    }

    #[test]
    fn profile_export_and_import_between_pcs() {
        let mut pc1 = fixture();
        fs::write(&pc1.file.path, "[S]\nMouseSensitivity=2\nResolutionSizeX=1920\n").unwrap();
        pc1.vault.check(&[pc1.file.clone()], &builds("1"), 100).unwrap();
        let profile = pc1.vault.export_profile(&[], 150).unwrap();
        assert_eq!(profile.games.len(), 1);
        assert_eq!(profile.games[0].files[0].file_name, "GameUserSettings.ini");
        assert!(pc1.vault.export_profile(&["steam:99".into()], 150).unwrap().games.is_empty());

        // Round trip through JSON as the file on disk would.
        let json = serde_json::to_string(&profile).unwrap();
        let profile: Profile = serde_json::from_str(&json).unwrap();

        let mut pc2 = fixture();
        fs::write(&pc2.file.path, "[S]\nMouseSensitivity=1\nResolutionSizeX=2560\n").unwrap();
        pc2.vault.check(&[pc2.file.clone()], &builds("1"), 100).unwrap();
        let preview = pc2.vault.import_profile(&profile, false, 200).unwrap();
        assert_eq!(preview.len(), 1);
        assert_eq!(preview[0].changed, vec!["[S] MouseSensitivity"]);
        assert!(!preview[0].applied);
        assert!(fs::read_to_string(&pc2.file.path).unwrap().contains("MouseSensitivity=1\n"));

        let done = pc2.vault.import_profile(&profile, true, 300).unwrap();
        assert!(done[0].applied);
        assert_eq!(fs::read_to_string(&pc2.file.path).unwrap(), "[S]\nMouseSensitivity=2\nResolutionSizeX=2560\n");

        // Several accounts on the target PC: an unlabelled match is ambiguous,
        // a labelled one goes only to the same account.
        let other = TrackedFile { label: "GameUserSettings.ini · B".into(), path: pc2.file.path.with_file_name("b").join("GameUserSettings.ini"), ..pc2.file.clone() };
        fs::create_dir_all(other.path.parent().unwrap()).unwrap();
        fs::write(&other.path, "[S]\nMouseSensitivity=9\n").unwrap();
        pc2.vault.check(std::slice::from_ref(&other), &builds("1"), 310).unwrap();
        let mut foreign = profile.clone();
        foreign.games[0].files[0].label = "GameUserSettings.ini · A".into();
        let items = pc2.vault.import_profile(&foreign, true, 320).unwrap();
        assert_eq!(items.len(), 1);
        assert!(items[0].skipped.as_deref().unwrap().contains("ambiguous"));
        assert!(fs::read_to_string(&other.path).unwrap().contains("MouseSensitivity=9"));
        let mut same = profile.clone();
        same.games[0].files[0].label = "GameUserSettings.ini · B".into();
        let items = pc2.vault.import_profile(&same, true, 330).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "GameUserSettings.ini · B");
        assert_eq!(fs::read_to_string(&other.path).unwrap(), "[S]\nMouseSensitivity=2\n");

        let mut wrong = profile.clone();
        wrong.format = "x".into();
        assert!(pc2.vault.import_profile(&wrong, false, 400).is_err());
    }

    #[test]
    fn restore_refuses_read_only_files() {
        let mut f = fixture();
        fs::write(&f.file.path, "a=1\n").unwrap();
        f.vault.check(&[f.file.clone()], &builds("1"), 100).unwrap();
        let id = f.vault.overview().unwrap()[0].files[0].snapshots[0].id;
        let mut perms = fs::metadata(&f.file.path).unwrap().permissions();
        perms.set_readonly(true);
        fs::set_permissions(&f.file.path, perms.clone()).unwrap();
        let err = f.vault.restore(id, 200).unwrap_err();
        assert!(err.to_string().contains("read-only"));
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        fs::set_permissions(&f.file.path, perms).unwrap();
    }

    #[test]
    fn old_snapshots_are_pruned_with_their_blobs() {
        let mut f = fixture();
        for i in 0..(KEEP_SNAPSHOTS + 5) {
            fs::write(&f.file.path, format!("v={i}\n")).unwrap();
            f.vault.check(&[f.file.clone()], &builds("1"), i).unwrap();
        }
        let snaps = &f.vault.overview().unwrap()[0].files[0].snapshots;
        assert_eq!(snaps.len() as i64, KEEP_SNAPSHOTS);
        let blobs = walk_count(&f.vault.blobs);
        assert_eq!(blobs as i64, KEEP_SNAPSHOTS);
    }

    fn walk_count(dir: &Path) -> usize {
        fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| if e.path().is_dir() { walk_count(&e.path()) } else { 1 })
            .sum()
    }

    #[test]
    fn missing_and_oversized_files_are_skipped() {
        let mut f = fixture();
        assert!(f.vault.check(&[f.file.clone()], &builds("1"), 100).unwrap().is_empty());
        fs::write(&f.file.path, vec![b'a'; MAX_FILE_BYTES as usize + 1]).unwrap();
        f.vault.check(&[f.file.clone()], &builds("1"), 200).unwrap();
        assert!(f.vault.overview().unwrap().is_empty());
    }
}
