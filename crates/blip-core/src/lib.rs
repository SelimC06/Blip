pub mod config;
pub mod llm;
pub mod model;
pub mod profile;
pub mod score;
pub mod sources;
pub mod store;

use anyhow::Result;
use model::ScanReport;
use std::time::Duration;
use store::Store;

/// One full Phase-0 cycle: fetch all sources, dedupe against the store,
/// return only never-seen-before postings. Source failures land in
/// `report.errors` instead of aborting the cycle (the pill's Error state
/// is built on this).
pub fn run_scan(store: &Store) -> Result<ScanReport> {
    let client = reqwest::blocking::Client::builder()
        .user_agent("blip/0.1 (job scan; local personal use)")
        .timeout(Duration::from_secs(20))
        .build()?;

    let mut report = ScanReport::default();
    let mut all = Vec::new();

    match sources::simplify::fetch(&client) {
        Ok(mut p) => all.append(&mut p),
        Err(e) => report.errors.push(format!("simplify: {e}")),
    }
    for (board, company) in sources::greenhouse::WATCHLIST {
        match sources::greenhouse::fetch_board(&client, board, company) {
            Ok(mut p) => all.append(&mut p),
            Err(e) => report.errors.push(format!("greenhouse:{board}: {e}")),
        }
    }

    report.scanned = all.len();
    for posting in all {
        if store.insert_if_new(&posting)? {
            report.new.push(posting);
        }
    }
    store.log_cycle(report.scanned, report.new.len(), &report.errors)?;
    Ok(report)
}
