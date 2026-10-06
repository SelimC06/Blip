pub mod applied_log;
pub mod config;
pub mod describe;
pub mod export;
pub mod llm;
pub mod model;
pub mod profile;
pub mod score;
pub mod secrets;
pub mod sources;
pub mod store;

use anyhow::Result;
use model::{Cancelled, Posting, ScanReport, SourceStatus};
use std::time::Duration;
use store::Store;

pub fn http_client() -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .user_agent("blip/0.1 (job scan; local personal use)")
        .timeout(Duration::from_secs(20))
        .build()?)
}

/// One full fetch cycle: every source, dedupe against the store, return only
/// never-seen-before postings. A failing source is recorded in
/// `report.sources` / `report.errors` instead of aborting the cycle.
/// `cancelled` is polled between sources.
pub fn run_scan(store: &Store, cancelled: &(dyn Fn() -> bool + Sync)) -> Result<ScanReport> {
    let client = http_client()?;
    let mut report = ScanReport::default();
    let mut all = Vec::new();

    let mut record = |name: String, result: Result<Vec<Posting>>, all: &mut Vec<Posting>| {
        match result {
            Ok(mut p) => {
                report.sources.push(SourceStatus { name, ok: true, count: p.len(), error: String::new() });
                all.append(&mut p);
            }
            Err(e) => {
                let msg = format!("{e}");
                report.errors.push(format!("{name}: {msg}"));
                report.sources.push(SourceStatus { name, ok: false, count: 0, error: msg });
            }
        }
    };

    record("github:simplify".into(), sources::simplify::fetch(&client), &mut all);
    for (board, company) in sources::greenhouse::WATCHLIST {
        if cancelled() {
            return Err(Cancelled.into());
        }
        record(
            format!("greenhouse:{board}"),
            sources::greenhouse::fetch_board(&client, board, company),
            &mut all,
        );
    }
    for (board, company) in sources::ashby::WATCHLIST {
        if cancelled() {
            return Err(Cancelled.into());
        }
        record(
            format!("ashby:{board}"),
            sources::ashby::fetch_board(&client, board, company),
            &mut all,
        );
    }

    report.scanned = all.len();
    for posting in all {
        if store.insert_if_new(&posting)? {
            report.new.push(posting);
        }
    }
    store.log_cycle(report.scanned, report.new.len(), &report.errors)?;
    store.record_source_health(&report.sources)?;
    Ok(report)
}
