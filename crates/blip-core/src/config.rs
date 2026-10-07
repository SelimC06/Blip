use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// One company job board Blip reads directly.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CompanyEntry {
    /// "greenhouse" | "ashby" | "lever"
    pub platform: String,
    /// The board's slug on that platform, e.g. "stripe".
    pub board: String,
    /// Display name, used as the posting's company.
    pub name: String,
}

impl CompanyEntry {
    fn new(platform: &str, board: &str, name: &str) -> Self {
        CompanyEntry { platform: platform.into(), board: board.into(), name: name.into() }
    }

    /// Matches the source names used in source health ("greenhouse:stripe").
    pub fn source_name(&self) -> String {
        format!("{}:{}", self.platform, self.board)
    }
}

pub fn default_companies() -> Vec<CompanyEntry> {
    vec![
        CompanyEntry::new("greenhouse", "stripe", "Stripe"),
        CompanyEntry::new("greenhouse", "datadog", "Datadog"),
        CompanyEntry::new("greenhouse", "databricks", "Databricks"),
        CompanyEntry::new("greenhouse", "duolingo", "Duolingo"),
        CompanyEntry::new("greenhouse", "figma", "Figma"),
        CompanyEntry::new("greenhouse", "cloudflare", "Cloudflare"),
        CompanyEntry::new("greenhouse", "robinhood", "Robinhood"),
        CompanyEntry::new("greenhouse", "discord", "Discord"),
        CompanyEntry::new("ashby", "ramp", "Ramp"),
        CompanyEntry::new("ashby", "openai", "OpenAI"),
        CompanyEntry::new("ashby", "notion", "Notion"),
        CompanyEntry::new("ashby", "plaid", "Plaid"),
        CompanyEntry::new("ashby", "linear", "Linear"),
        CompanyEntry::new("lever", "palantir", "Palantir"),
    ]
}

