use crate::model::{Posting, SourceStatus};
use crate::score::Scored;
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

/// Columns added after the first release. Applied with ALTER TABLE on open,
/// so existing databases upgrade in place.
const MIGRATIONS: &[&str] = &[
    "ALTER TABLE postings ADD COLUMN score INTEGER",
    "ALTER TABLE postings ADD COLUMN reason TEXT",
    "ALTER TABLE postings ADD COLUMN deadline TEXT",
    "ALTER TABLE postings ADD COLUMN reminded INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE postings ADD COLUMN surfaced_at TEXT",
    "ALTER TABLE postings ADD COLUMN applied_at TEXT",
    "ALTER TABLE postings ADD COLUMN description TEXT NOT NULL DEFAULT ''",
];

#[derive(Debug, Clone)]
pub struct Reminder {
    pub fingerprint: String,
    pub company: String,
    pub title: String,
    pub deadline: String,
}

#[derive(Debug, Clone)]
pub struct ExportRow {
    pub first_seen: String,
    pub status: String,
    pub score: Option<i64>,
    pub company: String,
    pub title: String,
    pub location: String,
    pub posted: String,
    pub deadline: String,
    pub url: String,
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
            );
            CREATE TABLE IF NOT EXISTS source_health (
                name       TEXT PRIMARY KEY,
                ok         INTEGER NOT NULL,
                count      INTEGER NOT NULL,
                error      TEXT NOT NULL DEFAULT '',
                checked_at TEXT NOT NULL DEFAULT (datetime('now'))
            );",
        )?;
        for sql in MIGRATIONS {
            if let Err(e) = conn.execute(sql, []) {
                if !e.to_string().contains("duplicate column") {
                    return Err(e.into());
                }
            }
        }
        Ok(Store { conn })
    }

    /// Insert if never seen. Returns true when the posting is new.
    pub fn insert_if_new(&self, p: &Posting) -> Result<bool> {
        let n = self.conn.execute(
            "INSERT OR IGNORE INTO postings
             (fingerprint, company, title, location, url, source, season, posted, description)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                p.fingerprint(),
                p.company,
                p.title,
                p.location,
                p.url,
                p.source,
                p.season,
                p.posted,
                p.description
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
            "SELECT company, title, location, url, source, season, posted, description
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
                description: r.get(7)?,
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// Cache a fetched description so the page is never fetched twice.
    pub fn save_description(&self, fingerprint: &str, description: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE postings SET description = ?2 WHERE fingerprint = ?1",
            params![fingerprint, description],
        )?;
        Ok(())
    }

    /// Applied / dismissed transitions from the UI.
    pub fn set_status(&self, fingerprint: &str, status: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE postings SET status = ?2,
                applied_at = CASE WHEN ?2 = 'applied' THEN datetime('now') ELSE applied_at END
             WHERE fingerprint = ?1",
            params![fingerprint, status],
        )?;
        Ok(())
    }

    /// Remember everything the LLM said about the postings it scored, so the
    /// export and deadline reminders can use it later.
    pub fn save_scores(&self, scored: &[Scored]) -> Result<()> {
        for s in scored {
            self.conn.execute(
                "UPDATE postings SET score = ?2, reason = ?3, deadline = ?4 WHERE fingerprint = ?1",
                params![s.posting.fingerprint(), s.score, s.reason, s.deadline],
            )?;
        }
        Ok(())
    }

    pub fn mark_surfaced(&self, fingerprints: &[String]) -> Result<()> {
        for fp in fingerprints {
            self.conn.execute(
                "UPDATE postings SET status = 'surfaced', surfaced_at = datetime('now')
                 WHERE fingerprint = ?1",
                params![fp],
            )?;
        }
        Ok(())
    }

    /// Shown to the user, not yet applied or dismissed, deadline within
    /// `days`, and never reminded before.
    pub fn due_reminders(&self, days: i64) -> Result<Vec<Reminder>> {
        let mut stmt = self.conn.prepare(
            "SELECT fingerprint, company, title, deadline FROM postings
             WHERE status = 'surfaced' AND reminded = 0 AND deadline IS NOT NULL
               AND date(deadline) >= date('now', 'localtime')
               AND date(deadline) <= date('now', 'localtime', ?1)",
        )?;
        let rows = stmt.query_map(params![format!("+{days} days")], |r| {
            Ok(Reminder {
                fingerprint: r.get(0)?,
                company: r.get(1)?,
                title: r.get(2)?,
                deadline: r.get(3)?,
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn mark_reminded(&self, fingerprint: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE postings SET reminded = 1 WHERE fingerprint = ?1",
            params![fingerprint],
        )?;
        Ok(())
    }

    pub fn record_source_health(&self, statuses: &[SourceStatus]) -> Result<()> {
        for s in statuses {
            self.conn.execute(
                "INSERT INTO source_health (name, ok, count, error, checked_at)
                 VALUES (?1, ?2, ?3, ?4, datetime('now'))
                 ON CONFLICT(name) DO UPDATE SET
                   ok = excluded.ok, count = excluded.count,
                   error = excluded.error, checked_at = excluded.checked_at",
                params![s.name, s.ok, s.count, s.error],
            )?;
        }
        Ok(())
    }

    pub fn source_health(&self) -> Result<Vec<SourceStatus>> {
        let mut stmt = self
            .conn
            .prepare("SELECT name, ok, count, error FROM source_health ORDER BY ok, name")?;
        let rows = stmt.query_map([], |r| {
            Ok(SourceStatus {
                name: r.get(0)?,
                ok: r.get(1)?,
                count: r.get(2)?,
                error: r.get(3)?,
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// Everything first seen in the last `days` days, newest first.
    pub fn export_since(&self, days: i64) -> Result<Vec<ExportRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT first_seen, status, score, company, title, location, posted,
                    COALESCE(deadline, ''), url
             FROM postings WHERE first_seen >= datetime('now', ?1)
             ORDER BY first_seen DESC, score DESC",
        )?;
        let rows = stmt.query_map(params![format!("-{days} days")], |r| {
            Ok(ExportRow {
                first_seen: r.get(0)?,
                status: r.get(1)?,
                score: r.get(2)?,
                company: r.get(3)?,
                title: r.get(4)?,
                location: r.get(5)?,
                posted: r.get(6)?,
                deadline: r.get(7)?,
                url: r.get(8)?,
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn total_postings(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM postings", [], |r| r.get(0))?)
    }
}
