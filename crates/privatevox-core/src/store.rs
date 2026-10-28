//! History store: SQLite in WAL mode, one transaction per dictation (design §6, §10).

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub struct NewDictation {
    pub app_name: Option<String>,
    pub mode: String,
    pub model: String,
    pub language: Option<String>,
    pub duration_ms: u32,
    pub raw_text: String,
    pub polished_text: Option<String>,
    /// Endpoint id that produced `polished_text`, when an LLM did.
    pub polished_by: Option<String>,
    /// `none` (no LLM step), `ok`, or `unpolished` (every endpoint failed; raw text was pasted).
    pub polish_status: String,
    pub status: String,
    pub timing_json: Option<String>,
    /// `local`, `remote:<host label>` (this machine's copy of a dictation a host processed) or `host` (processed for a client).
    pub engine_kind: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Dictation {
    pub id: i64,
    pub created_at_ms: i64,
    pub app_name: Option<String>,
    pub mode: String,
    pub model: String,
    pub language: Option<String>,
    pub duration_ms: u32,
    pub raw_text: String,
    pub polished_text: Option<String>,
    pub polished_by: Option<String>,
    pub polish_status: String,
    pub status: String,
    pub timing_json: Option<String>,
    pub pinned: bool,
    pub engine_kind: String,
    /// Kept audio clip (16 kHz WAV), when keep-audio was on.
    pub audio_path: Option<String>,
}

pub struct Store {
    conn: Connection,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS dictations (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  created_at_ms INTEGER NOT NULL,
  app_name TEXT,
  mode TEXT NOT NULL,
  model TEXT NOT NULL,
  engine_kind TEXT NOT NULL DEFAULT 'local',
  language TEXT,
  duration_ms INTEGER NOT NULL,
  raw_text TEXT NOT NULL,
  polished_text TEXT,
  polished_by TEXT,
  status TEXT NOT NULL,
  audio_path TEXT,
  timing_json TEXT,
  pinned INTEGER NOT NULL DEFAULT 0,
  deleted_at_ms INTEGER,
  polish_status TEXT NOT NULL DEFAULT 'none'
);
CREATE INDEX IF NOT EXISTS dictations_created ON dictations(created_at_ms DESC);
CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value_json TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS models (
  id TEXT PRIMARY KEY,
  load_ms INTEGER,
  speed_x REAL,
  mem_bytes INTEGER,
  measured_at_ms INTEGER NOT NULL
);
";

/// What was measured about a model on this machine (design §4.6, §9).
#[derive(Debug, Clone, PartialEq, serde::Serialize, Default)]
pub struct ModelStats {
    pub load_ms: Option<u64>,
    pub speed_x: Option<f32>,
    pub mem_bytes: Option<u64>,
}

/// Forward migrations, each idempotent so a crash between a step and the version write is harmless.
fn migrate(conn: &Connection, from: i64) -> Result<()> {
    if from < 2 {
        let has_column: bool = conn
            .prepare("PRAGMA table_info(dictations)")?
            .query_map([], |r| r.get::<_, String>(1))?
            .filter_map(|r| r.ok())
            .any(|c| c == "polish_status");
        if !has_column {
            conn.execute("ALTER TABLE dictations ADD COLUMN polish_status TEXT NOT NULL DEFAULT 'none'", [])?;
        }
    }
    conn.execute("UPDATE schema_version SET version = ?1", params![SCHEMA_VERSION])?;
    Ok(())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

const COLUMNS: &str = "id, created_at_ms, app_name, mode, model, language, duration_ms, raw_text, polished_text, status, timing_json, pinned, polished_by, polish_status, engine_kind, audio_path";
/// Current schema version; `open` migrates older files forward.
pub const SCHEMA_VERSION: i64 = 2;

fn map_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Dictation> {
    Ok(Dictation {
        id: r.get(0)?,
        created_at_ms: r.get(1)?,
        app_name: r.get(2)?,
        mode: r.get(3)?,
        model: r.get(4)?,
        language: r.get(5)?,
        duration_ms: r.get::<_, i64>(6)? as u32,
        raw_text: r.get(7)?,
        polished_text: r.get(8)?,
        status: r.get(9)?,
        timing_json: r.get(10)?,
        pinned: r.get::<_, i64>(11)? != 0,
        polished_by: r.get(12)?,
        polish_status: r.get(13)?,
        engine_kind: r.get(14)?,
        audio_path: r.get(15)?,
    })
}

impl Store {
    pub fn open(path: &Path) -> Result<Store> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(SCHEMA)?;
        let version: Option<i64> = conn.query_row("SELECT version FROM schema_version LIMIT 1", [], |r| r.get(0)).optional()?;
        match version {
            None => {
                conn.execute("INSERT INTO schema_version(version) VALUES (?1)", params![SCHEMA_VERSION])?;
            }
            Some(v) if v < SCHEMA_VERSION => migrate(&conn, v)?,
            Some(_) => {}
        }
        Ok(Store { conn })
    }

    pub fn schema_version(&self) -> Result<i64> {
        Ok(self.conn.query_row("SELECT version FROM schema_version LIMIT 1", [], |r| r.get(0))?)
    }

    pub fn journal_mode(&self) -> Result<String> {
        Ok(self.conn.pragma_query_value(None, "journal_mode", |r| r.get::<_, String>(0))?)
    }

    /// Inserts one dictation in its own transaction and returns its id.
    pub fn insert(&mut self, d: &NewDictation) -> Result<i64> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO dictations(created_at_ms, app_name, mode, model, language, duration_ms, raw_text, polished_text, status, timing_json, polished_by, polish_status, engine_kind)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![now_ms(), d.app_name, d.mode, d.model, d.language, d.duration_ms as i64, d.raw_text, d.polished_text, d.status, d.timing_json, d.polished_by, d.polish_status, d.engine_kind],
        )?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(id)
    }

    /// Newest first, excluding soft-deleted rows.
    pub fn list(&self, limit: u32, offset: u32) -> Result<Vec<Dictation>> {
        let sql = format!("SELECT {COLUMNS} FROM dictations WHERE deleted_at_ms IS NULL ORDER BY pinned DESC, created_at_ms DESC, id DESC LIMIT ?1 OFFSET ?2");
        let mut st = self.conn.prepare_cached(&sql)?;
        let rows = st.query_map(params![limit as i64, offset as i64], map_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Case-insensitive substring search over raw and polished text.
    pub fn search(&self, query: &str, limit: u32) -> Result<Vec<Dictation>> {
        let like = format!("%{}%", query.to_lowercase());
        let sql = format!("SELECT {COLUMNS} FROM dictations WHERE deleted_at_ms IS NULL AND (lower(raw_text) LIKE ?1 OR lower(coalesce(polished_text, '')) LIKE ?1) ORDER BY created_at_ms DESC LIMIT ?2");
        let mut st = self.conn.prepare_cached(&sql)?;
        let rows = st.query_map(params![like, limit as i64], map_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn get(&self, id: i64) -> Result<Option<Dictation>> {
        let sql = format!("SELECT {COLUMNS} FROM dictations WHERE id = ?1 AND deleted_at_ms IS NULL");
        Ok(self.conn.query_row(&sql, params![id], map_row).optional()?)
    }

    /// Re-polish result: new mode, text, producing endpoint and status; `raw_text` is untouched.
    pub fn update_polish(&mut self, id: i64, mode: &str, polished_text: Option<&str>, polished_by: Option<&str>, polish_status: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE dictations SET mode = ?1, polished_text = ?2, polished_by = ?3, polish_status = ?4 WHERE id = ?5",
            params![mode, polished_text, polished_by, polish_status, id],
        )?;
        Ok(())
    }

    pub fn set_audio_path(&mut self, id: i64, path: Option<&str>) -> Result<()> {
        self.conn.execute("UPDATE dictations SET audio_path = ?1 WHERE id = ?2", params![path, id])?;
        Ok(())
    }

    /// A re-run from the kept clip replaces Whisper's text.
    pub fn update_raw(&mut self, id: i64, raw_text: &str) -> Result<()> {
        self.conn.execute("UPDATE dictations SET raw_text = ?1 WHERE id = ?2", params![raw_text, id])?;
        Ok(())
    }

    pub fn update_text(&mut self, id: i64, polished_text: &str) -> Result<()> {
        self.conn.execute("UPDATE dictations SET polished_text = ?1 WHERE id = ?2", params![polished_text, id])?;
        Ok(())
    }

    /// Soft delete: the row stays for recovery, hidden from lists.
    pub fn delete(&mut self, id: i64) -> Result<()> {
        self.conn.execute("UPDATE dictations SET deleted_at_ms = ?1 WHERE id = ?2", params![now_ms(), id])?;
        Ok(())
    }

    pub fn set_pinned(&mut self, id: i64, pinned: bool) -> Result<()> {
        self.conn.execute("UPDATE dictations SET pinned = ?1 WHERE id = ?2", params![pinned as i64, id])?;
        Ok(())
    }

    pub fn model_stats(&self, id: &str) -> Result<Option<ModelStats>> {
        Ok(self
            .conn
            .query_row("SELECT load_ms, speed_x, mem_bytes FROM models WHERE id = ?1", params![id], |r| {
                Ok(ModelStats {
                    load_ms: r.get::<_, Option<i64>>(0)?.map(|v| v as u64),
                    speed_x: r.get::<_, Option<f64>>(1)?.map(|v| v as f32),
                    mem_bytes: r.get::<_, Option<i64>>(2)?.map(|v| v as u64),
                })
            })
            .optional()?)
    }

    /// Upsert; `None` fields keep the stored value.
    pub fn set_model_stats(&mut self, id: &str, stats: &ModelStats) -> Result<()> {
        self.conn.execute(
            "INSERT INTO models(id, load_ms, speed_x, mem_bytes, measured_at_ms) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET
               load_ms = coalesce(excluded.load_ms, models.load_ms),
               speed_x = coalesce(excluded.speed_x, models.speed_x),
               mem_bytes = coalesce(excluded.mem_bytes, models.mem_bytes),
               measured_at_ms = excluded.measured_at_ms",
            params![id, stats.load_ms.map(|v| v as i64), stats.speed_x.map(|v| v as f64), stats.mem_bytes.map(|v| v as i64), now_ms()],
        )?;
        Ok(())
    }

    pub fn count(&self) -> Result<u32> {
        Ok(self.conn.query_row("SELECT count(*) FROM dictations WHERE deleted_at_ms IS NULL", [], |r| r.get::<_, i64>(0))? as u32)
    }

    /// All rows including soft-deleted ones (tests and future "empty trash").
    pub fn raw_row_count(&self) -> Result<u32> {
        Ok(self.conn.query_row("SELECT count(*) FROM dictations", [], |r| r.get::<_, i64>(0))? as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> (tempfile::TempDir, Store) {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(&d.path().join("privatevox.sqlite")).unwrap();
        (d, s)
    }

    fn row(text: &str) -> NewDictation {
        NewDictation {
            app_name: Some("TextEdit".into()),
            mode: "raw".into(),
            model: "large-v3-turbo".into(),
            language: Some("en".into()),
            duration_ms: 1200,
            raw_text: text.into(),
            polished_text: None,
            polished_by: None,
            polish_status: "none".into(),
            status: "ok".into(),
            timing_json: Some(r#"{"decode_ms":1030}"#.into()),
            engine_kind: "local".into(),
        }
    }

    #[test]
    fn open_creates_schema_in_wal_mode() {
        let (_d, s) = fresh();
        assert_eq!(s.journal_mode().unwrap(), "wal");
        assert_eq!(s.count().unwrap(), 0);
    }

    #[test]
    fn insert_then_list_newest_first() {
        let (_d, mut s) = fresh();
        let a = s.insert(&row("first")).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let b = s.insert(&row("second")).unwrap();
        assert!(b > a);
        let rows = s.list(10, 0).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].raw_text, "second");
        assert_eq!(rows[1].raw_text, "first");
        assert_eq!(rows[0].app_name.as_deref(), Some("TextEdit"));
        assert!(!rows[0].pinned);
        assert_eq!(s.list(1, 1).unwrap()[0].raw_text, "first");
    }

    #[test]
    fn search_matches_case_insensitively() {
        let (_d, mut s) = fresh();
        s.insert(&row("Send the Deck before three")).unwrap();
        s.insert(&row("benchmark the turbo model")).unwrap();
        let hits = s.search("DECK", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].raw_text, "Send the Deck before three");
        assert!(s.search("nothing", 10).unwrap().is_empty());
    }

    #[test]
    fn update_text_changes_polished() {
        let (_d, mut s) = fresh();
        let id = s.insert(&row("um hello")).unwrap();
        s.update_text(id, "Hello.").unwrap();
        let r = &s.list(10, 0).unwrap()[0];
        assert_eq!(r.polished_text.as_deref(), Some("Hello."));
        assert_eq!(r.raw_text, "um hello");
    }

    #[test]
    fn delete_hides_row_but_keeps_it() {
        let (_d, mut s) = fresh();
        let id = s.insert(&row("gone")).unwrap();
        s.delete(id).unwrap();
        assert_eq!(s.count().unwrap(), 0);
        assert!(s.list(10, 0).unwrap().is_empty());
        assert_eq!(s.raw_row_count().unwrap(), 1);
    }

    #[test]
    fn pin_toggles() {
        let (_d, mut s) = fresh();
        let id = s.insert(&row("keep")).unwrap();
        s.set_pinned(id, true).unwrap();
        assert!(s.list(10, 0).unwrap()[0].pinned);
    }

    #[test]
    fn model_stats_roundtrip() {
        let (_d, mut s) = fresh();
        assert!(s.model_stats("large-v3").unwrap().is_none());
        s.set_model_stats("large-v3", &ModelStats { load_ms: Some(1300), speed_x: Some(4.2), mem_bytes: Some(1_050_000_000) }).unwrap();
        let st = s.model_stats("large-v3").unwrap().unwrap();
        assert_eq!(st.load_ms, Some(1300));
        assert_eq!(st.speed_x, Some(4.2));
        assert_eq!(st.mem_bytes, Some(1_050_000_000));
        s.set_model_stats("large-v3", &ModelStats { load_ms: None, speed_x: Some(5.0), mem_bytes: Some(1_050_000_000) }).unwrap();
        assert_eq!(s.model_stats("large-v3").unwrap().unwrap().speed_x, Some(5.0));
    }

    #[test]
    fn pinned_rows_list_first() {
        let (_d, mut s) = fresh();
        let a = s.insert(&row("older")).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        s.insert(&row("newer")).unwrap();
        s.set_pinned(a, true).unwrap();
        assert_eq!(s.list(10, 0).unwrap()[0].raw_text, "older");
    }

    #[test]
    fn reopen_keeps_rows() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("privatevox.sqlite");
        {
            let mut s = Store::open(&p).unwrap();
            s.insert(&row("persist")).unwrap();
        }
        let s = Store::open(&p).unwrap();
        assert_eq!(s.count().unwrap(), 1);
    }

    /// The schema as shipped in 0.2.0 (version 1): no `polish_status` column.
    const V1_SCHEMA: &str = "
CREATE TABLE schema_version (version INTEGER NOT NULL);
INSERT INTO schema_version(version) VALUES (1);
CREATE TABLE dictations (
  id INTEGER PRIMARY KEY AUTOINCREMENT, created_at_ms INTEGER NOT NULL, app_name TEXT, mode TEXT NOT NULL, model TEXT NOT NULL,
  engine_kind TEXT NOT NULL DEFAULT 'local', language TEXT, duration_ms INTEGER NOT NULL, raw_text TEXT NOT NULL, polished_text TEXT,
  polished_by TEXT, status TEXT NOT NULL, audio_path TEXT, timing_json TEXT, pinned INTEGER NOT NULL DEFAULT 0, deleted_at_ms INTEGER);
CREATE TABLE settings (key TEXT PRIMARY KEY, value_json TEXT NOT NULL);
CREATE TABLE models (id TEXT PRIMARY KEY, load_ms INTEGER, speed_x REAL, mem_bytes INTEGER);
INSERT INTO dictations(created_at_ms, mode, model, duration_ms, raw_text, status) VALUES (1, 'raw', 'large-v3-turbo', 500, 'old row', 'ok');
";

    #[test]
    fn migrates_v1_database_to_v2() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("privatevox.sqlite");
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch(V1_SCHEMA).unwrap();
        }
        let s = Store::open(&path).unwrap();
        assert_eq!(s.schema_version().unwrap(), 2);
        let rows = s.list(10, 0).unwrap();
        assert_eq!(rows[0].raw_text, "old row");
        assert_eq!(rows[0].polish_status, "none");
        assert_eq!(rows[0].polished_by, None);
        // Opening again is idempotent.
        drop(s);
        let s = Store::open(&path).unwrap();
        assert_eq!(s.schema_version().unwrap(), 2);
    }

    #[test]
    fn fresh_database_is_version_2() {
        let (_d, s) = fresh();
        assert_eq!(s.schema_version().unwrap(), 2);
    }

    #[test]
    fn insert_stores_polish_fields() {
        let (_d, mut s) = fresh();
        let mut r = row("raw");
        r.polished_text = Some("Raw.".into());
        r.polished_by = Some("ep-1".into());
        r.polish_status = "ok".into();
        let id = s.insert(&r).unwrap();
        let d = s.get(id).unwrap().unwrap();
        assert_eq!(d.polished_by.as_deref(), Some("ep-1"));
        assert_eq!(d.polish_status, "ok");
        assert!(s.get(9999).unwrap().is_none());
    }

    #[test]
    fn update_polish_changes_mode_text_and_status() {
        let (_d, mut s) = fresh();
        let id = s.insert(&row("raw")).unwrap();
        s.update_polish(id, "email", Some("Dear all,"), Some("ep-1"), "ok").unwrap();
        let d = s.get(id).unwrap().unwrap();
        assert_eq!(d.mode, "email");
        assert_eq!(d.polished_text.as_deref(), Some("Dear all,"));
        assert_eq!(d.polished_by.as_deref(), Some("ep-1"));
        assert_eq!(d.polish_status, "ok");
        assert_eq!(d.raw_text, "raw");
    }

    #[test]
    fn audio_path_round_trip_and_update_raw() {
        let (_d, mut s) = fresh();
        let id = s.insert(&row("raw")).unwrap();
        assert!(s.get(id).unwrap().unwrap().audio_path.is_none());
        s.set_audio_path(id, Some("/tmp/clips/1.wav")).unwrap();
        assert_eq!(s.get(id).unwrap().unwrap().audio_path.as_deref(), Some("/tmp/clips/1.wav"));
        s.update_raw(id, "second pass").unwrap();
        assert_eq!(s.get(id).unwrap().unwrap().raw_text, "second pass");
        s.set_audio_path(id, None).unwrap();
        assert!(s.list(10, 0).unwrap()[0].audio_path.is_none());
    }
}
