//! First-run setup: get Ollama running, download the two models, and hand
//! off to the resume picker (shared with Settings). The UI drives the steps;
//! these commands report state and do the slow parts.

use crate::{set_state, Shared};
use blip_core::config;
use serde::Serialize;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, State};

#[derive(Serialize)]
pub struct ModelStatus {
    name: String,
    /// What Blip uses it for, in the user's terms.
    purpose: &'static str,
    size: &'static str,
    installed: bool,
}

#[derive(Serialize)]
pub struct SetupStatus {
    /// "running" | "installed" (present but not running) | "missing"
    ollama: &'static str,
    models: Vec<ModelStatus>,
    resume_path: String,
}

fn approx_size(model: &str) -> &'static str {
    match model.split(':').next().unwrap_or(model) {
        "nomic-embed-text" => "274 MB",
        "gemma3" if model.ends_with(":4b") => "3.3 GB",
        "gemma3" if model.ends_with(":1b") => "815 MB",
        "llama3.2" => "2.0 GB",
        "qwen3" if model.ends_with(":8b") => "5.2 GB",
        _ => "",
    }
}

fn http(timeout: Option<Duration>) -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(3))
        .timeout(timeout)
        .build()
        .expect("http client")
}

fn ollama_running(url: &str) -> bool {
    http(Some(Duration::from_millis(1500)))
        .get(format!("{url}/api/version"))
        .send()
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Where an Ollama install lives when it isn't running.
fn ollama_install() -> Option<PathBuf> {
    let home = dirs_home();
    let candidates: Vec<PathBuf> = if cfg!(target_os = "macos") {
        vec![
            PathBuf::from("/Applications/Ollama.app"),
            home.join("Applications/Ollama.app"),
            PathBuf::from("/opt/homebrew/bin/ollama"),
            PathBuf::from("/usr/local/bin/ollama"),
        ]
    } else if cfg!(target_os = "windows") {
        let local = std::env::var("LOCALAPPDATA").map(PathBuf::from).unwrap_or_default();
        vec![
            local.join("Programs/Ollama/ollama app.exe"),
            local.join("Programs/Ollama/ollama.exe"),
        ]
    } else {
        vec![PathBuf::from("/usr/local/bin/ollama"), PathBuf::from("/usr/bin/ollama")]
    };
    candidates.into_iter().find(|p| p.exists())
}

fn dirs_home() -> PathBuf {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// Installed model names, e.g. ["gemma3:4b", "nomic-embed-text:latest"].
fn installed_models(url: &str) -> Vec<String> {
    http(Some(Duration::from_secs(3)))
        .get(format!("{url}/api/tags"))
        .send()
        .and_then(|r| r.json::<Value>())
        .map(|v| {
            v["models"]
                .as_array()
                .map(|a| a.iter().filter_map(|m| m["name"].as_str().map(String::from)).collect())
                .unwrap_or_default()
        })
        .unwrap_or_default()
}

/// "nomic-embed-text" is installed as "nomic-embed-text:latest".
fn has_model(installed: &[String], wanted: &str) -> bool {
    installed
        .iter()
        .any(|n| n == wanted || (!wanted.contains(':') && *n == format!("{wanted}:latest")))
}

#[tauri::command]
pub async fn setup_status() -> Result<SetupStatus, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let cfg = config::load_or_create().map_err(|e| e.to_string())?;
        let running = ollama_running(&cfg.ollama_url);
        let installed = if running { installed_models(&cfg.ollama_url) } else { vec![] };
        let models = [(&cfg.chat_model, "reads and scores postings"), (&cfg.embed_model, "matches postings to your resume")]
            .into_iter()
            .map(|(name, purpose)| ModelStatus {
                name: name.clone(),
                purpose,
                size: approx_size(name),
                installed: has_model(&installed, name),
            })
            .collect();
        Ok(SetupStatus {
            ollama: if running {
                "running"
            } else if ollama_install().is_some() {
                "installed"
            } else {
                "missing"
            },
            models,
            resume_path: cfg.resume_path,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Launch an installed Ollama. The UI polls setup_status until it's up.
#[tauri::command]
pub fn start_ollama() -> Result<(), String> {
    let path = ollama_install().ok_or("Ollama isn't installed yet")?;
    let launched = if path.extension().is_some_and(|e| e == "app") {
        std::process::Command::new("open").arg("-a").arg(&path).spawn()
    } else if path.file_name().is_some_and(|n| n == "ollama app.exe") {
        std::process::Command::new(&path).spawn()
    } else {
        std::process::Command::new(&path).arg("serve").spawn()
    };
    launched.map(|_| ()).map_err(|e| format!("couldn't start Ollama: {e}"))
}

#[derive(Clone, Serialize)]
struct PullProgress {
    model: String,
    /// 1-based position among the models being downloaded.
    index: usize,
    count: usize,
    status: String,
    completed: u64,
    total: u64,
}

/// Download whichever of the two models are missing, streaming progress to
/// the UI as "setup-pull" events.
#[tauri::command]
pub async fn pull_models(app: AppHandle) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        let cfg = config::load_or_create().map_err(|e| e.to_string())?;
        let installed = installed_models(&cfg.ollama_url);
        let wanted: Vec<String> = [cfg.chat_model.clone(), cfg.embed_model.clone()]
            .into_iter()
            .filter(|m| !has_model(&installed, m))
            .collect();
        // Large downloads: no overall timeout, only on connecting.
        let client = http(None);
        for (i, model) in wanted.iter().enumerate() {
            let resp = client
                .post(format!("{}/api/pull", cfg.ollama_url))
                .json(&json!({ "model": model, "stream": true }))
                .send()
                .map_err(|_| "Ollama stopped responding. Make sure it's open, then try again.".to_string())?;
            if !resp.status().is_success() {
                return Err(format!("Ollama couldn't download {model} ({})", resp.status()));
            }
            // Progress arrives per layer (digest); sum layers for one bar.
            let mut layers: std::collections::HashMap<String, (u64, u64)> = Default::default();
            let mut last_emit = Instant::now() - Duration::from_secs(1);
            for line in BufReader::new(resp).lines() {
                let line = line.map_err(|e| format!("download interrupted: {e}"))?;
                let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
                if let Some(err) = v["error"].as_str() {
                    return Err(format!("Ollama couldn't download {model}: {err}"));
                }
                if let (Some(d), Some(total)) = (v["digest"].as_str(), v["total"].as_u64()) {
                    let done = v["completed"].as_u64().unwrap_or(0);
                    layers.insert(d.to_string(), (done, total));
                }
                let status = v["status"].as_str().unwrap_or("").to_string();
                let finished = status == "success";
                if finished || last_emit.elapsed() >= Duration::from_millis(120) {
                    last_emit = Instant::now();
                    let (completed, total) = layers
                        .values()
                        .fold((0, 0), |(c, t), (lc, lt)| (c + lc, t + lt));
                    let _ = app.emit(
                        "setup-pull",
                        PullProgress {
                            model: model.clone(),
                            index: i + 1,
                            count: wanted.len(),
                            status,
                            completed,
                            total,
                        },
                    );
                }
            }
        }
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Save the preferences from the last step, mark setup done, and start the
/// first scan.
#[tauri::command]
pub fn finish_setup(app: AppHandle, cfg: config::Config) -> Result<(), String> {
    let mut cfg = cfg;
    cfg.setup_done = true;
    config::save(&cfg).map_err(|e| e.to_string())?;
    let shared: State<Arc<Shared>> = app.state();
    set_state(&app, &shared, |ui| {
        if ui.status == "setup" {
            ui.status = "rest".into();
        }
        ui.message = String::new();
        ui.cycle_minutes = cfg.cycle_minutes;
    });
    let _ = shared.scan_now.send(());
    Ok(())
}
