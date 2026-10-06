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
    /// How many embedding-prefiltered candidates the LLM deep-reads per cycle.
    pub prefilter_top: usize,
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
            prefilter_top: 20,
            backend: "ollama".into(),
            ollama_url: "http://localhost:11434".into(),
            chat_model: "gemma3:4b".into(),
            embed_model: "nomic-embed-text".into(),
            anthropic_model: "claude-opus-5-5".into(),
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
