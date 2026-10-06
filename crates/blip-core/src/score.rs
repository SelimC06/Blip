//! Two-stage ranking: free hard filters, then embedding similarity cuts the
//! field to ~20, then the LLM deep-reads only those survivors.

use crate::config::Config;
use crate::describe;
use crate::llm::Llm;
use crate::model::{Cancelled, Posting};
use crate::profile::Profile;
use crate::store::Store;
use anyhow::Result;
use chrono::{Local, NaiveDate, Utc};
use regex::Regex;

#[derive(Debug)]
pub struct Scored {
    pub posting: Posting,
    pub score: u8,
    pub reason: String,
    pub red_flags: Vec<String>,
    /// Application deadline (YYYY-MM-DD), only when the description states one.
    pub deadline: Option<String>,
}

/// Small models will invent deadlines. Keep one only if it parses, lies in
/// the next year, and the description actually talks about a deadline.
pub fn validate_deadline(raw: Option<&str>, description: &str) -> Option<String> {
    let raw = raw?.trim();
    let date = NaiveDate::parse_from_str(raw, "%Y-%m-%d").ok()?;
    let today = Local::now().date_naive();
    if date < today || date > today + chrono::Duration::days(365) {
        return None;
    }
    let mentions = Regex::new(
        r"(?i)\b(deadline|apply by|applications? (close|due|accepted until|will be accepted)|closing date|closes on|no later than)\b",
    )
    .unwrap();
    mentions.is_match(description).then(|| date.format("%Y-%m-%d").to_string())
}

/// Days from today until a YYYY-MM-DD deadline.
pub fn days_until(deadline: &str) -> Option<i64> {
    let d = NaiveDate::parse_from_str(deadline, "%Y-%m-%d").ok()?;
    Some((d - Local::now().date_naive()).num_days())
}

/// Posting age in days from either source's format: Simplify ages like
/// "0d" / "3w" / "2mo", or Greenhouse ISO timestamps. None = unknown (passes).
pub fn age_days(posted: &str) -> Option<f64> {
    let posted = posted.trim();
    if posted.is_empty() {
        return None;
    }
    let rel = Regex::new(r"^(\d+)\s*(h|d|w|mo)$").unwrap();
    if let Some(c) = rel.captures(posted) {
        let n: f64 = c[1].parse().ok()?;
        return Some(match &c[2] {
            "h" => n / 24.0,
            "d" => n,
            "w" => n * 7.0,
            _ => n * 30.0, // "mo"
        });
    }
    if posted.len() >= 10 {
        if let Ok(date) = NaiveDate::parse_from_str(&posted[..10], "%Y-%m-%d") {
            return Some((Utc::now().date_naive() - date).num_days() as f64);
        }
    }
    None
}

