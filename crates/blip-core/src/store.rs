use crate::model::Posting;
use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};

pub struct Store {
    conn: Connection,
}

/// Default DB location: ~/Library/Application Support/Blip/blip.db (macOS)
/// or the platform equivalent. The CLI can override with --db.
pub fn default_db_path() -> PathBuf {
    let base = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join("Blip").join("blip.db")
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS postings (
                fingerprint TEXT PRIMARY KEY,
                company     TEXT NOT NULL,
                title       TEXT NOT NULL,
                location    TEXT NOT NULL,
                url         TEXT NOT NULL,
                source      TEXT NOT NULL,
                season      TEXT NOT NULL DEFAULT '',
                posted      TEXT NOT NULL DEFAULT '',
                first_seen  TEXT NOT NULL DEFAULT (datetime('now')),
                status      TEXT NOT NULL DEFAULT 'new'
            );
            CREATE TABLE IF NOT EXISTS cycles (
                id        INTEGER PRIMARY KEY AUTOINCREMENT,
                ran_at    TEXT NOT NULL DEFAULT (datetime('now')),
                scanned   INTEGER NOT NULL,
                new_count INTEGER NOT NULL,
                errors    TEXT NOT NULL DEFAULT ''
            );",
        )?;
        Ok(Store { conn })
    }

    /// Insert if never seen. Returns true when the posting is new.
    pub fn insert_if_new(&self, p: &Posting) -> Result<bool> {
        let n = self.conn.execute(
            "INSERT OR IGNORE INTO postings
             (fingerprint, company, title, location, url, source, season, posted)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                p.fingerprint(),
                p.company,
                p.title,
                p.location,
                p.url,
                p.source,
                p.season,
                p.posted
            ],
        )?;
        Ok(n == 1)
    }

    pub fn log_cycle(&self, scanned: usize, new_count: usize, errors: &[String]) -> Result<()> {
        self.conn.execute(
            "INSERT INTO cycles (scanned, new_count, errors) VALUES (?1, ?2, ?3)",
            params![scanned, new_count, errors.join("; ")],
        )?;
        Ok(())
    }

    /// Postings never yet shown to the user — the scoring pool each cycle.
    pub fn unsurfaced(&self) -> Result<Vec<Posting>> {
        let mut stmt = self.conn.prepare(
            "SELECT company, title, location, url, source, season, posted
             FROM postings WHERE status = 'new'",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Posting {
                company: r.get(0)?,
                title: r.get(1)?,
                location: r.get(2)?,
                url: r.get(3)?,
                source: r.get(4)?,
                season: r.get(5)?,
                posted: r.get(6)?,
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// Applied / dismissed / surfaced transitions from the UI.
    pub fn set_status(&self, fingerprint: &str, status: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE postings SET status = ?2 WHERE fingerprint = ?1",
            params![fingerprint, status],
        )?;
        Ok(())
    }

    pub fn mark_surfaced(&self, fingerprints: &[String]) -> Result<()> {
        for fp in fingerprints {
            self.conn.execute(
                "UPDATE postings SET status = 'surfaced' WHERE fingerprint = ?1",
                params![fp],
            )?;
        }
        Ok(())
    }

    pub fn total_postings(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM postings", [], |r| r.get(0))?)
    }
}
