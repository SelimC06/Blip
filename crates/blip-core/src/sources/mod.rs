pub mod amazon;
pub mod ashby;
pub mod greenhouse;
pub mod lever;
pub mod oracle;
pub mod simplify;
pub mod usajobs;
pub mod vansh;
pub mod workday;

use crate::config::CompanyEntry;
use crate::model::Posting;
use anyhow::{anyhow, bail, Result};
use regex::Regex;
use std::sync::LazyLock;

pub const PLATFORMS: [&str; 3] = ["greenhouse", "ashby", "lever"];

/// Read one company's board on whichever platform it uses.
pub fn fetch_company(client: &reqwest::blocking::Client, c: &CompanyEntry) -> Result<Vec<Posting>> {
    match c.platform.as_str() {
        "greenhouse" => greenhouse::fetch_board(client, &c.board, &c.name),
        "ashby" => ashby::fetch_board(client, &c.board, &c.name),
        "lever" => lever::fetch_board(client, &c.board, &c.name),
        "workday" => workday::fetch_board(client, &c.board, &c.name),
        "oracle" => oracle::fetch_board(client, &c.board, &c.name),
        other => bail!("unknown job board platform \"{other}\""),
    }
}

/// A board found from what the user typed, with how many early-career roles
/// it has right now.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Found {
    pub entry: CompanyEntry,
    pub roles: usize,
}

static BOARD_URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(?:(?:boards|job-boards)\.greenhouse\.io/(?:embed/job_board\?for=)?|boards-api\.greenhouse\.io/v1/boards/|(?:jobs|api)\.ashbyhq\.com/(?:posting-api/job-board/)?|(?:jobs|api)\.lever\.co/(?:v0/postings/)?)([a-z0-9][a-z0-9._-]*)",
    )
    .unwrap()
});

/// "https://jobs.lever.co/palantir/…" → ("lever", "palantir"). Workday and
/// Oracle boards come back as "tenant.wdN/site" and "host/siteNumber".
pub fn parse_board_url(s: &str) -> Option<(&'static str, String)> {
    let with_scheme = if s.contains("://") { s.to_string() } else { format!("https://{s}") };
    if let Some(site) = workday::parse_url(&with_scheme) {
        return Some(("workday", site.board()));
    }
    if let Some(site) = oracle::parse_url(&with_scheme) {
        return Some(("oracle", site.board()));
    }
    let caps = BOARD_URL.captures(s)?;
    let whole = caps.get(0)?.as_str().to_lowercase();
    let platform = if whole.contains("greenhouse") {
        "greenhouse"
    } else if whole.contains("ashbyhq") {
        "ashby"
    } else {
        "lever"
    };
    Some((platform, caps[1].to_lowercase()))
}

/// Slugs a company name is likely to use: "Scale AI" → scaleai, scale-ai, scale.
fn slug_guesses(name: &str) -> Vec<String> {
    let words: Vec<String> = name
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect();
    let mut out = vec![words.concat(), words.join("-")];
    if let Some(first) = words.first() {
        out.push(first.clone());
    }
    out.dedup();
    out.retain(|s| !s.is_empty());
    out
}