pub fn hard_filter(cfg: &Config, p: &Posting) -> bool {
    // 🎓 on the Simplify list = advanced degree (MS/PhD) required.
    if cfg.exclude_advanced_degree && p.title.contains('\u{1F393}') {
        return false;
    }
    if let Some(age) = age_days(&p.posted) {
        if age > cfg.max_age_days {
            return false;
        }
    }
    // Season mismatch only disqualifies when both sides state one.
    if !p.season.is_empty() && !cfg.season.is_empty() && p.season != cfg.season {
        return false;
    }
    if !cfg.role_types.is_empty() {
        let title = p.title.to_lowercase();
        let matches_type = cfg.role_types.iter().any(|t| {
            let pat = match t.as_str() {
                "internship" => r"\bintern(ship)?s?\b",
                "co-op" => r"\bco-? ?op\b",
                "new-grad" => r"\b(new ?grad|university grad|early career)\b",
                _ => return false,
            };
            Regex::new(&format!("(?i){pat}")).unwrap().is_match(&title)
        });
        if !matches_type {
            return false;
        }
    }
    true
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

/// Rank candidates and return the top `top_n`. Every scored posting (not
/// just the top) is saved back to the store with its score and deadline.
/// `cancelled` is polled before each LLM call.
pub fn rank(
    llm: &Llm,
    cfg: &Config,
    profile: &Profile,
    store: &Store,
    candidates: Vec<Posting>,
    top_n: usize,
    cancelled: &dyn Fn() -> bool,
) -> Result<Vec<Scored>> {
    let filtered: Vec<Posting> = candidates
        .into_iter()
        .filter(|p| hard_filter(cfg, p))
        .collect();
    if filtered.is_empty() {
        return Ok(vec![]);
    }

    // Stage 1: embedding similarity prefilter.
    eprintln!("prefiltering {} postings by embedding…", filtered.len());
    let texts: Vec<String> = filtered
        .iter()
        .map(|p| format!("{} at {} — {}", p.title, p.company, p.location))
        .collect();
    let embs = llm.embed(&texts)?;
    let mut by_sim: Vec<(f32, Posting)> = embs
        .iter()
        .zip(filtered)
        .map(|(e, p)| (cosine(e, &profile.embedding), p))
        .collect();
    by_sim.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    by_sim.truncate(cfg.prefilter_top);
    let mut shortlist: Vec<Posting> = by_sim.into_iter().map(|(_, p)| p).collect();

    // Fetch real job descriptions for the shortlist (cached in the store).
    let missing: Vec<(usize, String)> = shortlist
        .iter()
        .enumerate()
        .filter(|(_, p)| p.description.is_empty() && p.url.starts_with("http"))
        .map(|(i, p)| (i, p.url.clone()))
        .collect();
    if !missing.is_empty() {
        eprintln!("fetching {} job descriptions…", missing.len());
        for (i, text) in describe::fetch_missing(&missing) {
            store.save_description(&shortlist[i].fingerprint(), &text)?;
            shortlist[i].description = text;
        }
    }

    // Stage 2: LLM scores each survivor.
    eprintln!("scoring {} candidates with {}…", shortlist.len(), chat_model_name(cfg));
    let today = Local::now().format("%Y-%m-%d");
    let system = format!(
        "You are Blip, scoring how well a job posting fits a candidate. Today is {today}. \
         Internships and co-ops are for current students, so a graduation date after the \
         internship is normal and never a red flag. \
         Respond with only a JSON object: \
         {{\"score\": <integer 0-100>, \"reason\": \"<one short sentence naming the specific fit>\", \
         \"red_flags\": [\"<anything disqualifying, e.g. degree or citizenship requirements; often empty>\"], \
         \"deadline\": \"<YYYY-MM-DD only if the description explicitly states an application deadline, otherwise null>\"}}"
    );
    let profile_str = serde_json::to_string(&profile.data).unwrap_or_default();

    let mut scored = Vec::new();
    for p in shortlist {
        if cancelled() {
            return Err(Cancelled.into());
        }
        let description = if p.description.is_empty() {
            "(not available — judge from the title)".to_string()
        } else {
            describe::excerpt(&p.description)
        };
        let user = format!(
            "CANDIDATE PROFILE: {profile_str}\n\
             CANDIDATE IS LOOKING FOR: {}\n\
             POSTING: {} — {} — {}{}\n\
             DESCRIPTION:\n{description}",
            cfg.looking_for,
            p.company,
            p.title,
            p.location,
            if p.season.is_empty() { String::new() } else { format!(" — {}", p.season) },
        );
        match llm.chat_json(&system, &user) {
            Ok(v) => scored.push(Scored {
                score: v["score"].as_u64().unwrap_or(0).min(100) as u8,
                reason: v["reason"].as_str().unwrap_or("").to_string(),
                red_flags: v["red_flags"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(String::from))
                            .filter(|s| !s.trim().is_empty())
                            .collect()
                    })
                    .unwrap_or_default(),
                deadline: validate_deadline(v["deadline"].as_str(), &p.description),
                posting: p,
            }),
            Err(e) => eprintln!("  ⚠ scoring {} — {}: {e}", p.company, p.title),
        }
    }
    store.save_scores(&scored)?;

    // Final order: score, then freshness.
    scored.sort_by(|a, b| {
        b.score.cmp(&a.score).then(
            age_days(&a.posting.posted)
                .unwrap_or(f64::MAX)
                .partial_cmp(&age_days(&b.posting.posted).unwrap_or(f64::MAX))
                .unwrap_or(std::cmp::Ordering::Equal),
        )
    });
    scored.truncate(top_n);
    Ok(scored)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn in_days(n: i64) -> String {
        (Local::now().date_naive() + chrono::Duration::days(n)).format("%Y-%m-%d").to_string()
    }

    #[test]
    fn keeps_a_stated_future_deadline() {
        let d = in_days(10);
        assert_eq!(
            validate_deadline(Some(&d), "Applications close on that date. Apply by then."),
            Some(d)
        );
    }

    #[test]
    fn rejects_invented_past_and_far_deadlines() {
        let text = "Application deadline: see portal";
        assert_eq!(validate_deadline(Some(&in_days(10)), "We build rockets."), None);
        assert_eq!(validate_deadline(Some(&in_days(-3)), text), None);
        assert_eq!(validate_deadline(Some(&in_days(500)), text), None);
        assert_eq!(validate_deadline(Some("next friday"), text), None);
        assert_eq!(validate_deadline(None, text), None);
    }
}

fn chat_model_name(cfg: &Config) -> String {
    if cfg.backend == "anthropic" {
        cfg.anthropic_model.clone()
    } else {
        cfg.chat_model.clone()
    }
}
