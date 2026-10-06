use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// User settings. Written as JSON next to the DB; the Phase 3 settings UI
/// edits this same file. API keys never live here — ANTHROPIC_API_KEY env
/// for now, Keychain/Credential Manager in Phase 3.
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
            season: "Summer 2027".into(),
            max_age_days: 7.0,
            exclude_advanced_degree: true,
            cycle_minutes: 30,
            active_start_hour: 8,
            active_end_hour: 23,
            applied_log_path: String::new(),
            notify_enabled: true,
            notify_threshold: 90,
            battery_pause_below: 20,
            prefilter_top: 20,
            min_score: 60,
            backend: "ollama".into(),
            ollama_url: "http://localhost:11434".into(),
            chat_model: "gemma3:4b".into(),
            embed_model: "nomic-embed-text".into(),
            anthropic_model: "claude-opus-5-5".into(),
        }
    }
}

impl Config {
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
