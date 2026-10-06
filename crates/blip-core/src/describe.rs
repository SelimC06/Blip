//! Job descriptions for the scoring shortlist: fetch each posting's page and
//! boil it down to plain text. Server-rendered boards (Greenhouse, Lever,
//! iCIMS…) work; JavaScript-only pages (Workday) yield little and the scorer
//! falls back to title/company/location.

use regex::Regex;
use std::time::Duration;

const MAX_CHARS: usize = 2500;

pub fn html_to_text(html: &str) -> String {
    let drop_blocks = Regex::new(
        r"(?is)<script\b.*?</script>|<style\b.*?</style>|<noscript\b.*?</noscript>|<svg\b.*?</svg>|<head\b.*?</head>|<nav\b.*?</nav>|<footer\b.*?</footer>",
    )
    .unwrap();
    let breaks = Regex::new(r"(?i)<(br|/p|/div|/li|/h\d)\b[^>]*>").unwrap();
    let tags = Regex::new(r"(?s)<[^>]+>").unwrap();
    let spaces = Regex::new(r"[ \t\u{a0}]+").unwrap();
    let blank_lines = Regex::new(r"\n\s*\n+").unwrap();

    let s = drop_blocks.replace_all(html, " ");
    let s = breaks.replace_all(&s, "\n");
    let s = tags.replace_all(&s, " ");
    let s = s
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&#39;", "'")
        .replace("&quot;", "\"");
    let s = spaces.replace_all(&s, " ");
    blank_lines.replace_all(&s, "\n").trim().to_string()
}

/// Trim to a prompt-sized excerpt.
pub fn excerpt(text: &str) -> String {
    text.chars().take(MAX_CHARS).collect()
}

/// Fetch descriptions for every posting that lacks one, in parallel.
/// Returns (index, text) for each page that yielded real content.
pub fn fetch_missing(urls: &[(usize, String)]) -> Vec<(usize, String)> {
    let Ok(client) = reqwest::blocking::Client::builder()
        .user_agent("Mozilla/5.0 (Macintosh) blip/0.1")
        .timeout(Duration::from_secs(8))
        .build()
    else {
        return vec![];
    };
    std::thread::scope(|scope| {
        let handles: Vec<_> = urls
            .iter()
            .map(|(i, url)| {
                let client = &client;
                scope.spawn(move || {
                    let html = client.get(url).send().ok()?.error_for_status().ok()?.text().ok()?;
                    let text = html_to_text(&html);
                    // Under ~300 chars is a JS shell or an error page, not a job.
                    (text.len() >= 300).then(|| (*i, excerpt(&text)))
                })
            })
            .collect();
        handles.into_iter().filter_map(|h| h.join().ok().flatten()).collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_markup_scripts_and_entities() {
        let html = "<html><head><title>x</title></head><body><nav>Menu</nav>\
            <script>var a=1;</script><h1>Software Intern</h1><p>Build &amp; ship.</p>\
            <ul><li>Rust</li><li>SQL</li></ul><footer>© Co</footer></body></html>";
        let t = html_to_text(html);
        assert!(t.contains("Software Intern"));
        assert!(t.contains("Build & ship."));
        assert!(t.contains("Rust") && t.contains("SQL"));
        assert!(!t.contains("var a") && !t.contains("Menu") && !t.contains("©"));
    }
}
