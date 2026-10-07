//! Job descriptions for the scoring shortlist: fetch each posting's page and
//! boil it down to plain text. Server-rendered boards (Greenhouse, Lever,
//! iCIMS…) work; JavaScript-only pages (Workday) yield little and the scorer
//! falls back to title/company/location.

use crate::sources::{oracle, workday};
use regex::Regex;
use std::net::IpAddr;
use std::sync::LazyLock;
use std::time::Duration;

const MAX_CHARS: usize = 2500;

static DROP_BLOCKS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?is)<script\b.*?</script>|<style\b.*?</style>|<noscript\b.*?</noscript>|<svg\b.*?</svg>|<head\b.*?</head>|<nav\b.*?</nav>|<footer\b.*?</footer>",
    )
    .unwrap()
});
static BREAKS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)<(br|/p|/div|/li|/h\d)\b[^>]*>").unwrap());
static TAGS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<[^>]+>").unwrap());
static SPACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[ \t\u{a0}]+").unwrap());
static BLANK_LINES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\n\s*\n+").unwrap());

pub fn html_to_text(html: &str) -> String {
    let s = DROP_BLOCKS.replace_all(html, " ");
    let s = BREAKS.replace_all(&s, "\n");
    let s = TAGS.replace_all(&s, " ");
    let s = s
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&#39;", "'")
        .replace("&quot;", "\"");
    let s = SPACES.replace_all(&s, " ");
    BLANK_LINES.replace_all(&s, "\n").trim().to_string()
}

/// Trim to a prompt-sized excerpt.
pub fn excerpt(text: &str) -> String {
    text.chars().take(MAX_CHARS).collect()
}

/// Links come from a community-edited list, so only fetch public https pages:
/// never plain http, localhost, or private-network addresses.
pub fn is_public_https(url: &reqwest::Url) -> bool {
    if url.scheme() != "https" {
        return false;
    }
    let Some(host) = url.host_str() else { return false };
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = bare.parse::<IpAddr>() {
        return match ip {
            IpAddr::V4(v4) => !(v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.octets()[0] == 100 && (64..128).contains(&v4.octets()[1])),
            IpAddr::V6(v6) => {
                let s0 = v6.segments()[0];
                !(v6.is_loopback() || v6.is_unspecified() || s0 & 0xfe00 == 0xfc00 || s0 & 0xffc0 == 0xfe80)
            }
        };
    }
    let h = host.to_ascii_lowercase();
    h.contains('.')
        && !(h == "localhost"
            || h.ends_with(".localhost")
            || h.ends_with(".local")
            || h.ends_with(".internal")
            || h.ends_with(".home.arpa"))
}

/// Fetch descriptions for every posting that lacks one, in parallel.
/// Returns (index, text) for each page that yielded real content.
/// `cancelled` is checked before each request.
pub fn fetch_missing(
    urls: &[(usize, String)],
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Vec<(usize, String)> {
    let redirects = reqwest::redirect::Policy::custom(|attempt| {
        if attempt.previous().len() >= 5 || !is_public_https(attempt.url()) {
            attempt.stop()
        } else {
            attempt.follow()
        }
    });
    let Ok(client) = reqwest::blocking::Client::builder()
        .user_agent("Mozilla/5.0 (Macintosh) blip/0.1")
        .timeout(Duration::from_secs(6))
        .redirect(redirects)
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
                    if cancelled() {
                        return None;
                    }
                    if let Some(text) = crate::sources::ashby::description_for(client, url) {
                        return Some((*i, excerpt(&text)));
                    }
                    // Workday and Oracle job pages are JavaScript-only; read
                    // the JSON their pages load from instead.
                    let api = workday::detail_api_url(url)
                        .map(|u| (u, workday::description_from_detail as fn(&serde_json::Value) -> Option<String>))
                        .or_else(|| oracle::detail_api_url(url).map(|u| (u, oracle::description_from_detail as _)));
                    let target = api.as_ref().map(|(u, _)| u.as_str()).unwrap_or(url);
                    let parsed = reqwest::Url::parse(target).ok()?;
                    if !is_public_https(&parsed) {
                        return None;
                    }
                    let resp = client.get(parsed).header("Accept", "application/json, text/html").send().ok()?.error_for_status().ok()?;
                    let text = match api {
                        Some((_, extract)) => extract(&resp.json::<serde_json::Value>().ok()?)?,
                        None => html_to_text(&resp.text().ok()?),
                    };
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

    /// Live: reads a real JPMorgan (Oracle) and GM (Workday) job through
    /// their JSON endpoints. Run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn reads_workday_oracle_and_ashby_descriptions_live() {
        let client = crate::http_client().unwrap();
        let orc = crate::sources::oracle::fetch_board(&client, "jpmc.fa.oraclecloud.com/CX_1001", "JPMorgan").unwrap();
        let wd = crate::sources::workday::fetch_board(&client, "generalmotors.wd5/Careers_GM", "GM").unwrap();
        let ashby = crate::sources::ashby::fetch_board(&client, "notion", "Notion").unwrap();
        let urls: Vec<(usize, String)> = vec![(0, orc[0].url.clone()), (1, wd[0].url.clone()), (2, ashby[0].url.clone())];
        let got = fetch_missing(&urls, &|| false);
        assert_eq!(got.len(), 3, "all three descriptions read: {urls:?}");
        for (_, text) in got {
            assert!(text.len() > 300, "{text}");
        }
    }

    #[test]
    fn only_public_https_is_fetchable() {
        let ok = |u: &str| is_public_https(&reqwest::Url::parse(u).unwrap());
        assert!(ok("https://boards.greenhouse.io/x/jobs/1"));
        assert!(ok("https://8.8.8.8/job"));
        assert!(!ok("http://boards.greenhouse.io/x/jobs/1"));
        assert!(!ok("https://localhost:11434/api/tags"));
        assert!(!ok("https://127.0.0.1/"));
        assert!(!ok("https://192.168.1.1/admin"));
        assert!(!ok("https://10.0.0.5/"));
        assert!(!ok("https://169.254.169.254/latest/meta-data"));
        assert!(!ok("https://[::1]/"));
        assert!(!ok("https://printer.local/"));
        assert!(!ok("https://intranet/"));
    }
}
