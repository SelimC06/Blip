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
    "ALTER TABLE postings ADD COLUMN job_key TEXT",
    "ALTER TABLE postings ADD COLUMN red_flags TEXT",
    "ALTER TABLE postings ADD COLUMN score_key TEXT",
    "ALTER TABLE postings ADD COLUMN auth_req TEXT",
    "ALTER TABLE postings ADD COLUMN dismissed_at TEXT",
];

/// One row of the history view.
#[derive(Debug, Clone, serde::Serialize)]
pub struct HistoryRow {
    pub fingerprint: String,
    pub company: String,
    pub title: String,
    pub location: String,
    pub url: String,
    pub posted: String,
    pub deadline: String,
    pub score: Option<i64>,
    pub reason: String,
    pub status: String,
    /// When it happened for this view (shown, applied, or dismissed), UTC
    /// "YYYY-MM-DD HH:MM:SS", or "" for rows from before timestamps existed.
    pub at: String,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct HistoryCounts {
    pub shown: i64,
    pub applied: i64,
    pub dismissed: i64,
}

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
        conn.execute("CREATE INDEX IF NOT EXISTS postings_job_key ON postings(job_key)", [])?;
        let store = Store { conn };
        store.backfill_job_keys()?;
        store.backfill_auth_req()?;
        Ok(store)
    }

    /// Classify descriptions saved before the authorization filter existed.
    fn backfill_auth_req(&self) -> Result<()> {
        let pending: Vec<(String, String, String)> = {
            let mut stmt = self.conn.prepare(
                "SELECT fingerprint, title, description FROM postings WHERE auth_req IS NULL",
            )?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            rows.filter_map(|r| r.ok()).collect()
        };
        for (fp, title, description) in pending {
            self.conn.execute(
                "UPDATE postings SET auth_req = ?2 WHERE fingerprint = ?1",
                params![fp, crate::auth::classify(&title, &description)],
            )?;
        }
        Ok(())
    }

    /// Give rows saved before job keys existed their key, then retire the
    /// second copy of any job stored twice: the copy you've already seen
    /// (surfaced/applied/dismissed) wins, otherwise the earliest.
    fn backfill_job_keys(&self) -> Result<()> {
        let pending: Vec<(String, String)> = {
            let mut stmt = self
                .conn
                .prepare("SELECT fingerprint, url FROM postings WHERE job_key IS NULL")?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.filter_map(|r| r.ok()).collect()
        };
        if pending.is_empty() {
            return Ok(());
        }
        for (fp, url) in &pending {
            // '' marks "no key" so the row isn't revisited on every open.
            let key = crate::model::job_key(url).unwrap_or_default();
            self.conn.execute(
                "UPDATE postings SET job_key = ?2 WHERE fingerprint = ?1",
                params![fp, key],
            )?;
        }
        self.conn.execute(
            "UPDATE postings SET status = 'duplicate'
             WHERE status = 'new' AND job_key != '' AND fingerprint != (
               SELECT keep.fingerprint FROM postings keep
               WHERE keep.job_key = postings.job_key AND keep.status != 'duplicate'
               ORDER BY (keep.status != 'new') DESC, keep.first_seen, keep.fingerprint
               LIMIT 1)",
            [],
        )?;
        Ok(())
    }

    /// Insert if never seen. Returns true when the posting is new.
    ///
    /// Known postings get their source-owned fields refreshed (posting date,
    /// published deadline) so corrections from the source reach old rows.
    /// A posting whose job key is already stored counts as seen.
    pub fn insert_if_new(&self, p: &Posting) -> Result<bool> {
        let fp = p.fingerprint();
        let exists: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM postings WHERE fingerprint = ?1)",
            params![fp],
            |r| r.get(0),
        )?;
        if exists {
            self.conn.execute(
                "UPDATE postings SET posted = ?2,
                    deadline = CASE WHEN ?3 != '' THEN ?3 ELSE deadline END
                 WHERE fingerprint = ?1",
                params![fp, p.posted, p.deadline],
            )?;
            return Ok(false);
        }
        let key = p.job_key().unwrap_or_default();
        if !key.is_empty() {
            let dup: bool = self.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM postings WHERE job_key = ?1)",
                params![key],
                |r| r.get(0),
            )?;
            if dup {
                return Ok(false);
            }
        }
        self.conn.execute(
            "INSERT INTO postings
             (fingerprint, company, title, location, url, source, season, posted,
              description, deadline, job_key, auth_req)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULLIF(?10, ''), ?11, ?12)",
            params![
                fp,
                p.company,
                p.title,
                p.location,
                p.url,
                p.source,
                p.season,
                p.posted,
                p.description,
                p.deadline,
                key,
                p.auth_requirement()
            ],
        )?;
        Ok(true)
    }

    pub fn log_cycle(&self, scanned: usize, new_count: usize, errors: &[String]) -> Result<()> {
        self.conn.execute(
            "INSERT INTO cycles (scanned, new_count, errors) VALUES (?1, ?2, ?3)",
            params![scanned, new_count, errors.join("; ")],
        )?;
        Ok(())
    }

    /// Postings never yet shown to the user — the scoring pool each cycle.
    /// Each comes with its cached score when one exists for this `score_key`
    /// (same resume, same "looking for", same model), so it isn't re-scored.
    pub fn unsurfaced(&self, score_key: &str) -> Result<Vec<(Posting, Option<Scored>)>> {
        let mut stmt = self.conn.prepare(
            "SELECT company, title, location, url, source, season, posted, description,
                    COALESCE(deadline, ''), score, reason, red_flags, score_key
             FROM postings WHERE status = 'new'",
        )?;
        let rows = stmt.query_map([], |r| {
            let posting = Posting {
                company: r.get(0)?,
                title: r.get(1)?,
                location: r.get(2)?,
                url: r.get(3)?,
                source: r.get(4)?,
                season: r.get(5)?,
                posted: r.get(6)?,
                description: r.get(7)?,
                deadline: r.get(8)?,
            };
            let score: Option<i64> = r.get(9)?;
            let cached_key: Option<String> = r.get(12)?;
            let cached = match (score, cached_key) {
                (Some(score), Some(k)) if k == score_key => Some(Scored {
                    score: score.clamp(0, 100) as u8,
                    reason: r.get::<_, Option<String>>(10)?.unwrap_or_default(),
                    red_flags: r
                        .get::<_, Option<String>>(11)?
                        .and_then(|j| serde_json::from_str(&j).ok())
                        .unwrap_or_default(),
                    deadline: (!posting.deadline.is_empty()).then(|| posting.deadline.clone()),
                    posting: posting.clone(),
                }),
                _ => None,
            };
            Ok((posting, cached))
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// Cache a fetched description (so the page is never fetched twice)
    /// along with the work-authorization requirement it states.
    pub fn save_description(&self, p: &Posting) -> Result<()> {
        self.conn.execute(
            "UPDATE postings SET description = ?2, auth_req = ?3 WHERE fingerprint = ?1",
            params![p.fingerprint(), p.description, p.auth_requirement()],
        )?;
        Ok(())
    }

    /// Applied / dismissed transitions from the UI.
    pub fn set_status(&self, fingerprint: &str, status: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE postings SET status = ?2,
                applied_at = CASE WHEN ?2 = 'applied' THEN datetime('now') ELSE applied_at END,
                dismissed_at = CASE WHEN ?2 = 'dismissed' THEN datetime('now') ELSE dismissed_at END
             WHERE fingerprint = ?1",
            params![fingerprint, status],
        )?;
        Ok(())
    }

    /// Remember everything the LLM said about the postings it scored, so the
    /// export and deadline reminders can use it later.
    /// A source-published deadline is never overwritten by "the LLM found none".
    pub fn save_scores(&self, scored: &[Scored], score_key: &str) -> Result<()> {
        for s in scored {
            self.conn.execute(
                "UPDATE postings SET score = ?2, reason = ?3, deadline = COALESCE(?4, deadline),
                    red_flags = ?5, score_key = ?6
                 WHERE fingerprint = ?1",
                params![
                    s.posting.fingerprint(),
                    s.score,
                    s.reason,
                    s.deadline,
                    serde_json::to_string(&s.red_flags).unwrap_or_default(),
                    score_key
                ],
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

    /// "shown" = surfaced and not acted on; "applied"; "dismissed". Newest
    /// first; rows from before timestamps existed sort last.
    pub fn history(&self, kind: &str, limit: i64, offset: i64) -> Result<Vec<HistoryRow>> {
        let (status, at) = match kind {
            "applied" => ("applied", "COALESCE(applied_at, surfaced_at, '')"),
            "dismissed" => ("dismissed", "COALESCE(dismissed_at, surfaced_at, '')"),
            _ => ("surfaced", "COALESCE(surfaced_at, '')"),
        };
        let sql = format!(
            "SELECT fingerprint, company, title, location, url, posted, COALESCE(deadline, ''),
                    score, COALESCE(reason, ''), status, {at} AS at
             FROM postings WHERE status = ?1
             ORDER BY at = '' , at DESC, score DESC
             LIMIT ?2 OFFSET ?3"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![status, limit, offset], |r| {
            Ok(HistoryRow {
                fingerprint: r.get(0)?,
                company: r.get(1)?,
                title: r.get(2)?,
                location: r.get(3)?,
                url: r.get(4)?,
                posted: r.get(5)?,
                deadline: r.get(6)?,
                score: r.get(7)?,
                reason: r.get(8)?,
                status: r.get(9)?,
                at: r.get(10)?,
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn history_counts(&self) -> Result<HistoryCounts> {
        let mut counts = HistoryCounts::default();
        let mut stmt = self.conn.prepare(
            "SELECT status, COUNT(*) FROM postings
             WHERE status IN ('surfaced', 'applied', 'dismissed') GROUP BY status",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        for (status, n) in rows.flatten() {
            match status.as_str() {
                "surfaced" => counts.shown = n,
                "applied" => counts.applied = n,
                _ => counts.dismissed = n,
            }
        }
        Ok(counts)
    }

    /// Undo a dismissal: back to "shown", so it can be applied to later.
    pub fn restore(&self, fingerprint: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE postings SET status = 'surfaced', dismissed_at = NULL
             WHERE fingerprint = ?1 AND status = 'dismissed'",
            params![fingerprint],
        )?;
        Ok(())
    }

    pub fn total_postings(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM postings", [], |r| r.get(0))?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(name: &str) -> (Store, PathBuf) {
        let dir = std::env::temp_dir().join(format!("blip-store-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("blip.db");
        (Store::open(&path).unwrap(), dir)
    }

    fn posting(company: &str, title: &str, location: &str, url: &str) -> Posting {
        Posting {
            company: company.into(),
            title: title.into(),
            location: location.into(),
            url: url.into(),
            source: "test".into(),
            season: String::new(),
            posted: "2026-10-01T00:00:00Z".into(),
            description: String::new(),
            deadline: String::new(),
        }
    }

    #[test]
    fn same_job_from_two_sources_is_stored_once() {
        let (store, dir) = temp_store("dedupe");
        let community = posting("Datadog", "Software Engineering Intern", "NYC",
            "https://careers.datadoghq.com/detail/8114161/?utm_source=Simplify&gh_jid=8114161");
        let board = posting("Datadog", "Software Engineering Intern - 2027", "New York, New York, USA",
            "https://careers.datadoghq.com/detail/8114161/?gh_jid=8114161");
        assert!(store.insert_if_new(&community).unwrap());
        assert!(!store.insert_if_new(&board).unwrap(), "second listing of job 8114161 is a duplicate");
        assert_eq!(store.total_postings().unwrap(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn known_postings_get_corrected_dates_and_deadlines() {
        let (store, dir) = temp_store("refresh");
        let mut p = posting("Robinhood", "SWE Intern", "Menlo Park", "https://x.test/1");
        p.posted = "2026-10-05T00:00:00Z".into(); // wrong: was updated_at
        store.insert_if_new(&p).unwrap();
        p.posted = "2026-08-01T00:00:00Z".into(); // corrected: first_published
        p.deadline = "2026-10-14".into();
        assert!(!store.insert_if_new(&p).unwrap());
        let rows = store.unsurfaced("k").unwrap();
        assert_eq!(rows[0].0.posted, "2026-08-01T00:00:00Z");
        assert_eq!(rows[0].0.deadline, "2026-10-14");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn history_tracks_shown_applied_dismissed_and_restore() {
        let (store, dir) = temp_store("history");
        let a = posting("Acme", "Intern A", "NYC", "https://x.test/a");
        let b = posting("Bolt", "Intern B", "SF", "https://x.test/b");
        let c = posting("Core", "Intern C", "LA", "https://x.test/c");
        for p in [&a, &b, &c] {
            store.insert_if_new(p).unwrap();
        }
        store.mark_surfaced(&[a.fingerprint(), b.fingerprint(), c.fingerprint()]).unwrap();
        store.set_status(&b.fingerprint(), "applied").unwrap();
        store.set_status(&c.fingerprint(), "dismissed").unwrap();

        let n = store.history_counts().unwrap();
        assert_eq!((n.shown, n.applied, n.dismissed), (1, 1, 1));
        assert_eq!(store.history("shown", 10, 0).unwrap()[0].company, "Acme");
        let applied = store.history("applied", 10, 0).unwrap();
        assert_eq!(applied[0].company, "Bolt");
        assert!(!applied[0].at.is_empty(), "applied rows carry when it happened");
        assert_eq!(store.history("dismissed", 10, 0).unwrap()[0].company, "Core");

        store.restore(&c.fingerprint()).unwrap();
        let n = store.history_counts().unwrap();
        assert_eq!((n.shown, n.applied, n.dismissed), (2, 1, 0));
        // Restore only undoes dismissals; it never touches applied roles.
        store.restore(&b.fingerprint()).unwrap();
        assert_eq!(store.history_counts().unwrap().applied, 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn scores_are_cached_per_key_and_keep_source_deadlines() {
        let (store, dir) = temp_store("cache");
        let mut p = posting("Acme", "Intern", "Remote", "https://x.test/2");
        p.deadline = "2026-12-01".into();
        store.insert_if_new(&p).unwrap();
        let s = Scored {
            posting: p.clone(),
            score: 88,
            reason: "fits".into(),
            red_flags: vec!["needs clearance".into()],
            deadline: None, // the LLM found none
        };
        store.save_scores(&[s], "key-A").unwrap();

        let rows = store.unsurfaced("key-A").unwrap();
        let cached = rows[0].1.as_ref().expect("cached under the same key");
        assert_eq!(cached.score, 88);
        assert_eq!(cached.red_flags, vec!["needs clearance".to_string()]);
        assert_eq!(cached.deadline.as_deref(), Some("2026-12-01"), "source deadline survives");

        let rows = store.unsurfaced("key-B").unwrap();
        assert!(rows[0].1.is_none(), "a different resume/model/looking-for rescores");
        let _ = std::fs::remove_dir_all(dir);
    }
}