fn title_case(slug: &str) -> String {
    slug.split(['-', '_', '.'])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Greenhouse publishes the company's display name; the others don't.
fn official_name(client: &reqwest::blocking::Client, platform: &str, board: &str) -> Option<String> {
    if platform != "greenhouse" {
        return None;
    }
    let v: serde_json::Value = client
        .get(format!("https://boards-api.greenhouse.io/v1/boards/{board}"))
        .send()
        .ok()?
        .json()
        .ok()?;
    v["name"].as_str().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Turn a careers URL or a company name into a verified board. Names are
/// tried against all three platforms at once; the first that answers wins
/// (in Greenhouse, Ashby, Lever order).
pub fn find_company(client: &reqwest::blocking::Client, query: &str) -> Result<Found> {
    let query = query.trim();
    if query.is_empty() {
        bail!("Type a company name or paste its careers page link.");
    }
    // "General Motors https://…" names a board whose platform can't name itself.
    let link = query.split_whitespace().find(|t| t.contains('/') && t.contains('.'));
    let given_name = link.map(|l| query.replace(l, " ").split_whitespace().collect::<Vec<_>>().join(" "));
    let given_name = given_name.filter(|n| !n.is_empty());
    let (candidates, typed_name): (Vec<(&str, String)>, Option<String>) = match link.and_then(parse_board_url) {
        Some((platform, board)) => (vec![(platform, board)], given_name),
        None if link.is_some() => {
            bail!("That link isn't a Greenhouse, Ashby, Lever, Workday, or Oracle job board. Try typing the company's name instead.")
        }
        None => {
            let guesses = slug_guesses(query);
            let cands = PLATFORMS
                .iter()
                .flat_map(|p| guesses.iter().map(move |s| (*p, s.clone())))
                .collect();
            (cands, Some(query.to_string()))
        }
    };

    let results: Vec<Option<(usize, Vec<Posting>)>> = std::thread::scope(|scope| {
        let handles: Vec<_> = candidates
            .iter()
            .enumerate()
            .map(|(i, (platform, board))| {
                scope.spawn(move || {
                    let entry = CompanyEntry {
                        platform: platform.to_string(),
                        board: board.clone(),
                        name: String::new(),
                    };
                    fetch_company(client, &entry).ok().map(|p| (i, p))
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().ok().flatten()).collect()
    });

    let (i, postings) = results
        .into_iter()
        .flatten()
        .min_by_key(|(i, _)| *i)
        .ok_or_else(|| match &typed_name {
            Some(n) => anyhow!(
                "Couldn't find \"{n}\" on Greenhouse, Ashby, or Lever. Paste a link to its careers page instead (Workday and Oracle sites need a link)."
            ),
            None => anyhow!("That job board doesn't exist or isn't public."),
        })?;
    let (platform, board) = &candidates[i];
    // Workday/Oracle boards look like "generalmotors.wd5/Careers_GM"; the
    // tenant is the closest thing to a name they have.
    let slug = board.split(['.', '/']).next().unwrap_or(board);
    let name = official_name(client, platform, board)
        .or(typed_name)
        .unwrap_or_else(|| title_case(slug));
    Ok(Found {
        entry: CompanyEntry { platform: platform.to_string(), board: board.clone(), name },
        roles: postings.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_board_urls_from_all_three_platforms() {
        assert_eq!(parse_board_url("https://boards.greenhouse.io/stripe"), Some(("greenhouse", "stripe".into())));
        assert_eq!(parse_board_url("https://job-boards.greenhouse.io/gleanwork/jobs/4595665005"), Some(("greenhouse", "gleanwork".into())));
        assert_eq!(parse_board_url("https://boards.greenhouse.io/embed/job_board?for=Discord"), Some(("greenhouse", "discord".into())));
        assert_eq!(parse_board_url("https://jobs.ashbyhq.com/notion/e66c6658-9e65-4c58"), Some(("ashby", "notion".into())));
        assert_eq!(parse_board_url("jobs.lever.co/palantir/6ed76ce8"), Some(("lever", "palantir".into())));
        assert_eq!(parse_board_url("https://careers.google.com/jobs"), None);
        assert_eq!(
            parse_board_url("https://generalmotors.wd5.myworkdayjobs.com/en-US/Careers_GM/job/X_JR-1"),
            Some(("workday", "generalmotors.wd5/Careers_GM".into()))
        );
        assert_eq!(
            parse_board_url("jpmc.fa.oraclecloud.com/hcmUI/CandidateExperience/en/sites/CX_1001/requisitions"),
            Some(("oracle", "jpmc.fa.oraclecloud.com/CX_1001".into()))
        );
    }

    #[test]
    fn guesses_slugs_from_names() {
        assert_eq!(slug_guesses("Scale AI"), vec!["scaleai", "scale-ai", "scale"]);
        assert_eq!(slug_guesses("Stripe"), vec!["stripe"]);
        assert_eq!(title_case("scale-ai"), "Scale Ai");
    }
}
