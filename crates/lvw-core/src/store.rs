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
    pub status: String,
    pub timing_json: Option<String>,
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
    pub status: String,
    pub timing_json: Option<String>,
    pub pinned: bool,
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
  deleted_at_ms INTEGER
);
CREATE INDEX IF NOT EXISTS dictations_created ON dictations(created_at_ms DESC);
CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value_json TEXT NOT NULL);
";

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

const COLUMNS: &str = "id, created_at_ms, app_name, mode, model, language, duration_ms, raw_text, polished_text, status, timing_json, pinned";

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
        if version.is_none() {
            conn.execute("INSERT INTO schema_version(version) VALUES (1)", [])?;
        }
        Ok(Store { conn })
    }

    pub fn journal_mode(&self) -> Result<String> {
        Ok(self.conn.pragma_query_value(None, "journal_mode", |r| r.get::<_, String>(0))?)
    }

    /// Inserts one dictation in its own transaction and returns its id.
    pub fn insert(&mut self, d: &NewDictation) -> Result<i64> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO dictations(created_at_ms, app_name, mode, model, language, duration_ms, raw_text, polished_text, status, timing_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![now_ms(), d.app_name, d.mode, d.model, d.language, d.duration_ms as i64, d.raw_text, d.polished_text, d.status, d.timing_json],
        )?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(id)
    }

    /// Newest first, excluding soft-deleted rows.
    pub fn list(&self, limit: u32, offset: u32) -> Result<Vec<Dictation>> {
        let sql = format!("SELECT {COLUMNS} FROM dictations WHERE deleted_at_ms IS NULL ORDER BY created_at_ms DESC, id DESC LIMIT ?1 OFFSET ?2");
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
        let s = Store::open(&d.path().join("lvw.sqlite")).unwrap();
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
            status: "ok".into(),
            timing_json: Some(r#"{"decode_ms":1030}"#.into()),
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
    fn reopen_keeps_rows() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("lvw.sqlite");
        {
            let mut s = Store::open(&p).unwrap();
            s.insert(&row("persist")).unwrap();
        }
        let s = Store::open(&p).unwrap();
        assert_eq!(s.count().unwrap(), 1);
    }
}
