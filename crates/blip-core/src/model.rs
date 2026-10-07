/// A normalized job posting, whatever source it came from.
#[derive(Debug, Clone)]
pub struct Posting {
    pub company: String,
    pub title: String,
    pub location: String,
    pub url: String,
    pub source: String,
    /// e.g. "Summer 2027" when detectable, otherwise empty.
    pub season: String,
    /// Raw posted-date/age string from the source ("Oct 05", "2026-10-04T...").
    pub posted: String,
    /// Plain-text job description when known (Ashby gives it up front;
    /// others are fetched at scoring time). Empty otherwise.
    pub description: String,
    /// Application deadline (YYYY-MM-DD) when the source publishes one
    /// (Greenhouse's application_deadline). Empty otherwise.
    pub deadline: String,
}

impl Posting {
    /// Work-authorization requirement read from the title markers and
    /// description: one of the `auth` constants, or "" when none is stated.
    pub fn auth_requirement(&self) -> &'static str {
        crate::auth::classify(&self.title, &self.description)
    }
}

impl Posting {
    /// Dedupe key: same role reposted on another board or URL collapses to one.
    /// Built from normalized company|title|location|season, never the URL.
    pub fn fingerprint(&self) -> String {
        format!(
            "{}|{}|{}|{}",
            norm(&self.company),
            norm(&self.title),
            norm(&self.location),
            norm(&self.season)
        )
    }

    /// Second dedupe key: the job's ID on its applicant-tracking system, read
    /// from the URL. Catches one job listed by both the community list and the
    /// company's own board under slightly different titles or locations.
    pub fn job_key(&self) -> Option<String> {
        job_key(&self.url)
    }
}

pub fn job_key(url: &str) -> Option<String> {
    use std::sync::LazyLock;
    static GH: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"(?:[?&]gh_jid=|greenhouse\.io/[^/?#]+/jobs/)(\d{5,})").unwrap()
    });
    static ASHBY: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"ashbyhq\.com/[^/?#]+/([0-9a-fA-F-]{36})").unwrap()
    });
    static LEVER: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"lever\.co/[^/?#]+/([0-9a-fA-F-]{36})").unwrap()
    });
    if let Some(c) = GH.captures(url) {
        return Some(format!("gh:{}", &c[1]));
    }
    if let Some(c) = ASHBY.captures(url) {
        return Some(format!("ashby:{}", c[1].to_lowercase()));
    }
    static WORKDAY: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"(?i)([a-z0-9-]+)\.wd\d+\.myworkdayjobs\.com/(?:[a-z]{2}-[a-z]{2}/)?[^/?#]+/job/(?:[^?#]*/)?([^/?#]+)").unwrap()
    });
    static ORACLE: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"(?i)([a-z0-9-]+)\.fa(?:\.[a-z0-9-]+)?\.oraclecloud\.com/.*?/job/(\d+)").unwrap()
    });
    if let Some(c) = WORKDAY.captures(url) {
        return Some(format!("wd:{}:{}", c[1].to_lowercase(), c[2].to_lowercase()));
    }
    if let Some(c) = ORACLE.captures(url) {
        return Some(format!("orc:{}:{}", c[1].to_lowercase(), &c[2]));
    }
    LEVER.captures(url).map(|c| format!("lever:{}", c[1].to_lowercase()))
}

/// Case- and spacing-insensitive season comparison ("summer  2027" == "Summer 2027").
pub fn same_season(a: &str, b: &str) -> bool {
    let n = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    n(a) == n(b)
}

fn norm(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase()
}

#[derive(Debug, Default)]
pub struct ScanReport {
    pub scanned: usize,
    pub new: Vec<Posting>,
    pub errors: Vec<String>,
    pub sources: Vec<SourceStatus>,
}

/// One source's outcome for the latest cycle (e.g. "greenhouse:stripe").
#[derive(Debug, Clone, serde::Serialize)]
pub struct SourceStatus {
    pub name: String,
    pub ok: bool,
    pub count: usize,
    pub error: String,
}

/// Returned when the user cancels a cycle mid-scan. Callers can tell it
/// apart from real failures with `err.is::<Cancelled>()`.
#[derive(Debug)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("scan cancelled")
    }
}

impl std::error::Error for Cancelled {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_key_matches_across_url_styles() {
        let simplify = "https://boards.greenhouse.io/cloudflare/jobs/8245211?utm_source=Simplify&ref=Simplify";
        let board = "https://boards.greenhouse.io/cloudflare/jobs/8245211?gh_jid=8245211";
        let custom = "https://careers.datadoghq.com/detail/8114161/?gh_jid=8114161";
        let newer = "https://job-boards.greenhouse.io/gleanwork/jobs/4595665005?utm_source=Simplify";
        assert_eq!(job_key(simplify).as_deref(), Some("gh:8245211"));
        assert_eq!(job_key(board), job_key(simplify));
        assert_eq!(job_key(custom).as_deref(), Some("gh:8114161"));
        assert_eq!(job_key(newer).as_deref(), Some("gh:4595665005"));
        assert_eq!(
            job_key("https://jobs.ashbyhq.com/notion/E66C6658-9e65-4c58-8db2-844628b6e8f8").as_deref(),
            Some("ashby:e66c6658-9e65-4c58-8db2-844628b6e8f8")
        );
        assert_eq!(job_key("https://example.com/careers/123"), None);
        // Workday: a community-list link and Blip's own link are the same job.
        assert_eq!(
            job_key("https://generalmotors.wd5.myworkdayjobs.com/en-CA/Careers_GM/job/Warren-Michigan/XMLNAME-Intern_JR-202621695?utm_source=Simplify"),
            job_key("https://generalmotors.wd5.myworkdayjobs.com/Careers_GM/job/Warren-Michigan/XMLNAME-Intern_JR-202621695")
        );
        assert_eq!(
            job_key("https://egup.fa.us2.oraclecloud.com/hcmUI/CandidateExperience/en/sites/CX/job/20278933?utm_source=x").as_deref(),
            Some("orc:egup:20278933")
        );
    }

    #[test]
    fn seasons_compare_loosely() {
        assert!(same_season("summer 2027", "Summer 2027"));
        assert!(same_season(" Summer   2027 ", "Summer 2027"));
        assert!(!same_season("Fall 2027", "Summer 2027"));
    }
}
