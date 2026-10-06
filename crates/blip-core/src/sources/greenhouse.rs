//! Greenhouse public job-board JSON — official, unauthenticated, per company:
//! https://boards-api.greenhouse.io/v1/boards/{board}/jobs
//! Phase 3 moves this watchlist into user settings.

use crate::model::Posting;
use anyhow::Result;
use regex::Regex;

// Ramp/OpenAI/Notion use Ashby, not Greenhouse — an Ashby source is a
// Phase 4 addition; this list moves to user settings in Phase 3.
pub const WATCHLIST: &[(&str, &str)] = &[
    ("stripe", "Stripe"),
    ("datadog", "Datadog"),
    ("databricks", "Databricks"),
    ("duolingo", "Duolingo"),
    ("figma", "Figma"),
    ("cloudflare", "Cloudflare"),
    ("robinhood", "Robinhood"),
    ("discord", "Discord"),
];

pub fn fetch_board(
    client: &reqwest::blocking::Client,
    board: &str,
    company: &str,
) -> Result<Vec<Posting>> {
    let url = format!("https://boards-api.greenhouse.io/v1/boards/{board}/jobs");
    let v: serde_json::Value = client.get(&url).send()?.error_for_status()?.json()?;
    let season_re = Regex::new(r"(?i)(summer|fall|spring|winter)\s*'?(20\d\d|\d\d)\b").unwrap();
    // Word boundaries matter: a bare "intern" substring also matches
    // "Internal Audit" and floods the results.
    let early_career =
        Regex::new(r"(?i)\b(intern(ship)?s?|co-? ?op|new ?grad|university|campus|early career)\b")
            .unwrap();

    let mut out = Vec::new();
    for job in v["jobs"].as_array().cloned().unwrap_or_default() {
        let title = job["title"].as_str().unwrap_or("").trim().to_string();
        if !early_career.is_match(&title) {
            continue;
        }
        let season = season_re
            .captures(&title)
            .map(|c| {
                let mut word = c[1].to_string();
                if let Some(f) = word.get_mut(0..1) {
                    f.make_ascii_uppercase();
                }
                let yr = &c[2];
                let yr = if yr.len() == 2 { format!("20{yr}") } else { yr.to_string() };
                format!("{word} {yr}")
            })
            .unwrap_or_default();
        out.push(Posting {
            company: company.to_string(),
            title,
            location: job["location"]["name"].as_str().unwrap_or("").to_string(),
            url: job["absolute_url"].as_str().unwrap_or("").to_string(),
            source: format!("greenhouse:{board}"),
            season,
            posted: job["updated_at"].as_str().unwrap_or("").to_string(),
            description: String::new(),
        });
    }
    Ok(out)
}
