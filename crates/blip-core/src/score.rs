//! Two-stage ranking: free hard filters, then embedding similarity cuts the
//! field to ~20, then the LLM deep-reads only those survivors.

use crate::config::Config;
use crate::llm::Llm;
use crate::model::Posting;
use crate::profile::Profile;
use anyhow::Result;
use chrono::{NaiveDate, Utc};
use regex::Regex;

#[derive(Debug)]
pub struct Scored {
    pub posting: Posting,
    pub score: u8,
    pub reason: String,
    pub red_flags: Vec<String>,
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

pub fn rank(
    llm: &Llm,
    cfg: &Config,
    profile: &Profile,
    candidates: Vec<Posting>,
    top_n: usize,
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

    // Stage 2: LLM scores each survivor.
    eprintln!("scoring {} candidates with {}…", by_sim.len(), chat_model_name(cfg));
    let system = "You are Blip, scoring how well a job posting fits a candidate. \
                  Respond with only a JSON object: \
                  {\"score\": <integer 0-100>, \"reason\": \"<one short sentence>\", \
                  \"red_flags\": [\"<anything disqualifying, often empty>\"]}";
    let profile_str = serde_json::to_string(&profile.data).unwrap_or_default();

    let mut scored = Vec::new();
    for (_, p) in by_sim {
        let user = format!(
            "CANDIDATE PROFILE: {profile_str}\n\
             CANDIDATE IS LOOKING FOR: {}\n\
             POSTING: {} — {} — {}{}",
            cfg.looking_for,
            p.company,
            p.title,
            p.location,
            if p.season.is_empty() { String::new() } else { format!(" — {}", p.season) },
        );
        match llm.chat_json(system, &user) {
            Ok(v) => scored.push(Scored {
                score: v["score"].as_u64().unwrap_or(0).min(100) as u8,
                reason: v["reason"].as_str().unwrap_or("").to_string(),
                red_flags: v["red_flags"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                    .unwrap_or_default(),
                posting: p,
            }),
            Err(e) => eprintln!("  ⚠ scoring {} — {}: {e}", p.company, p.title),
        }
    }

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

fn chat_model_name(cfg: &Config) -> String {
    if cfg.backend == "anthropic" {
        cfg.anthropic_model.clone()
    } else {
        cfg.chat_model.clone()
    }
}
