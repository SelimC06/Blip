//! Resume ingestion: file → text → structured profile JSON (via LLM, once)
//! → embedding. Cached on disk; rebuilt only when the resume file changes.

use crate::config::Config;
use crate::llm::Llm;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Serialize, Deserialize)]
pub struct Profile {
    pub data: serde_json::Value,
    pub embedding: Vec<f32>,
    pub resume_path: String,
    pub resume_mtime: u64,
    /// The "looking for" text the embedding was built with.
    #[serde(default)]
    pub looking_for: String,
}

pub const NO_RESUME: &str =
    "No resume yet. Choose yours in Settings → Profile (CLI: blip profile --resume <file>).";

pub fn profile_path() -> PathBuf {
    let base = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join("Blip").join("profile.json")
}

pub fn extract_resume_text(path: &Path) -> Result<String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    let text = match ext.as_str() {
        "pdf" => pdf_extract::extract_text(path)
            .with_context(|| format!("extracting text from {}", path.display()))?,
        "txt" | "md" => std::fs::read_to_string(path)?,
        other => bail!("unsupported resume format .{other} — use PDF, txt, or md"),
    };
    // Plenty for any resume; keeps the prompt bounded if someone points
    // Blip at the wrong file.
    Ok(text.chars().take(15_000).collect())
}

pub fn build(llm: &Llm, cfg: &Config) -> Result<Profile> {
    if cfg.resume_path.trim().is_empty() {
        bail!(NO_RESUME);
    }
    let path = PathBuf::from(&cfg.resume_path);
    if !path.exists() {
        bail!("Your resume file moved or was deleted ({}). Choose it again in Settings → Profile.", path.display());
    }
    let text = extract_resume_text(&path)?;

    let system = "You turn resumes into structured JSON for job matching. \
                  Respond with only a JSON object, no prose.";
    let user = format!(
        "Extract this resume into exactly this JSON shape:\n\
         {{\"summary\": \"<2 sentences>\", \"skills\": [\"...\"], \
         \"projects\": [{{\"name\": \"...\", \"tech\": \"...\"}}], \
         \"experience\": [{{\"company\": \"...\", \"role\": \"...\"}}], \
         \"education\": {{\"school\": \"...\", \"degree\": \"...\", \"grad_date\": \"...\"}}, \
         \"work_authorization\": \"<citizen|permanent_resident|needs_sponsorship|unknown>\"}}\n\n\
         RESUME:\n{text}"
    );
    let data = llm.chat_json(system, &user)?;
    let embedding = embed_profile(llm, &data, &cfg.looking_for)?;

    let profile = Profile {
        data,
        embedding,
        resume_path: cfg.resume_path.clone(),
        resume_mtime: mtime(&path),
        looking_for: cfg.looking_for.clone(),
    };
    save(&profile)?;
    Ok(profile)
}

/// What the posting embeddings get compared against: resume summary,
/// skills, and the user's "looking for" text.
fn embed_profile(llm: &Llm, data: &serde_json::Value, looking_for: &str) -> Result<Vec<f32>> {
    let embed_text = format!(
        "{} Skills: {}. Looking for: {}",
        data["summary"].as_str().unwrap_or(""),
        data["skills"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(", "))
            .unwrap_or_default(),
        looking_for
    );
    Ok(llm.embed(&[embed_text])?.into_iter().next().unwrap_or_default())
}

/// Cached profile if the resume file hasn't changed; rebuild otherwise.
/// A changed "looking for" only re-embeds (one fast local call).
pub fn load_or_build(llm: &Llm, cfg: &Config) -> Result<Profile> {
    let path = profile_path();
    if let Ok(raw) = std::fs::read_to_string(&path) {
        if let Ok(mut p) = serde_json::from_str::<Profile>(&raw) {
            if p.resume_path == cfg.resume_path
                && p.resume_mtime == mtime(Path::new(&cfg.resume_path))
                && !p.embedding.is_empty()
            {
                if p.looking_for != cfg.looking_for {
                    p.embedding = embed_profile(llm, &p.data, &cfg.looking_for)?;
                    p.looking_for = cfg.looking_for.clone();
                    save(&p)?;
                }
                return Ok(p);
            }
        }
    }
    eprintln!("building profile from {} …", cfg.resume_path);
    build(llm, cfg)
}

fn save(p: &Profile) -> Result<()> {
    let path = profile_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_string(p)?)?;
    Ok(())
}

fn mtime(path: &Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
