pub mod applied_log;
pub mod auth;
pub mod config;
pub mod describe;
pub mod export;
pub mod llm;
pub mod location;
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
pub fn run_scan(
    store: &Store,
    cfg: &config::Config,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<ScanReport> {
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

    if cfg.use_simplify {
        record("github:simplify".into(), sources::simplify::fetch(&client), &mut all);
    }
    // Only worth the download when the user wants new-grad roles at all.
    if cfg.use_simplify_new_grad && cfg.role_types.iter().any(|t| t == "new-grad") {
        record("github:simplify-new-grad".into(), sources::simplify::fetch_new_grad(&client), &mut all);
    }
    if cfg.use_vansh {
        record("github:vanshb03".into(), sources::vansh::fetch(&client), &mut all);
    }
    if cfg.use_amazon {
        record("amazon".into(), sources::amazon::fetch(&client), &mut all);
    }
    if cfg.use_usajobs {
        let key = secrets::usajobs_key().unwrap_or_default();
        record(
            "usajobs".into(),
            sources::usajobs::fetch(&client, &cfg.usajobs_email, &key, &cfg.role_types),
            &mut all,
        );
    }
    for company in &cfg.companies {
        if cancelled() {
            return Err(Cancelled.into());
        }
        record(company.source_name(), sources::fetch_company(&client, company), &mut all);
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
