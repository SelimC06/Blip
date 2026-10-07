//! Workday career sites (GM, NVIDIA, Intel, Salesforce, much of the Fortune
//! 500). Not a published API: it's the JSON endpoint Workday's own career
//! pages load from, so it works without a key but could change.
//!
//!   board  = "{tenant}.{wdN}/{site}"   e.g. "generalmotors.wd5/Careers_GM"
//!   search = POST https://{tenant}.{wdN}.myworkdayjobs.com/wday/cxs/{tenant}/{site}/jobs
//!   detail = GET  https://{host}/wday/cxs/{tenant}/{site}/job/…
//!
//! Search is relevance-ordered and loose ("intern" also matches "internal"),
//! so results are paged up to a cap and filtered by title.

use crate::model::Posting;
use anyhow::{bail, Result};
use chrono::{Duration, Local};
use regex::Regex;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::LazyLock;

const PAGE: usize = 20; // Workday's maximum page size
const MAX_PAGES: usize = 6; // per search term, per scan
const SEARCHES: [&str; 3] = ["intern", "co-op", "university graduate"];

static SITE_URL: LazyLock<Regex> = LazyLock::new(|| {
    // https://generalmotors.wd5.myworkdayjobs.com/en-US/Careers_GM/job/…
    Regex::new(r"(?i)https?://([a-z0-9-]+)\.(wd\d+)\.myworkdayjobs\.com/(?:[a-z]{2}-[a-z]{2}/)?([a-z0-9_-]+)(/job/[^?#]*)?").unwrap()
});
static EARLY_CAREER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(intern(ship)?s?|co-? ?op|new ?grad|university grad(uate)?|early career|graduate program)\b").unwrap()
});
static SEASON: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(summer|fall|spring|winter)\s*'?(20\d\d|\d\d)\b").unwrap());
static POSTED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)(today|yesterday|(\d+)\+?\s+days?\s+ago)").unwrap());

pub struct Site {
    pub tenant: String,
    pub wd: String,
    pub site: String,
    /// "/job/…" when the link pointed at a single job.
    pub job_path: Option<String>,
}

impl Site {
    pub fn host(&self) -> String {
        format!("{}.{}.myworkdayjobs.com", self.tenant, self.wd)
    }
    pub fn board(&self) -> String {
        format!("{}.{}/{}", self.tenant, self.wd, self.site)
    }
    fn api(&self) -> String {
        format!("https://{}/wday/cxs/{}/{}", self.host(), self.tenant, self.site)
    }
}

pub fn parse_url(url: &str) -> Option<Site> {
    let c = SITE_URL.captures(url)?;
    let site = c[3].to_string();
    if site.eq_ignore_ascii_case("wday") {
        return None;
    }
    Some(Site {
        tenant: c[1].to_lowercase(),
        wd: c[2].to_lowercase(),
        site,
        job_path: c.get(4).map(|m| m.as_str().to_string()),
    })
}

/// "generalmotors.wd5/Careers_GM" → Site.
pub fn parse_board(board: &str) -> Option<Site> {
    let (host, site) = board.split_once('/')?;
    let (tenant, wd) = host.split_once('.')?;
    Some(Site { tenant: tenant.into(), wd: wd.into(), site: site.into(), job_path: None })
}

/// The JSON endpoint behind a public job link, for reading its description.
pub fn detail_api_url(job_url: &str) -> Option<String> {
    let s = parse_url(job_url)?;
    let path = s.job_path.as_deref()?;
    Some(format!("{}{}", s.api(), path))
}

/// Description text from a detail response.
pub fn description_from_detail(v: &Value) -> Option<String> {
    let html = v["jobPostingInfo"]["jobDescription"].as_str()?;
    Some(crate::describe::html_to_text(html))
}

/// "Posted 23 Days Ago" / "Posted Today" / "Posted 30+ Days Ago" → a date.
fn posted_date(s: &str) -> String {
    let today = Local::now().date_naive();
    let Some(c) = POSTED.captures(s) else { return String::new() };
    let days = match c[1].to_lowercase().as_str() {
        "today" => 0,
        "yesterday" => 1,
        _ => c.get(2).and_then(|n| n.as_str().parse().ok()).unwrap_or(0),
    };
    (today - Duration::days(days)).format("%Y-%m-%d").to_string()
}

