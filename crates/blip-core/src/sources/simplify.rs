//! SimplifyJobs community internship lists on GitHub — the highest-signal
//! free source. Their README holds HTML tables (one per category):
//! <tr> <td>company link</td> <td>role</td> <td>location</td>
//!      <td>apply links</td> <td>age "0d"/"2mo"</td> </tr>
//! "↳" in the company cell means "same company as previous row"; 🔒 = closed.

use crate::model::Posting;
use anyhow::{anyhow, Result};
use regex::Regex;

/// Tried in order; first README that yields postings wins. Newer season
/// repos get created over time, so keep the freshest first.
const REPOS: &[(&str, &str)] = &[
    ("SimplifyJobs/Summer2027-Internships", "Summer 2027"),
    ("SimplifyJobs/Summer2026-Internships", "Summer 2026"),
];

pub fn fetch(client: &reqwest::blocking::Client) -> Result<Vec<Posting>> {
    for (repo, season) in REPOS {
        let url = format!("https://raw.githubusercontent.com/{repo}/dev/README.md");
        match client.get(&url).send() {
            Ok(resp) if resp.status().is_success() => {
                let body = resp.text()?;
                let postings = parse_html_rows(&body, season, repo);
                if !postings.is_empty() {
                    return Ok(postings);
                }
            }
            _ => continue,
        }
    }
    Err(anyhow!("no SimplifyJobs README yielded postings"))
}

fn parse_html_rows(html: &str, season: &str, repo: &str) -> Vec<Posting> {
    let td = Regex::new(r"(?s)<td[^>]*>(.*?)</td>").unwrap();
    let href = Regex::new(r#"href="([^"]+)""#).unwrap();
    let tags = Regex::new(r"(?s)<[^>]+>").unwrap();

    let clean = |cell: &str| -> String {
        tags.replace_all(&cell.replace("<br>", ", ").replace("</br>", ", "), "")
            .replace("&amp;", "&")
            .trim()
            // Simplify decorates hot roles with 🔥 and similar markers.
            .trim_start_matches(|c: char| !c.is_alphanumeric())
            .trim()
            .to_string()
    };

    let mut out = Vec::new();
    let mut last_company = String::new();

    for row in html.split("<tr").skip(1) {
        let row = match row.split("</tr>").next() {
            Some(r) => r,
            None => continue,
        };
        if row.contains('\u{1F512}') {
            continue; // 🔒 closed posting
        }
        let cells: Vec<&str> = td.captures_iter(row).map(|c| c.get(1).unwrap().as_str()).collect();
        if cells.len() < 5 {
            continue;
        }

        let company = if cells[0].contains('\u{21B3}') {
            last_company.clone()
        } else {
            clean(cells[0])
        };
        if company.is_empty() {
            continue;
        }
        last_company = company.clone();

        let title = clean(cells[1]);
        let location = clean(cells[2]);
        // First href in the apply cell is the company's own application link
        // (the second is Simplify's autofill link).
        let Some(url) = href.captures(cells[3]).map(|c| c[1].to_string()) else {
            continue;
        };
        let posted = clean(cells[4]); // age like "0d", "1mo"

        if title.is_empty() {
            continue;
        }
        out.push(Posting {
            company,
            title,
            location,
            url,
            source: format!("github:{repo}"),
            season: season.to_string(),
            posted,
            description: String::new(),
        });
    }
    out
}
