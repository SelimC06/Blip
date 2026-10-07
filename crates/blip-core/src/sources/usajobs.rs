//! USAJobs, the official federal jobs API. Needs a free key, requested at
//! https://developer.usajobs.gov/apirequest/, sent with the email it was
//! issued to:
//!   GET https://data.usajobs.gov/api/search?HiringPath=student&DatePosted=60…
//!   Host: data.usajobs.gov · User-Agent: <email> · Authorization-Key: <key>
//! HiringPath "student" is the Pathways internship program and "graduates"
//! the recent-graduate program; their titles ("Student Trainee
//! (Engineering)") rarely say "intern", so the path decides the role type.

use crate::model::Posting;
use anyhow::{bail, Result};
use serde_json::Value;
use std::collections::HashSet;

pub const PATHS: [(&str, &str); 2] = [("student", "internship"), ("graduates", "new-grad")];
const PER_PAGE: usize = 500; // the API's maximum
const MAX_PAGES: usize = 3;

/// The role type a USAJobs posting counts as, from its source.
pub fn role_type(source: &str) -> Option<&'static str> {
    let path = source.strip_prefix("usajobs:")?;
    PATHS.iter().find(|(p, _)| *p == path).map(|(_, role)| *role)
}

fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().map(text).filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

/// One result item. Fields are read from `MatchedObjectDescriptor` when the
/// item wraps them in one, otherwise from the item itself.
fn to_posting(item: &Value, path: &str) -> Option<Posting> {
    let d = if item["MatchedObjectDescriptor"].is_object() { &item["MatchedObjectDescriptor"] } else { item };
    let title = d["PositionTitle"].as_str()?.trim().to_string();
    let url = d["PositionURI"].as_str().or_else(|| d["ApplyURI"][0].as_str())?.to_string();
    let details = &d["UserArea"]["Details"];
    let description = [
        text(&details["JobSummary"]),
        text(&d["QualificationSummary"]),
        text(&details["MajorDuties"]),
        text(&details["Requirements"]),
        text(&details["Education"]),
    ]
    .into_iter()
    .filter(|s| !s.is_empty())
    .collect::<Vec<_>>()
    .join("\n");
    let company = d["OrganizationName"]
        .as_str()
        .or_else(|| d["DepartmentName"].as_str())
        .unwrap_or("US Government")
        .to_string();
    let date = |k: &str| d[k].as_str().and_then(|s| s.get(..10)).unwrap_or("").to_string();
    Some(Posting {
        company,
        title,
        location: d["PositionLocationDisplay"].as_str().unwrap_or("").to_string(),
        url,
        source: format!("usajobs:{path}"),
        season: String::new(),
        posted: date("PublicationStartDate"),
        description: crate::describe::html_to_text(&description),
        deadline: date("ApplicationCloseDate"),
    })
}

pub fn fetch(client: &reqwest::blocking::Client, email: &str, key: &str, role_types: &[String]) -> Result<Vec<Posting>> {
    if email.trim().is_empty() || key.trim().is_empty() {
        bail!("needs your USAJobs API key and the email it was issued to (Settings → Sources)");
    }
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (path, role) in PATHS {
        if !role_types.iter().any(|r| r == role) {
            continue;
        }
        for page in 1..=MAX_PAGES {
            let url = format!(
                "https://data.usajobs.gov/api/search?HiringPath={path}&DatePosted=60&ResultsPerPage={PER_PAGE}&Page={page}"
            );
            let resp = client
                .get(&url)
                .header("Host", "data.usajobs.gov")
                .header("User-Agent", email.trim())
                .header("Authorization-Key", key.trim())
                .send()?;
            if resp.status() == reqwest::StatusCode::UNAUTHORIZED || resp.status() == reqwest::StatusCode::FORBIDDEN {
                bail!("USAJobs rejected the key ({}). Check the key and that the email matches the one you registered.", resp.status());
            }
            let v: Value = resp.error_for_status()?.json()?;
            let items = v["SearchResult"]["SearchResultItems"].as_array().cloned().unwrap_or_default();
            for item in &items {
                if let Some(p) = to_posting(item, path) {
                    if seen.insert(p.url.clone()) {
                        out.push(p);
                    }
                }
            }
            if items.len() < PER_PAGE {
                break;
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn item(wrapped: bool) -> Value {
        let d = json!({
            "PositionID": "NASA-123",
            "PositionTitle": "Student Trainee (Engineering)",
            "PositionURI": "https://www.usajobs.gov/job/812345600",
            "ApplyURI": ["https://www.usajobs.gov/job/812345600/apply"],
            "PositionLocationDisplay": "Houston, Texas",
            "OrganizationName": "National Aeronautics and Space Administration",
            "DepartmentName": "National Aeronautics and Space Administration",
            "PublicationStartDate": "2026-10-01T00:00:00.0000",
            "ApplicationCloseDate": "2026-10-20T23:59:59.9970",
            "QualificationSummary": "Must be enrolled in an accredited program.",
            "UserArea": { "Details": {
                "JobSummary": "Join NASA as a Pathways intern.",
                "MajorDuties": ["Assist engineers", "Run tests"],
                "Requirements": "U.S. Citizenship is required."
            }}
        });
        if wrapped { json!({ "MatchedObjectId": "812345600", "MatchedObjectDescriptor": d }) } else { d }
    }

    #[test]
    fn reads_items_wrapped_or_not() {
        for wrapped in [true, false] {
            let p = to_posting(&item(wrapped), "student").unwrap();
            assert_eq!(p.title, "Student Trainee (Engineering)");
            assert_eq!(p.company, "National Aeronautics and Space Administration");
            assert_eq!(p.posted, "2026-10-01");
            assert_eq!(p.deadline, "2026-10-20");
            assert!(p.description.contains("Run tests"));
            assert_eq!(p.auth_requirement(), crate::auth::CITIZENSHIP);
            assert_eq!(role_type(&p.source), Some("internship"));
        }
        assert_eq!(role_type("usajobs:graduates"), Some("new-grad"));
        assert_eq!(role_type("greenhouse:stripe"), None);
    }
}