pub fn fetch_board(client: &reqwest::blocking::Client, board: &str, company: &str) -> Result<Vec<Posting>> {
    let Some(site) = parse_board(board) else {
        bail!("not a Workday board: {board}");
    };
    let search = |term: &str| -> Result<Vec<Posting>> {
        let mut found = Vec::new();
        for page in 0..MAX_PAGES {
            let v: Value = client
                .post(format!("{}/jobs", site.api()))
                .header("Accept", "application/json")
                .json(&json!({ "appliedFacets": {}, "limit": PAGE, "offset": page * PAGE, "searchText": term }))
                .send()?
                .error_for_status()?
                .json()?;
            let jobs = v["jobPostings"].as_array().cloned().unwrap_or_default();
            let before = found.len();
            for job in &jobs {
                let Some(path) = job["externalPath"].as_str() else { continue };
                let title = job["title"].as_str().unwrap_or("").trim().to_string();
                if !EARLY_CAREER.is_match(&title) {
                    continue;
                }
                let season = SEASON
                    .captures(&title)
                    .map(|c| {
                        let mut w = c[1].to_lowercase();
                        w[..1].make_ascii_uppercase();
                        let y = &c[2];
                        format!("{w} {}", if y.len() == 2 { format!("20{y}") } else { y.to_string() })
                    })
                    .unwrap_or_default();
                found.push(Posting {
                    company: company.to_string(),
                    title,
                    location: job["locationsText"].as_str().unwrap_or("").to_string(),
                    url: format!("https://{}/{}{}", site.host(), site.site, path),
                    source: format!("workday:{}", site.board()),
                    season,
                    posted: posted_date(job["postedOn"].as_str().unwrap_or("")),
                    description: String::new(),
                    deadline: String::new(),
                });
            }
            // Relevance-ordered: on GM, 90% of real internships sit in the
            // first six pages, so a dry page past the first few ends it.
            let dry = found.len() == before && page >= 3;
            if jobs.len() < PAGE || dry {
                break;
            }
        }
        Ok(found)
    };
    let results: Vec<Result<Vec<Posting>>> = std::thread::scope(|scope| {
        let handles: Vec<_> = SEARCHES.iter().map(|t| scope.spawn(move || search(t))).collect();
        handles.into_iter().map(|h| h.join().unwrap_or_else(|_| bail_thread())).collect()
    });
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for r in results {
        for p in r? {
            if seen.insert(p.url.clone()) {
                out.push(p);
            }
        }
    }
    Ok(out)
}

fn bail_thread() -> Result<Vec<Posting>> {
    bail!("Workday search thread failed")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_site_and_job_links() {
        let s = parse_url("https://generalmotors.wd5.myworkdayjobs.com/en-CA/Careers_GM/job/Warren-Michigan/XMLNAME-Intern_JR-202621695?utm_source=Simplify").unwrap();
        assert_eq!(s.board(), "generalmotors.wd5/Careers_GM");
        assert_eq!(
            detail_api_url("https://generalmotors.wd5.myworkdayjobs.com/en-CA/Careers_GM/job/Warren-Michigan/XMLNAME-Intern_JR-202621695?utm_source=Simplify").as_deref(),
            Some("https://generalmotors.wd5.myworkdayjobs.com/wday/cxs/generalmotors/Careers_GM/job/Warren-Michigan/XMLNAME-Intern_JR-202621695")
        );
        assert_eq!(parse_url("https://nvidia.wd5.myworkdayjobs.com/NVIDIAExternalCareerSite").unwrap().board(), "nvidia.wd5/NVIDIAExternalCareerSite");
        assert!(detail_api_url("https://nvidia.wd5.myworkdayjobs.com/NVIDIAExternalCareerSite").is_none());
        assert!(parse_url("https://boards.greenhouse.io/stripe").is_none());
    }

    #[test]
    fn reads_relative_posting_dates() {
        let today = Local::now().date_naive();
        assert_eq!(posted_date("Posted Today"), today.format("%Y-%m-%d").to_string());
        assert_eq!(posted_date("Posted 23 Days Ago"), (today - Duration::days(23)).format("%Y-%m-%d").to_string());
        assert_eq!(posted_date("Posted 30+ Days Ago"), (today - Duration::days(30)).format("%Y-%m-%d").to_string());
        assert_eq!(posted_date(""), "");
    }
}
