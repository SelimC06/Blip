//! Lever public postings API — official, unauthenticated, per company:
//! https://api.lever.co/v0/postings/{board}?mode=json
//! Responses include the plain-text description, so no page fetch.

use crate::model::Posting;
use anyhow::{bail, Result};
use regex::Regex;
use std::sync::LazyLock;

static EARLY_CAREER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(intern(ship)?s?|co-? ?op|new ?grad|university|campus|early career)\b").unwrap()
});
static SEASON: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(summer|fall|spring|winter)\s*'?(20\d\d|\d\d)\b").unwrap());

pub fn fetch_board(client: &reqwest::blocking::Client, board: &str, company: &str) -> Result<Vec<Posting>> {
    let url = format!("https://api.lever.co/v0/postings/{board}?mode=json");
    let v: serde_json::Value = client.get(&url).send()?.error_for_status()?.json()?;
    let Some(jobs) = v.as_array() else {
        bail!("unexpected response from Lever");
    };

    let mut out = Vec::new();
    for job in jobs {
        let title = job["text"].as_str().unwrap_or("").trim().to_string();
        let commitment = job["categories"]["commitment"].as_str().unwrap_or("");
        if !(EARLY_CAREER.is_match(&title) || EARLY_CAREER.is_match(commitment)) {
            continue;
        }
        let season = SEASON
            .captures(&title)
            .map(|c| {
                let mut word = c[1].to_lowercase();
                word[..1].make_ascii_uppercase();
                let yr = &c[2];
                let yr = if yr.len() == 2 { format!("20{yr}") } else { yr.to_string() };
                format!("{word} {yr}")
            })
            .unwrap_or_default();
        // allLocations lists every office; fall back to the single location.
        let location = job["categories"]["allLocations"]
            .as_array()
            .map(|a| a.iter().filter_map(|l| l.as_str()).collect::<Vec<_>>().join(", "))
            .filter(|s| !s.is_empty())
            .or_else(|| job["categories"]["location"].as_str().map(String::from))
            .unwrap_or_default();
        let posted = job["createdAt"]
            .as_i64()
            .and_then(|ms| chrono::DateTime::from_timestamp_millis(ms))
            .map(|d| d.to_rfc3339())
            .unwrap_or_default();
        let description = [
            job["descriptionPlain"].as_str().unwrap_or(""),
            &job["lists"]
                .as_array()
                .map(|lists| {
                    lists
                        .iter()
                        .map(|l| {
                            let items = crate::describe::html_to_text(l["content"].as_str().unwrap_or(""));
                            format!("{}\n{}", l["text"].as_str().unwrap_or(""), items)
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default(),
            job["additionalPlain"].as_str().unwrap_or(""),
        ]
        .join("\n")
        .trim()
        .to_string();

        out.push(Posting {
            company: company.to_string(),
            title,
            location,
            url: job["hostedUrl"].as_str().unwrap_or("").to_string(),
            source: format!("lever:{board}"),
            season,
            posted,
            description,
            deadline: String::new(),
        });
    }
    Ok(out)
}
