//! vanshb03/Summer2027-Internships (maintained with Ouckah): a second
//! community list. A Markdown table:
//! | Company | Role | Location | Application/Link | Date Posted |
//! "↳" repeats the previous company; 🔒 = closed. Unlike SimplifyJobs, it
//! really uses 🛂 (no sponsorship) and 🇺🇸 (citizens only) in the role cell,
//! which the authorization filter reads.

use crate::model::Posting;
use anyhow::{bail, Result};
use chrono::{Datelike, NaiveDate};
use regex::Regex;
use std::sync::LazyLock;

pub const REPO: &str = "vanshb03/Summer2027-Internships";
const SEASON: &str = "Summer 2027";

static MD_LINK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[([^\]]+)\]\(([^)]+)\)").unwrap());
static HREF: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"href="([^"]+)""#).unwrap());
static TAGS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<[^>]+>").unwrap());

pub fn fetch(client: &reqwest::blocking::Client) -> Result<Vec<Posting>> {
    for branch in ["dev", "main"] {
        let url = format!("https://raw.githubusercontent.com/{REPO}/{branch}/README.md");
        if let Ok(resp) = client.get(&url).send() {
            if resp.status().is_success() {
                let postings = parse(&resp.text()?, chrono::Local::now().date_naive());
                if !postings.is_empty() {
                    return Ok(postings);
                }
            }
        }
    }
    bail!("the vanshb03 internship list yielded no postings")
}

fn cell_text(cell: &str) -> String {
    let s = cell.replace("</br>", ", ").replace("<br>", ", ").replace("<br/>", ", ");
    let s = MD_LINK.replace_all(&s, "$1");
    TAGS.replace_all(&s, "")
        .replace("**", "")
        .replace("&amp;", "&")
        .trim()
        .to_string()
}

/// "Apr 19" → the most recent such date on or before today.
fn posted_date(s: &str, today: NaiveDate) -> String {
    let s = s.trim();
    for year in [today.year(), today.year() - 1] {
        if let Ok(d) = NaiveDate::parse_from_str(&format!("{s} {year}"), "%b %d %Y") {
            if d <= today {
                return d.format("%Y-%m-%d").to_string();
            }
        }
    }
    String::new()
}

fn parse(md: &str, today: NaiveDate) -> Vec<Posting> {
    let mut out = Vec::new();
    let mut last_company = String::new();
    let mut in_table = false;
    for line in md.lines() {
        let line = line.trim();
        if !line.starts_with('|') {
            in_table = false;
            continue;
        }
        let cells: Vec<&str> = line.trim_matches('|').split('|').map(str::trim).collect();
        if cells.first().is_some_and(|c| c.eq_ignore_ascii_case("company")) {
            in_table = true;
            continue;
        }
        if !in_table || cells.len() < 5 || cells.iter().all(|c| c.chars().all(|ch| "-: ".contains(ch))) {
            continue;
        }
        if line.contains('\u{1F512}') {
            continue; // 🔒 closed
        }
        let company = if cells[0].contains('\u{21B3}') { last_company.clone() } else { cell_text(cells[0]) };
        if company.is_empty() {
            continue;
        }
        last_company = company.clone();
        let title = cell_text(cells[1]);
        let Some(url) = HREF
            .captures(cells[3])
            .map(|c| c[1].to_string())
            .or_else(|| MD_LINK.captures(cells[3]).map(|c| c[2].to_string()))
        else {
            continue;
        };
        if title.is_empty() {
            continue;
        }
        out.push(Posting {
            company,
            title,
            location: cell_text(cells[2]),
            url,
            source: format!("github:{REPO}"),
            season: SEASON.to_string(),
            posted: posted_date(cells[4], today),
            description: String::new(),
            deadline: String::new(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
Intro text | with a pipe that is not a table
| Company | Role | Location | Application/Link | Date Posted |
| ------- | ---- | -------- | ---------------- | ----------- |
| **[Vertiv](https://vertiv.com)** | Product Management Intern 🛂 | Westerville, OH | <a href=\"https://egup.fa.us2.oraclecloud.com/job/1?utm_source=x\"><img src=\"a.png\"></a> | Oct 05 |
| ↳ | Hardware Intern 🇺🇸 | Delaware, OH</br>Austin, TX | <a href=\"https://egup.fa.us2.oraclecloud.com/job/2\"><img></a> | Sep 30 |
| Closed Co | Old Intern 🔒 | NYC | <a href=\"https://x.test/3\"></a> | Jan 02 |
| Nuclear Co | Research Intern | Washington, DC | <a href=\"https://job-boards.greenhouse.io/nuc/jobs/5391923008\"></a> | Nov 20 |
";

    #[test]
    fn parses_rows_markers_and_dates() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 7).unwrap();
        let p = parse(SAMPLE, today);
        assert_eq!(p.len(), 3, "closed row skipped");
        assert_eq!(p[0].company, "Vertiv");
        assert_eq!(p[0].posted, "2026-10-05");
        assert_eq!(p[0].auth_requirement(), crate::auth::NO_SPONSORSHIP);
        assert_eq!(p[1].company, "Vertiv", "↳ repeats the company");
        assert_eq!(p[1].location, "Delaware, OH, Austin, TX");
        assert_eq!(p[1].auth_requirement(), crate::auth::CITIZENSHIP);
        // "Nov 20" hasn't happened yet this year, so it's last year's.
        assert_eq!(p[2].posted, "2025-11-20");
        assert_eq!(p[2].job_key().as_deref(), Some("gh:5391923008"));
    }
}
