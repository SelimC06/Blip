//! LLM access. Embeddings always run locally through Ollama (Anthropic has
//! no embeddings endpoint); chat/scoring dispatches to Ollama or the
//! Anthropic Messages API depending on config.backend.

use crate::config::Config;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::time::Duration;

pub struct Llm {
    cfg: Config,
    client: reqwest::blocking::Client,
}

impl Llm {
    pub fn new(cfg: &Config) -> Result<Self> {
        Ok(Llm {
            cfg: cfg.clone(),
            // Generous timeout: first Ollama call may cold-load the model.
            client: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(300))
                .build()?,
        })
    }

    /// Embed a batch of texts via Ollama /api/embed.
    pub fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(texts.len());
        for chunk in texts.chunks(64) {
            let resp: Value = self
                .client
                .post(format!("{}/api/embed", self.cfg.ollama_url))
                .json(&json!({ "model": self.cfg.embed_model, "input": chunk }))
                .send()
                .context("Ollama unreachable — is `ollama serve` running?")?
                .error_for_status()?
                .json()?;
            let embs = resp["embeddings"]
                .as_array()
                .ok_or_else(|| anyhow!("no embeddings in Ollama response"))?;
            for e in embs {
                out.push(
                    e.as_array()
                        .ok_or_else(|| anyhow!("bad embedding row"))?
                        .iter()
                        .map(|v| v.as_f64().unwrap_or(0.0) as f32)
                        .collect(),
                );
            }
        }
        Ok(out)
    }

    /// One chat turn that must return JSON; parsed and returned as a Value.
    pub fn chat_json(&self, system: &str, user: &str) -> Result<Value> {
        match self.cfg.backend.as_str() {
            "anthropic" => self.anthropic_chat_json(system, user),
            _ => self.ollama_chat_json(system, user),
        }
    }

    fn ollama_chat_json(&self, system: &str, user: &str) -> Result<Value> {
        let resp: Value = self
            .client
            .post(format!("{}/api/chat", self.cfg.ollama_url))
            .json(&json!({
                "model": self.cfg.chat_model,
                "stream": false,
                "format": "json",
                "options": { "temperature": 0 },
                "messages": [
                    { "role": "system", "content": system },
                    { "role": "user", "content": user }
                ]
            }))
            .send()
            .context("Ollama unreachable — is `ollama serve` running?")?
            .error_for_status()?
            .json()?;
        let content = resp["message"]["content"]
            .as_str()
            .ok_or_else(|| anyhow!("no message content from Ollama"))?;
        extract_json(content)
    }

    fn anthropic_chat_json(&self, system: &str, user: &str) -> Result<Value> {
        let key = crate::secrets::anthropic_key().context(
            "backend is \"anthropic\" but no API key is saved — add one in Settings → Model",
        )?;
        let resp: Value = self
            .client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01")
            .json(&json!({
                "model": self.cfg.anthropic_model,
                "max_tokens": 1024,
                "system": system,
                "messages": [{ "role": "user", "content": user }]
            }))
            .send()?
            .error_for_status()?
            .json()?;
        if resp["stop_reason"].as_str() == Some("refusal") {
            bail!("Anthropic API declined the request (stop_reason: refusal)");
        }
        let text = resp["content"]
            .as_array()
            .and_then(|blocks| {
                blocks
                    .iter()
                    .find(|b| b["type"] == "text")
                    .and_then(|b| b["text"].as_str())
            })
            .ok_or_else(|| anyhow!("no text block in Anthropic response"))?;
        extract_json(text)
    }
}

/// Models sometimes wrap JSON in prose or code fences; take the outermost
/// {...} and parse that.
fn extract_json(text: &str) -> Result<Value> {
    let start = text.find('{').ok_or_else(|| anyhow!("no JSON in model output"))?;
    let end = text.rfind('}').ok_or_else(|| anyhow!("no JSON in model output"))?;
    serde_json::from_str(&text[start..=end]).context("model output was not valid JSON")
}
