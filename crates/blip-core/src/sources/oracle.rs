//! Oracle Recruiting Cloud career sites (JPMorgan, Oracle, many banks and
//! manufacturers). The REST endpoints the Candidate Experience pages use:
//!
//!   board  = "{host}/{siteNumber}"  e.g. "jpmc.fa.oraclecloud.com/CX_1001"
//!   search = GET https://{host}/hcmRestApi/resources/latest/recruitingCEJobRequisitions
//!   detail = GET https://{host}/hcmRestApi/resources/latest/recruitingCEJobRequisitionDetails
//!
//! Search can sort newest first, so paging stops at roles too old to matter.
//! Keyword search is loose ("intern" also returns analyst roles), so titles
//! are filtered here.

use crate::model::Posting;
use anyhow::{bail, Result};
use chrono::{Local, NaiveDate};
use regex::Regex;
use serde_json::Value;
use std::collections::HashSet;
use std::sync::LazyLock;

const PAGE: usize = 25;
const MAX_PAGES: usize = 6; // per search term, per scan
const MAX_AGE_DAYS: i64 = 60; // newest-first, so stop paging past this
const SEARCHES: [&str; 3] = ["intern", "co-op", "graduate"];

static SITE_URL: LazyLock<Regex> = LazyLock::new(|| {
    // https://egup.fa.us2.oraclecloud.com/hcmUI/CandidateExperience/en/sites/CX/job/20278933
    Regex::new(r"(?i)https?://([a-z0-9-]+\.fa(?:\.[a-z0-9-]+)?\.oraclecloud\.com)/hcmUI/CandidateExperience/[a-z]{2}(?:-[a-z]{2})?/sites/([a-z0-9_]+)(?:/job/(\d+))?").unwrap()
});
static EARLY_CAREER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(intern(ship)?s?|co-? ?op|new ?grad|university grad(uate)?|early career|graduate program|summer analyst|summer associate)\b").unwrap()
});
static SEASON: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(summer|fall|spring|winter)\s*'?(20\d\d|\d\d)\b").unwrap());

pub struct Site {
    pub host: String,
    pub site: String,
    pub job_id: Option<String>,
}

impl Site {
    pub fn board(&self) -> String {
        format!("{}/{}", self.host, self.site)
    }
    fn api(&self) -> String {
        format!("https://{}/hcmRestApi/resources/latest", self.host)
    }
}

pub fn parse_url(url: &str) -> Option<Site> {
    let c = SITE_URL.captures(url)?;
    Some(Site {
        host: c[1].to_lowercase(),
        site: c[2].to_string(),
        job_id: c.get(3).map(|m| m.as_str().to_string()),
    })
}

pub fn parse_board(board: &str) -> Option<Site> {
    let (host, site) = board.rsplit_once('/')?;
    Some(Site { host: host.into(), site: site.into(), job_id: None })
}

/// The JSON endpoint behind a public job link, for reading its description.
pub fn detail_api_url(job_url: &str) -> Option<String> {
    let s = parse_url(job_url)?;
    let id = s.job_id.as_deref()?;
    Some(format!(
        "{}/recruitingCEJobRequisitionDetails?expand=all&onlyData=true&finder=ById;Id=%22{id}%22,siteNumber={}",
        s.api(),
        s.site
    ))
}

/// Description text from a detail response (description, duties, and
/// qualifications are separate fields).
pub fn description_from_detail(v: &Value) -> Option<String> {
    let item = v["items"].as_array()?.first()?;
    let html = ["ExternalDescriptionStr", "ExternalResponsibilitiesStr", "ExternalQualificationsStr"]
        .iter()
        .filter_map(|k| item[*k].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let text = crate::describe::html_to_text(&html);
    (!text.is_empty()).then_some(text)
}

pub fn fetch_board(client: &reqwest::blocking::Client, board: &str, company: &str) -> Result<Vec<Posting>> {
    let Some(site) = parse_board(board) else {
        bail!("not an Oracle Recruiting board: {board}");
    };
    let today = Local::now().date_naive();
    let search = |term: &str| -> Result<Vec<Posting>> {
        let mut found = Vec::new();
        'pages: for page in 0..MAX_PAGES {
            let url = format!(
                "{}/recruitingCEJobRequisitions?onlyData=true&expand=requisitionList.secondaryLocations&finder=findReqs;siteNumber={},keyword={},limit={PAGE},offset={},sortBy=POSTING_DATES_DESC",
                site.api(),
                site.site,
                term.replace(' ', "%20"),
                page * PAGE
            );
            let v: Value = client.get(&url).send()?.error_for_status()?.json()?;
            let reqs = v["items"][0]["requisitionList"].as_array().cloned().unwrap_or_default();
            for r in &reqs {
                let posted = r["PostedDate"].as_str().unwrap_or("").to_string();
                if let Ok(d) = NaiveDate::parse_from_str(posted.get(..10).unwrap_or(""), "%Y-%m-%d") {
                    if (today - d).num_days() > MAX_AGE_DAYS {
                        break 'pages; // newest first: everything after is older
                    }
                }
                let Some(id) = r["Id"].as_str() else { continue };
                let title = r["Title"].as_str().unwrap_or("").trim().to_string();
                if !EARLY_CAREER.is_match(&title) {
                    continue;
                }
                let mut locations = vec![r["PrimaryLocation"].as_str().unwrap_or("").to_string()];
                for l in r["secondaryLocations"].as_array().into_iter().flatten() {
                    if let Some(name) = l["Name"].as_str() {
                        locations.push(name.to_string());
                    }
                }
                locations.retain(|l| !l.is_empty());
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
                    location: locations.join(", "),
                    url: format!("https://{}/hcmUI/CandidateExperience/en/sites/{}/job/{id}", site.host, site.site),
                    source: format!("oracle:{}", site.board()),
                    season,
                    posted,
                    description: String::new(),
                    deadline: String::new(),
                });
            }
            if reqs.len() < PAGE {
                break;
            }
        }
        Ok(found)
    };
    let results: Vec<Result<Vec<Posting>>> = std::thread::scope(|scope| {
        let handles: Vec<_> = SEARCHES.iter().map(|t| scope.spawn(move || search(t))).collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_else(|_| Err(anyhow::anyhow!("Oracle search thread failed"))))
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
    fn parses_site_and_job_links() {
        let s = parse_url("https://egup.fa.us2.oraclecloud.com/hcmUI/CandidateExperience/en/sites/CX/job/20278933?utm_source=x").unwrap();
        assert_eq!(s.board(), "egup.fa.us2.oraclecloud.com/CX");
        assert_eq!(s.job_id.as_deref(), Some("20278933"));
        assert!(detail_api_url("https://jpmc.fa.oraclecloud.com/hcmUI/CandidateExperience/en/sites/CX_1001/job/210791346")
            .unwrap()
            .contains("Id=%22210791346%22,siteNumber=CX_1001"));
        assert_eq!(
            parse_url("https://jpmc.fa.oraclecloud.com/hcmUI/CandidateExperience/en/sites/CX_1001/requisitions").unwrap().board(),
            "jpmc.fa.oraclecloud.com/CX_1001"
        );
        assert!(detail_api_url("https://jpmc.fa.oraclecloud.com/hcmUI/CandidateExperience/en/sites/CX_1001").is_none());
        assert_eq!(parse_board("egup.fa.us2.oraclecloud.com/CX").unwrap().site, "CX");
    }
}
