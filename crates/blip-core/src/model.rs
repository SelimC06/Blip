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
}