/// User settings. Written as JSON next to the DB; the settings UI edits this
/// same file. API keys never live here: they're in the OS credential store.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub resume_path: String,
    /// Free-text steer for scoring, beyond the resume itself.
    pub looking_for: String,
    /// Any of: "internship", "co-op", "new-grad". Empty = all.
    pub role_types: Vec<String>,
    pub season: String,
    /// Postings older than this never surface.
    pub max_age_days: f64,
    /// Drop roles marked 🎓 (requires MS/PhD) on the Simplify list.
    pub exclude_advanced_degree: bool,
    /// Fields the user wants ("ml-ai", "software", "data", …; see fields.rs).
    /// Roles whose titles name only other fields are dropped before scoring.
    pub target_fields: Vec<String>,
    /// "anywhere" or "us": drop postings that only name places outside the US.
    pub location_scope: String,
    /// Optional places to stay near ("NYC", "TX", "Bay Area"). Remote roles
    /// always pass. Empty = anywhere in scope.
    pub places: Vec<String>,
    /// "auto" (read from the resume), "citizen", "permanent_resident", or
    /// "needs_sponsorship". Decides which roles the sponsorship filter hides.
    pub work_authorization: String,
    /// Minutes between automatic scan cycles.
    pub cycle_minutes: u64,
    /// Automatic cycles only run between these local hours (0–24). Equal
    /// values mean "always". Manual scans ignore this.
    pub active_start_hour: u8,
    pub active_end_hour: u8,
    /// Spreadsheet that ✓ Applied appends to. Empty = ~/Documents/Applied.xlsx.
    pub applied_log_path: String,
    /// Native notification when a cycle finds a match at or above this score.
    pub notify_enabled: bool,
    pub notify_threshold: u8,
    /// Skip automatic cycles on battery below this percent. 0 = never pause.
    pub battery_pause_below: u8,
    /// How many embedding-prefiltered candidates the LLM deep-reads per cycle.
    pub prefilter_top: usize,
    /// Never surface a match below this score; an empty panel beats noise.
    pub min_score: u8,
    /// Most matches shown per scan (only ones clearing `min_score` count).
    pub results_per_scan: usize,
    /// First-run setup finished (or skipped by an existing install).
    pub setup_done: bool,
    /// Company job boards read directly every cycle.
    pub companies: Vec<CompanyEntry>,
    /// Read the SimplifyJobs community list (most of Blip's postings).
    pub use_simplify: bool,
    /// Read the SimplifyJobs new-grad list (only when "new grad" is a role type).
    pub use_simplify_new_grad: bool,
    /// Read the vanshb03 / Ouckah internship list.
    pub use_vansh: bool,
    /// Read amazon.jobs.
    pub use_amazon: bool,
    /// Read USAJobs (needs a free key, stored in the credential store).
    pub use_usajobs: bool,
    /// The email the USAJobs key was issued to; USAJobs requires it.
    pub usajobs_email: String,
    /// "ollama" (default, local) or "anthropic" (API key required).
    pub backend: String,
    pub ollama_url: String,
    pub chat_model: String,
    pub embed_model: String,
    pub anthropic_model: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            resume_path: String::new(),
            looking_for: "Software engineering internships and co-ops".into(),
            role_types: vec!["internship".into(), "co-op".into()],
            season: default_season(),
            max_age_days: 7.0,
            exclude_advanced_degree: true,
            target_fields: crate::fields::default_targets(),
            location_scope: "anywhere".into(),
            places: Vec::new(),
            work_authorization: "auto".into(),
            cycle_minutes: 30,
            active_start_hour: 8,
            active_end_hour: 23,
            applied_log_path: String::new(),
            notify_enabled: true,
            notify_threshold: 90,
            battery_pause_below: 20,
            prefilter_top: 20,
            min_score: 70,
            results_per_scan: 5,
            setup_done: false,
            companies: default_companies(),
            use_simplify: true,
            use_simplify_new_grad: true,
            use_vansh: true,
            use_amazon: true,
            use_usajobs: false,
            usajobs_email: String::new(),
            backend: "ollama".into(),
            ollama_url: "http://localhost:11434".into(),
            chat_model: "gemma3:4b".into(),
            embed_model: "nomic-embed-text".into(),
            anthropic_model: "claude-opus-5-5".into(),
        }
    }
}

/// The summer internship season people are recruiting for right now:
/// from August on, that's next summer.
pub fn default_season() -> String {
    use chrono::Datelike;
    let today = chrono::Local::now().date_naive();
    let year = if today.month() >= 8 { today.year() + 1 } else { today.year() };
    format!("Summer {year}")
}

impl Config {
    /// A fresh install: setup never finished and no resume chosen. Installs
    /// from before setup existed already have a resume, so they skip it.
    pub fn needs_setup(&self) -> bool {
        !self.setup_done && self.resume_path.trim().is_empty()
    }

    pub fn applied_log_path(&self) -> PathBuf {
        if self.applied_log_path.trim().is_empty() {
            dirs::document_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("Applied.xlsx")
        } else {
            PathBuf::from(&self.applied_log_path)
        }
    }

    /// Whether an automatic cycle may run at this local hour. Handles
    /// windows that wrap midnight (e.g. 22 → 6).
    pub fn is_active_hour(&self, hour: u8) -> bool {
        let (s, e) = (self.active_start_hour % 24, self.active_end_hour % 24);
        if s == e {
            true
        } else if s < e {
            hour >= s && hour < e
        } else {
            hour >= s || hour < e
        }
    }
}

pub fn config_path() -> PathBuf {
    let base = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join("Blip").join("config.json")
}

pub fn load_or_create() -> Result<Config> {
    let path = config_path();
    if path.exists() {
        let raw = std::fs::read_to_string(&path)?;
        serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()))
    } else {
        let cfg = Config::default();
        save(&cfg)?;
        Ok(cfg)
    }
}

pub fn save(cfg: &Config) -> Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(cfg)?)?;
    Ok(())
}
