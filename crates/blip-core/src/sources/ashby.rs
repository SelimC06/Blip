//! Ashby public job-board API — used by Ramp, OpenAI, Notion and many
//! startups: https://api.ashbyhq.com/posting-api/job-board/{board}
//! Responses include the full plain-text description, so no page fetch.

use crate::model::Posting;
use anyhow::Result;
use regex::Regex;

static JOB_URL: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
    Regex::new(r"(?i)jobs\.ashbyhq\.com/([a-z0-9._-]+)/([0-9a-f-]{36})").unwrap()
});

/// Ashby job pages are JavaScript-only. For a job link (often from a
/// community list), read the board's public API and pull that job's
/// description by ID.
pub fn description_for(client: &reqwest::blocking::Client, job_url: &str) -> Option<String> {
    let c = JOB_URL.captures(job_url)?;
    let (board, id) = (c[1].to_string(), c[2].to_lowercase());
    let body = client
        .get(format!("https://api.ashbyhq.com/posting-api/job-board/{board}"))
        .send()
        .ok()?
        .error_for_status()
        .ok()?
        .text()
        .ok()?;
    let body: String = body.chars().map(|ch| if ch.is_control() && ch != '\n' && ch != '\t' { ' ' } else { ch }).collect();
    let v: serde_json::Value = serde_json::from_str(&body).ok()?;
    v["jobs"]
        .as_array()?
        .iter()
        .find(|j| j["id"].as_str().is_some_and(|x| x.eq_ignore_ascii_case(&id)) || j["jobUrl"].as_str().is_some_and(|u| u.to_lowercase().contains(&id)))?
        ["descriptionPlain"]
        .as_str()
        .map(|s| s.to_string())
        .filter(|s| s.len() >= 300)
}

pub fn fetch_board(
    client: &reqwest::blocking::Client,
    board: &str,
    company: &str,
) -> Result<Vec<Posting>> {
    let url = format!("https://api.ashbyhq.com/posting-api/job-board/{board}");
    // Ashby descriptions contain raw control characters, which serde_json
    // rejects; strip them before parsing.
    // Big boards (OpenAI: 800+ jobs with full descriptions) run past the
    // shared 20 s timeout.
    let body = client
        .get(&url)
        .timeout(std::time::Duration::from_secs(90))
        .send()?
        .error_for_status()?
        .text()?;
    let body: String = body
        .chars()
        .map(|c| if c.is_control() && c != '\n' && c != '\t' { ' ' } else { c })
        .collect();
    let v: serde_json::Value = serde_json::from_str(&body)?;

    let early_career = Regex::new(
        r"(?i)\b(intern(ship)?s?|co-? ?op|new ?grad|university|campus|early career)\b",
    )
    .unwrap();
    let season_re = Regex::new(r"(?i)(summer|fall|spring|winter)\s*'?(20\d\d|\d\d)\b").unwrap();

    let mut out = Vec::new();
    for job in v["jobs"].as_array().cloned().unwrap_or_default() {
        if job["isListed"].as_bool() == Some(false) {
            continue;
        }
        let title = job["title"].as_str().unwrap_or("").trim().to_string();
        let is_intern_type = job["employmentType"].as_str() == Some("Intern");
        if !(early_career.is_match(&title) || is_intern_type) {
            continue;
        }
        let season = season_re
            .captures(&title)
            .map(|c| {
                let mut word = c[1].to_lowercase();
                word[..1].make_ascii_uppercase();
                let yr = &c[2];
                let yr = if yr.len() == 2 { format!("20{yr}") } else { yr.to_string() };
                format!("{word} {yr}")
            })
            .unwrap_or_default();
        out.push(Posting {
            company: company.to_string(),
            title,
            location: job["location"].as_str().unwrap_or("").to_string(),
            url: job["jobUrl"].as_str().unwrap_or("").to_string(),
            source: format!("ashby:{board}"),
            season,
            posted: job["publishedAt"].as_str().unwrap_or("").to_string(),
            description: job["descriptionPlain"].as_str().unwrap_or("").to_string(),
            deadline: String::new(),
        });
    }
    Ok(out)
}
