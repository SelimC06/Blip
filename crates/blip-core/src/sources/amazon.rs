//! Amazon's own job site, amazon.jobs. Public JSON, sorted newest first,
//! with full descriptions included:
//! https://www.amazon.jobs/en/search.json?base_query=intern&sort=recent
//! Its `is_intern` flag is always empty, so roles are filtered by title.

use crate::model::Posting;
use anyhow::Result;
use chrono::{Local, NaiveDate};
use regex::Regex;
use serde_json::Value;
use std::collections::HashSet;
use std::sync::LazyLock;

const PAGE: usize = 100;
const MAX_PAGES: usize = 5;
const MAX_AGE_DAYS: i64 = 60;
const SEARCHES: [&str; 3] = ["intern", "co-op", "new grad"];

static EARLY_CAREER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(intern(ship)?s?|co-? ?op|new ?grad|university grad(uate)?|early career)\b").unwrap()
});
static SEASON: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(summer|fall|spring|winter)\s*'?(20\d\d|\d\d)\b").unwrap());

/// "October  7, 2026" (Amazon pads single-digit days) → "2026-10-07".
fn posted_date(s: &str) -> Option<NaiveDate> {
    let squashed = s.split_whitespace().collect::<Vec<_>>().join(" ");
    NaiveDate::parse_from_str(&squashed, "%B %d, %Y").ok()
}

pub fn fetch(client: &reqwest::blocking::Client) -> Result<Vec<Posting>> {
    let today = Local::now().date_naive();
    let search = |term: &str| -> Result<Vec<Posting>> {
        let mut found = Vec::new();
        'pages: for page in 0..MAX_PAGES {
            let url = format!(
                "https://www.amazon.jobs/en/search.json?base_query={}&result_limit={PAGE}&offset={}&sort=recent",
                term.replace(' ', "%20"),
                page * PAGE
            );
            let v: Value = client.get(&url).send()?.error_for_status()?.json()?;
            let jobs = v["jobs"].as_array().cloned().unwrap_or_default();
            for j in &jobs {
                let posted = posted_date(j["posted_date"].as_str().unwrap_or(""));
                if posted.is_some_and(|d| (today - d).num_days() > MAX_AGE_DAYS) {
                    break 'pages; // newest first: the rest are older
                }
                let title = j["title"].as_str().unwrap_or("").trim().to_string();
                if !EARLY_CAREER.is_match(&title) {
                    continue;
                }
                let Some(path) = j["job_path"].as_str() else { continue };
                let description = ["description", "basic_qualifications", "preferred_qualifications"]
                    .iter()
                    .filter_map(|k| j[*k].as_str())
                    .map(crate::describe::html_to_text)
                    .collect::<Vec<_>>()
                    .join("\n");
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
                    company: "Amazon".to_string(),
                    title,
                    location: j["normalized_location"]
                        .as_str()
                        .or_else(|| j["location"].as_str())
                        .unwrap_or("")
                        .to_string(),
                    url: format!("https://www.amazon.jobs{path}"),
                    source: "amazon".to_string(),
                    season,
                    posted: posted.map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default(),
                    description,
                    deadline: String::new(),
                });
            }
            if jobs.len() < PAGE {
                break;
            }
        }
        Ok(found)
    };
    let results: Vec<Result<Vec<Posting>>> = std::thread::scope(|scope| {
        let handles: Vec<_> = SEARCHES.iter().map(|t| scope.spawn(move || search(t))).collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_else(|_| Err(anyhow::anyhow!("Amazon search thread failed"))))
            .collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_amazon_dates() {
        assert_eq!(posted_date("October  7, 2026"), NaiveDate::from_ymd_opt(2026, 10, 7));
        assert_eq!(posted_date("September 24, 2026"), NaiveDate::from_ymd_opt(2026, 9, 24));
        assert_eq!(posted_date(""), None);
    }
}
