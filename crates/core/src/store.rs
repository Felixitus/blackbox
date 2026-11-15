//! SQLite persistence: current system state, change journal and sessions.

use std::collections::HashMap;
use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::error::Result;
use crate::load::ProcessLoad;
use crate::model::{
    Category, ChangeEvent, ChangeKind, Fact, Session, SessionMetrics, Timestamp,
};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS state (
    category TEXT NOT NULL,
    key      TEXT NOT NULL,
    label    TEXT NOT NULL,
    value    TEXT NOT NULL,
    PRIMARY KEY (category, key)
);
CREATE TABLE IF NOT EXISTS baseline (
    category TEXT PRIMARY KEY,
    taken_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS changes (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    detected_at INTEGER NOT NULL,
    category    TEXT NOT NULL,
    key         TEXT NOT NULL,
    label       TEXT NOT NULL,
    kind        TEXT NOT NULL,
    old_value   TEXT,
    new_value   TEXT
);
CREATE INDEX IF NOT EXISTS idx_changes_time ON changes(detected_at);
CREATE TABLE IF NOT EXISTS sessions (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    game_id       TEXT NOT NULL,
    game_name     TEXT NOT NULL,
    started_at    INTEGER NOT NULL,
    ended_at      INTEGER,
    frames        INTEGER,
    avg_fps       REAL,
    p1_low_fps    REAL,
    p01_low_fps   REAL,
    median_ft_ms  REAL,
    stutters_min  REAL,
    capture_note  TEXT
);
CREATE INDEX IF NOT EXISTS idx_sessions_game ON sessions(game_id, started_at);
CREATE TABLE IF NOT EXISTS session_processes (
    session_id INTEGER NOT NULL,
    name       TEXT NOT NULL,
    avg_cpu    REAL NOT NULL,
    peak_cpu   REAL NOT NULL,
    PRIMARY KEY (session_id, name)
);
"#;

/// One difference found by a scan, before it gets an id.
struct Diff {
    key: String,
    label: String,
    kind: ChangeKind,
    old_value: Option<String>,
    new_value: Option<String>,
}

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Store> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Store> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Store> {
        conn.execute_batch(SCHEMA)?;
        // Columns added after the first release; existing databases get them here.
        add_column_if_missing(&conn, "sessions", "excluded", "INTEGER NOT NULL DEFAULT 0")?;
        add_column_if_missing(&conn, "sessions", "load_samples", "INTEGER")?;
        migrate_value_codes(&conn)?;
        Ok(Store { conn })
    }

    // ---- state & change journal -------------------------------------------------

    /// Replace the stored state of `category` with `facts` and journal every
    /// difference. The very first scan of a category only establishes the
    /// baseline and produces no change events.
    pub fn apply_scan(
        &mut self,
        category: Category,
        facts: &[Fact],
        now: Timestamp,
    ) -> Result<Vec<ChangeEvent>> {
        let tx = self.conn.transaction()?;
        let cat = category.as_str();

        let has_baseline: bool = tx
            .query_row(
                "SELECT 1 FROM baseline WHERE category = ?1",
                [cat],
                |_| Ok(()),
            )
            .optional()?
            .is_some();

        let mut old: HashMap<String, (String, String)> = HashMap::new();
        {
            let mut stmt = tx.prepare("SELECT key, label, value FROM state WHERE category = ?1")?;
            let rows = stmt.query_map([cat], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
            })?;
            for row in rows {
                let (k, l, v) = row?;
                old.insert(k, (l, v));
            }
        }

        let mut diffs: Vec<Diff> = Vec::new();
        for f in facts {
            debug_assert_eq!(f.category, category);
            match old.remove(&f.key) {
                None => diffs.push(Diff {
                    key: f.key.clone(),
                    label: f.label.clone(),
                    kind: ChangeKind::Added,
                    old_value: None,
                    new_value: Some(f.value.clone()),
                }),
                Some((_, old_value)) if old_value != f.value => diffs.push(Diff {
                    key: f.key.clone(),
                    label: f.label.clone(),
                    kind: ChangeKind::Modified,
                    old_value: Some(old_value),
                    new_value: Some(f.value.clone()),
                }),
                Some(_) => {}
            }
        }
        for (key, (label, value)) in old {
            diffs.push(Diff {
                key,
                label,
                kind: ChangeKind::Removed,
                old_value: Some(value),
                new_value: None,
            });
        }
        diffs.sort_by(|a, b| a.key.cmp(&b.key));

        tx.execute("DELETE FROM state WHERE category = ?1", [cat])?;
        {
            let mut ins = tx.prepare(
                "INSERT OR REPLACE INTO state (category, key, label, value) VALUES (?1, ?2, ?3, ?4)",
            )?;
            for f in facts {
                ins.execute(params![cat, f.key, f.label, f.value])?;
            }
        }

        let mut events = Vec::new();
        if has_baseline {
            let mut ins = tx.prepare(
                "INSERT INTO changes (detected_at, category, key, label, kind, old_value, new_value)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            for Diff { key, label, kind, old_value, new_value } in diffs {
                ins.execute(params![now, cat, key, label, kind.as_str(), old_value, new_value])?;
                events.push(ChangeEvent {
                    id: tx.last_insert_rowid(),
                    detected_at: now,
                    category,
                    key,
                    label,
                    kind,
                    old_value,
                    new_value,
                });
            }
        } else {
            tx.execute(
                "INSERT INTO baseline (category, taken_at) VALUES (?1, ?2)",
                params![cat, now],
            )?;
        }
        tx.commit()?;
        Ok(events)
    }

    /// Journal a change that was not found by comparing scans (e.g. a config
    /// file change detected by the Loadout Vault).
    #[allow(clippy::too_many_arguments)]
    pub fn record_change(
        &self,
        category: Category,
        key: &str,
        label: &str,
        kind: ChangeKind,
        old_value: Option<&str>,
        new_value: Option<&str>,
        now: Timestamp,
    ) -> Result<ChangeEvent> {
        self.conn.execute(
            "INSERT INTO changes (detected_at, category, key, label, kind, old_value, new_value)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![now, category.as_str(), key, label, kind.as_str(), old_value, new_value],
        )?;
        Ok(ChangeEvent {
            id: self.conn.last_insert_rowid(),
            detected_at: now,
            category,
            key: key.to_string(),
            label: label.to_string(),
            kind,
            old_value: old_value.map(str::to_string),
            new_value: new_value.map(str::to_string),
        })
    }

    pub fn current_state(&self) -> Result<Vec<Fact>> {
        let mut stmt = self
            .conn
            .prepare("SELECT category, key, label, value FROM state ORDER BY category, label")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (c, key, label, value) = row?;
            if let Some(category) = Category::parse(&c) {
                out.push(Fact { category, key, label, value });
            }
        }
        Ok(out)
    }

    /// Changes detected in the half-open interval `(after, until]`, oldest first.
    pub fn changes_between(&self, after: Timestamp, until: Timestamp) -> Result<Vec<ChangeEvent>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, detected_at, category, key, label, kind, old_value, new_value
             FROM changes WHERE detected_at > ?1 AND detected_at <= ?2
             ORDER BY detected_at, id",
        )?;
        let rows = stmt.query_map(params![after, until], change_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?.into_iter().flatten().collect())
    }

    // ---- sessions ---------------------------------------------------------------

    pub fn start_session(
        &self,
        game_id: &str,
        game_name: &str,
        started_at: Timestamp,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO sessions (game_id, game_name, started_at) VALUES (?1, ?2, ?3)",
            params![game_id, game_name, started_at],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn finish_session(
        &self,
        id: i64,
        ended_at: Timestamp,
        metrics: Option<&SessionMetrics>,
        capture_note: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET ended_at = ?2, frames = ?3, avg_fps = ?4, p1_low_fps = ?5,
                 p01_low_fps = ?6, median_ft_ms = ?7, stutters_min = ?8, capture_note = ?9
             WHERE id = ?1",
            params![
                id,
                ended_at,
                metrics.map(|m| m.frames as i64),
                metrics.map(|m| m.avg_fps),
                metrics.map(|m| m.p1_low_fps),
                metrics.map(|m| m.p01_low_fps),
                metrics.map(|m| m.median_frametime_ms),
                metrics.map(|m| m.stutters_per_min),
                capture_note,
            ],
        )?;
        Ok(())
    }

    /// Sessions that were never finished (e.g. the watcher was killed).
    /// They are closed with `ended_at = started_at` so they never count as data.
    pub fn close_dangling_sessions(&self) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE sessions SET ended_at = started_at,
                 capture_note = 'note:interrupted'
             WHERE ended_at IS NULL",
            [],
        )?)
    }

    /// All sessions, optionally limited to one game, oldest first.
    pub fn sessions(&self, game_id: Option<&str>) -> Result<Vec<Session>> {
        let sql = "SELECT id, game_id, game_name, started_at, ended_at, frames, avg_fps,
                          p1_low_fps, p01_low_fps, median_ft_ms, stutters_min, capture_note,
                          excluded, load_samples
                   FROM sessions WHERE (?1 IS NULL OR game_id = ?1)
                   ORDER BY started_at, id";
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(params![game_id], session_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// One session by id.
    pub fn session(&self, id: i64) -> Result<Option<Session>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, game_id, game_name, started_at, ended_at, frames, avg_fps,
                        p1_low_fps, p01_low_fps, median_ft_ms, stutters_min, capture_note,
                        excluded, load_samples
                 FROM sessions WHERE id = ?1",
                [id],
                session_from_row,
            )
            .optional()?)
    }

    /// Mark a session as (not) representative. Returns `false` if it does not exist.
    pub fn set_excluded(&self, id: i64, excluded: bool) -> Result<bool> {
        Ok(self.conn.execute("UPDATE sessions SET excluded = ?2 WHERE id = ?1", params![id, excluded])? > 0)
    }

    /// Store the background load measured during a session.
    pub fn save_session_load(&self, id: i64, samples: u32, processes: &[ProcessLoad]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("UPDATE sessions SET load_samples = ?2 WHERE id = ?1", params![id, samples])?;
        tx.execute("DELETE FROM session_processes WHERE session_id = ?1", [id])?;
        {
            let mut ins = tx.prepare(
                "INSERT OR REPLACE INTO session_processes (session_id, name, avg_cpu, peak_cpu) VALUES (?1, ?2, ?3, ?4)",
            )?;
            for p in processes {
                ins.execute(params![id, p.name, p.avg_cpu, p.peak_cpu])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Background load of a session, highest first.
    pub fn session_load(&self, id: i64) -> Result<Vec<ProcessLoad>> {
        let mut stmt = self.conn.prepare(
            "SELECT name, avg_cpu, peak_cpu FROM session_processes WHERE session_id = ?1 ORDER BY avg_cpu DESC, name",
        )?;
        let rows = stmt.query_map([id], |r| Ok(ProcessLoad { name: r.get(0)?, avg_cpu: r.get(1)?, peak_cpu: r.get(2)? }))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Distinct `(game_id, game_name)` pairs that have at least one session.
    pub fn games_with_sessions(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT game_id, MAX(game_name) FROM sessions GROUP BY game_id ORDER BY 2",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

fn change_from_row(r: &Row<'_>) -> rusqlite::Result<Option<ChangeEvent>> {
    let category = Category::parse(&r.get::<_, String>(2)?);
    let kind = ChangeKind::parse(&r.get::<_, String>(5)?);
    // Rows written by a newer version with unknown categories are skipped.
    let (Some(category), Some(kind)) = (category, kind) else {
        return Ok(None);
    };
    Ok(Some(ChangeEvent {
        id: r.get(0)?,
        detected_at: r.get(1)?,
        category,
        key: r.get(3)?,
        label: r.get(4)?,
        kind,
        old_value: r.get(6)?,
        new_value: r.get(7)?,
    }))
}

fn session_from_row(r: &Row<'_>) -> rusqlite::Result<Session> {
    let frames: Option<i64> = r.get(5)?;
    let metrics = match frames {
        Some(frames) => Some(SessionMetrics {
            frames: frames as u64,
            avg_fps: r.get(6)?,
            p1_low_fps: r.get(7)?,
            p01_low_fps: r.get(8)?,
            median_frametime_ms: r.get(9)?,
            stutters_per_min: r.get(10)?,
        }),
        None => None,
    };
    Ok(Session {
        id: r.get(0)?,
        game_id: r.get(1)?,
        game_name: r.get(2)?,
        started_at: r.get(3)?,
        ended_at: r.get(4)?,
        metrics,
        capture_note: r.get(11)?,
        excluded: r.get(12)?,
        load_samples: r.get::<_, Option<i64>>(13)?.map(|n| n.max(0) as u32),
    })
}

/// Earlier versions stored German words as values; they are language-neutral
/// codes now. Rewriting the stored state keeps the next scan from reporting
/// "an → on" as a change. Old journal entries are rendered as they are.
fn migrate_value_codes(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "UPDATE state SET value = 'on' WHERE category = 'system_setting' AND value = 'an';
         UPDATE state SET value = 'off' WHERE category = 'system_setting' AND value = 'aus';
         UPDATE state SET value = 'default' WHERE category = 'system_setting' AND value = 'Standard';
         UPDATE state SET value = 'unknown:' || substr(value, 12, length(value) - 12)
             WHERE category = 'system_setting' AND value LIKE 'unbekannt (%)';
         UPDATE state SET value = 'installed' WHERE category = 'windows_update' AND value = 'installiert';
         UPDATE state SET value = 'files ' || substr(value, 12)
             WHERE category = 'game_build' AND value LIKE 'Dateistand %';",
    )?;
    Ok(())
}

fn add_column_if_missing(conn: &Connection, table: &str, column: &str, decl: &str) -> Result<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let exists = stmt
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .iter()
        .any(|c| c == column);
    if !exists {
        conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fact(key: &str, value: &str) -> Fact {
        Fact::new(Category::GpuDriver, key, format!("GPU {key}"), value)
    }

    #[test]
    fn first_scan_is_baseline_without_events() {
        let mut s = Store::open_in_memory().unwrap();
        let ev = s.apply_scan(Category::GpuDriver, &[fact("a", "1")], 100).unwrap();
        assert!(ev.is_empty());
        assert_eq!(s.current_state().unwrap().len(), 1);
    }

    #[test]
    fn detects_added_removed_modified() {
        let mut s = Store::open_in_memory().unwrap();
        s.apply_scan(Category::GpuDriver, &[fact("a", "1"), fact("b", "1")], 100).unwrap();
        let ev = s
            .apply_scan(Category::GpuDriver, &[fact("a", "2"), fact("c", "1")], 200)
            .unwrap();
        let kinds: Vec<_> = ev.iter().map(|e| (e.key.as_str(), e.kind)).collect();
        assert_eq!(
            kinds,
            vec![
                ("a", ChangeKind::Modified),
                ("b", ChangeKind::Removed),
                ("c", ChangeKind::Added)
            ]
        );
        assert_eq!(ev[0].old_value.as_deref(), Some("1"));
        assert_eq!(ev[0].new_value.as_deref(), Some("2"));
        assert_eq!(s.changes_between(100, 200).unwrap().len(), 3);
        assert!(s.changes_between(200, 300).unwrap().is_empty());
    }

    #[test]
    fn unchanged_scan_emits_nothing() {
        let mut s = Store::open_in_memory().unwrap();
        s.apply_scan(Category::GpuDriver, &[fact("a", "1")], 100).unwrap();
        assert!(s.apply_scan(Category::GpuDriver, &[fact("a", "1")], 200).unwrap().is_empty());
    }

    #[test]
    fn empty_baseline_still_reports_later_additions() {
        let mut s = Store::open_in_memory().unwrap();
        s.apply_scan(Category::Autostart, &[], 100).unwrap();
        let f = Fact::new(Category::Autostart, "x", "x", "x.exe");
        assert_eq!(s.apply_scan(Category::Autostart, &[f], 200).unwrap().len(), 1);
    }

    #[test]
    fn categories_are_isolated() {
        let mut s = Store::open_in_memory().unwrap();
        s.apply_scan(Category::GpuDriver, &[fact("a", "1")], 100).unwrap();
        s.apply_scan(Category::Autostart, &[], 100).unwrap();
        // Scanning another category must not remove GPU facts.
        s.apply_scan(Category::Autostart, &[], 200).unwrap();
        assert_eq!(s.current_state().unwrap().len(), 1);
    }

    #[test]
    fn session_roundtrip() {
        let s = Store::open_in_memory().unwrap();
        let id = s.start_session("steam:1", "Game", 1000).unwrap();
        let m = SessionMetrics {
            frames: 5000,
            avg_fps: 120.0,
            p1_low_fps: 80.0,
            p01_low_fps: 60.0,
            median_frametime_ms: 8.2,
            stutters_per_min: 1.5,
        };
        s.finish_session(id, 2000, Some(&m), None).unwrap();
        let all = s.sessions(Some("steam:1")).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].metrics, Some(m));
        assert_eq!(all[0].duration_secs(), Some(1000));
        assert!(s.sessions(Some("steam:2")).unwrap().is_empty());
    }

    #[test]
    fn exclusion_and_background_load() {
        let s = Store::open_in_memory().unwrap();
        let id = s.start_session("steam:1", "Game", 1000).unwrap();
        s.finish_session(id, 2000, None, None).unwrap();
        let got = s.session(id).unwrap().unwrap();
        assert!(!got.excluded);
        assert_eq!(got.load_samples, None);

        assert!(s.set_excluded(id, true).unwrap());
        assert!(s.session(id).unwrap().unwrap().excluded);
        assert!(!s.set_excluded(999, true).unwrap());
        assert!(s.session(999).unwrap().is_none());

        let load = vec![
            ProcessLoad { name: "a.exe".into(), avg_cpu: 2.0, peak_cpu: 4.0 },
            ProcessLoad { name: "b.exe".into(), avg_cpu: 7.5, peak_cpu: 9.0 },
        ];
        s.save_session_load(id, 40, &load).unwrap();
        assert_eq!(s.session(id).unwrap().unwrap().load_samples, Some(40));
        let back = s.session_load(id).unwrap();
        assert_eq!(back[0].name, "b.exe");
        // Saving again replaces the previous list.
        s.save_session_load(id, 41, &load[..1]).unwrap();
        assert_eq!(s.session_load(id).unwrap().len(), 1);
    }

    #[test]
    fn german_value_words_become_codes() {
        let mut s = Store::open_in_memory().unwrap();
        let facts = [
            Fact::new(Category::SystemSetting, "game_mode", "Spielmodus", "an"),
            Fact::new(Category::SystemSetting, "hags", "HAGS", "unbekannt (3)"),
            Fact::new(Category::SystemSetting, "vbs", "VBS", "Standard"),
        ];
        s.apply_scan(Category::SystemSetting, &facts, 1).unwrap();
        s.apply_scan(Category::GameBuild, &[Fact::new(Category::GameBuild, "riot:x", "X", "Dateistand 2026-01-02 03:04")], 1).unwrap();
        migrate_value_codes(&s.conn).unwrap();
        migrate_value_codes(&s.conn).unwrap(); // idempotent
        let mut values: Vec<_> = s.current_state().unwrap().into_iter().map(|f| f.value).collect();
        values.sort();
        assert_eq!(values, vec!["default", "files 2026-01-02 03:04", "on", "unknown:3"]);
        // The next scan with the new codes reports nothing.
        let now = [
            Fact::new(Category::SystemSetting, "game_mode", "Spielmodus", "on"),
            Fact::new(Category::SystemSetting, "hags", "HAGS", "unknown:3"),
            Fact::new(Category::SystemSetting, "vbs", "VBS", "default"),
        ];
        assert!(s.apply_scan(Category::SystemSetting, &now, 2).unwrap().is_empty());
    }

    #[test]
    fn old_databases_are_migrated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.db");
        {
            // Schema of the first release, without the new columns.
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE sessions (id INTEGER PRIMARY KEY AUTOINCREMENT, game_id TEXT NOT NULL,
                    game_name TEXT NOT NULL, started_at INTEGER NOT NULL, ended_at INTEGER, frames INTEGER,
                    avg_fps REAL, p1_low_fps REAL, p01_low_fps REAL, median_ft_ms REAL, stutters_min REAL,
                    capture_note TEXT);
                 INSERT INTO sessions (game_id, game_name, started_at, ended_at) VALUES ('steam:1', 'Old', 5, 9);",
            )
            .unwrap();
        }
        let s = Store::open(&path).unwrap();
        let all = s.sessions(None).unwrap();
        assert_eq!(all.len(), 1);
        assert!(!all[0].excluded);
        // Opening twice must not try to add the columns again.
        drop(s);
        assert!(Store::open(&path).is_ok());
    }

    #[test]
    fn dangling_sessions_are_closed() {
        let s = Store::open_in_memory().unwrap();
        s.start_session("steam:1", "Game", 1000).unwrap();
        assert_eq!(s.close_dangling_sessions().unwrap(), 1);
        let all = s.sessions(None).unwrap();
        assert_eq!(all[0].duration_secs(), Some(0));
        assert!(all[0].metrics.is_none());
    }
}
