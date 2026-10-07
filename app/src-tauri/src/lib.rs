//! Blip pill app: a frameless always-on-top window whose Rust side runs the
//! blip-core pipeline on a schedule and feeds state to the HTML pill UI.

mod setup;

use blip_core::model::{Cancelled, SourceStatus};
use blip_core::score::{self, Scored};
use blip_core::store::{default_db_path, Store};
use blip_core::{applied_log, config, export, llm::Llm, profile, secrets};
use chrono::Timelike;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_notification::NotificationExt;

/// Flag deadlines this close in the panel; remind (once) at this many days.
const CLOSING_SOON_DAYS: i64 = 7;
const REMIND_DAYS: i64 = 3;

#[derive(Clone, Serialize, Deserialize, Default)]
struct JobDto {
    fingerprint: String,
    score: u8,
    reason: String,
    red_flags: Vec<String>,
    company: String,
    title: String,
    location: String,
    url: String,
    posted: String,
    #[serde(default)]
    deadline: String,
    /// Days until the deadline when it's within CLOSING_SOON_DAYS.
    #[serde(default)]
    closes_in: Option<i64>,
    /// ✓ pressed and logged; the row stays visible, dimmed.
    #[serde(default)]
    applied: bool,
}

#[derive(Clone, Serialize, Default)]
struct StatsDto {
    scanned: usize,
    new_count: usize,
    duration_secs: u64,
    errors: Vec<String>,
    sources_total: usize,
    sources_failed: usize,
}

/// Everything the frontend needs to render, in one payload.
#[derive(Clone, Serialize, Default)]
struct UiState {
    /// "rest" | "scanning" | "complete" | "paused" | "error"
    status: String,
    message: String,
    results: Vec<JobDto>,
    stats: StatsDto,
    cycle_minutes: u64,
    /// Automatic cycles paused (manual Scan now still works).
    paused: bool,
}

struct Shared {
    ui: Mutex<UiState>,
    scan_now: Sender<()>,
    hit: Mutex<HitRect>,
    cancel: AtomicBool,
}

/// The visible surface's rect, in logical px relative to the window. The
/// window is a fixed transparent canvas; everything outside this rect lets
/// clicks fall through to whatever app is underneath.
#[derive(Clone, Copy, serde::Deserialize)]
struct HitRect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

const WIN_W: f64 = 430.0;
const INITIAL_HIT: HitRect = HitRect { x: WIN_W - 16.0 - 146.0, y: 8.0, w: 146.0, h: 40.0 };

fn set_state(app: &AppHandle, shared: &Shared, f: impl FnOnce(&mut UiState)) {
    let snapshot = {
        let mut ui = shared.ui.lock().unwrap();
        f(&mut ui);
        ui.clone()
    };
    let _ = app.emit("blip-state", snapshot);
}

// ---------- commands ----------

#[tauri::command]
fn get_state(shared: State<Arc<Shared>>) -> UiState {
    shared.ui.lock().unwrap().clone()
}

#[tauri::command]
fn scan_now(shared: State<Arc<Shared>>) {
    if shared.ui.lock().unwrap().status == "scanning" {
        return;
    }
    let _ = shared.scan_now.send(());
}

/// Stops the running cycle at the next checkpoint (between sources or
/// between LLM calls). The pill goes back to Resting.
#[tauri::command]
fn cancel_scan(shared: State<Arc<Shared>>) {
    shared.cancel.store(true, Ordering::SeqCst);
}

#[tauri::command]
fn source_health() -> Result<Vec<SourceStatus>, String> {
    Store::open(&default_db_path())
        .and_then(|s| s.source_health())
        .map_err(|e| e.to_string())
}

/// Save the last 7 days of everything Blip found as CSV. Returns the path,
/// or None if the user cancelled the save dialog.
#[tauri::command]
async fn export_week(app: AppHandle) -> Result<Option<String>, String> {
    let name = format!("blip-week-{}.csv", chrono::Local::now().format("%Y-%m-%d"));
    let mut dialog = app
        .dialog()
        .file()
        .set_title("Export the last 7 days")
        .set_file_name(&name)
        .add_filter("CSV", &["csv"]);
    if let Some(docs) = dirs_documents() {
        dialog = dialog.set_directory(docs);
    }
    let Some(file) = dialog.blocking_save_file() else { return Ok(None) };
    let path = file.into_path().map_err(|e| e.to_string())?;
    let rows = Store::open(&default_db_path())
        .and_then(|s| s.export_since(7))
        .map_err(|e| e.to_string())?;
    export::write_csv(&path, &rows).map_err(|e| e.to_string())?;
    Ok(Some(path.to_string_lossy().into_owned()))
}

fn dirs_documents() -> Option<std::path::PathBuf> {
    config::Config::default().applied_log_path().parent().map(|p| p.to_path_buf())
}

#[tauri::command]
fn get_autostart(app: AppHandle) -> bool {
    app.autolaunch().is_enabled().unwrap_or(false)
}

#[tauri::command]
fn set_autostart(app: AppHandle, enabled: bool) -> Result<(), String> {
    let al = app.autolaunch();
    if enabled { al.enable() } else { al.disable() }.map_err(|e| e.to_string())
}

#[tauri::command]
fn toggle_pause(app: AppHandle, shared: State<Arc<Shared>>) {
    set_state(&app, &shared, |ui| {
        ui.paused = !ui.paused;
        // Only idle states switch the pill label; a scan in flight or fresh
        // results keep theirs, and pausing just stops future automatic cycles.
        match (ui.paused, ui.status.as_str()) {
            (true, "rest" | "error") => ui.status = "paused".into(),
            (false, "paused") => ui.status = "rest".into(),
            _ => {}
        }
    });
}

/// The results list lives here, not in the page: every state event re-sends
/// it, so ✕ and ✓ have to change it or the page re-renders the old list.
#[tauri::command]
fn dismiss_job(app: AppHandle, shared: State<Arc<Shared>>, fingerprint: String) -> Result<(), String> {
    let store = Store::open(&default_db_path()).map_err(|e| e.to_string())?;
    store.set_status(&fingerprint, "dismissed").map_err(|e| e.to_string())?;
    set_state(&app, &shared, |ui| ui.results.retain(|j| j.fingerprint != fingerprint));
    Ok(())
}

/// ✓ Applied: append to the spreadsheet first, and only mark the posting
/// applied once that worked, so a locked file never silently drops a row.
#[tauri::command]
async fn mark_applied(app: AppHandle, job: JobDto) -> Result<String, String> {
    let fingerprint = job.fingerprint.clone();
    let file = tauri::async_runtime::spawn_blocking(move || -> anyhow::Result<String> {
        let cfg = config::load_or_create()?;
        let path = cfg.applied_log_path();
        applied_log::append(
            &path,
            &applied_log::AppliedRow {
                date: chrono::Local::now().format("%Y-%m-%d").to_string(),
                company: job.company.clone(),
                role: job.title.clone(),
                location: job.location.clone(),
                score: job.score,
                posted: job.posted.clone(),
                url: job.url.clone(),
            },
        )?;
        Store::open(&default_db_path())?.set_status(&job.fingerprint, "applied")?;
        Ok(path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default())
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| format!("{e:#}"))?;
    let shared = app.state::<Arc<Shared>>();
    set_state(&app, &shared, |ui| {
        for j in ui.results.iter_mut().filter(|j| j.fingerprint == fingerprint) {
            j.applied = true;
        }
    });
    Ok(file)
}

// ---------- settings ----------

#[derive(Serialize)]
struct SettingsDto {
    config: config::Config,
    has_api_key: bool,
    applied_log_resolved: String,
    applied_log_exists: bool,
    profile_summary: String,
    /// What the resume says about work authorization, for the "From my
    /// resume" option ("citizen", "needs_sponsorship", "unknown", …).
    resume_authorization: String,
    has_usajobs_key: bool,
}

fn profile_summary() -> String {
    let Ok(raw) = std::fs::read_to_string(profile::profile_path()) else {
        return String::new();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return String::new();
    };
    let edu = &v["data"]["education"];
    [&edu["school"], &edu["degree"], &edu["grad_date"]]
        .iter()
        .filter_map(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

#[tauri::command]
fn get_settings() -> Result<SettingsDto, String> {
    let cfg = config::load_or_create().map_err(|e| e.to_string())?;
    let log = cfg.applied_log_path();
    Ok(SettingsDto {
        has_api_key: secrets::has_stored_anthropic_key(),
        has_usajobs_key: secrets::usajobs_key().is_some(),
        applied_log_resolved: log.to_string_lossy().into_owned(),
        applied_log_exists: log.exists(),
        profile_summary: profile_summary(),
        resume_authorization: std::fs::read_to_string(profile::profile_path())
            .ok()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
            .and_then(|v| v["data"]["work_authorization"].as_str().map(blip_core::auth::normalize_authorization))
            .unwrap_or("unknown")
            .to_string(),
        config: cfg,
    })
}

#[tauri::command]
fn save_settings(app: AppHandle, shared: State<Arc<Shared>>, cfg: config::Config) -> Result<(), String> {
    config::save(&cfg).map_err(|e| e.to_string())?;
    set_state(&app, &shared, |ui| ui.cycle_minutes = cfg.cycle_minutes);
    Ok(())
}

#[tauri::command]
fn set_api_key(key: String) -> Result<(), String> {
    if key.trim().is_empty() {
        return Err("key is empty".into());
    }
    secrets::set_anthropic_key(&key).map_err(|e| e.to_string())
}

#[tauri::command]
fn clear_api_key() -> Result<(), String> {
    secrets::delete_anthropic_key().map_err(|e| e.to_string())
}

#[tauri::command]
fn set_usajobs_key(key: String) -> Result<(), String> {
    if key.trim().is_empty() {
        return Err("key is empty".into());
    }
    secrets::set_usajobs_key(&key).map_err(|e| e.to_string())
}

#[tauri::command]
fn clear_usajobs_key() -> Result<(), String> {
    secrets::delete_usajobs_key().map_err(|e| e.to_string())
}

/// Pick a resume and save it to config. None if the user cancelled.
#[tauri::command]
async fn pick_resume(app: AppHandle) -> Result<Option<String>, String> {
    let picked = app
        .dialog()
        .file()
        .set_title("Choose your resume")
        .add_filter("Resume", &["pdf", "txt", "md"])
        .blocking_pick_file();
    let Some(file) = picked else { return Ok(None) };
    let path = file.into_path().map_err(|e| e.to_string())?;
    let mut cfg = config::load_or_create().map_err(|e| e.to_string())?;
    cfg.resume_path = path.to_string_lossy().into_owned();
    config::save(&cfg).map_err(|e| e.to_string())?;
    Ok(Some(cfg.resume_path))
}

/// Rebuild the profile from the configured resume (one LLM call, ~10 s).
#[tauri::command]
async fn rebuild_profile() -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(|| -> anyhow::Result<String> {
        let cfg = config::load_or_create()?;
        let llm = Llm::new(&cfg)?;
        profile::build(&llm, &cfg)?;
        Ok(profile_summary())
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
async fn pick_applied_log(app: AppHandle) -> Result<Option<String>, String> {
    let picked = app
        .dialog()
        .file()
        .set_title("Choose the spreadsheet Blip adds applied roles to")
        .add_filter("Excel workbook", &["xlsx"])
        .blocking_pick_file();
    let Some(file) = picked else { return Ok(None) };
    let path = file.into_path().map_err(|e| e.to_string())?;
    let mut cfg = config::load_or_create().map_err(|e| e.to_string())?;
    cfg.applied_log_path = path.to_string_lossy().into_owned();
    config::save(&cfg).map_err(|e| e.to_string())?;
    Ok(Some(cfg.applied_log_path))
}

#[tauri::command]
fn open_applied_log() -> Result<(), String> {
    let cfg = config::load_or_create().map_err(|e| e.to_string())?;
    let path = cfg.applied_log_path();
    if !path.exists() {
        return Err("Nothing logged yet — it's created on your first ✓".into());
    }
    tauri_plugin_opener::open_path(&path, None::<&str>).map_err(|e| e.to_string())
}

#[derive(Serialize)]
struct HistoryPage {
    rows: Vec<blip_core::store::HistoryRow>,
    counts: blip_core::store::HistoryCounts,
}

const HISTORY_PAGE: i64 = 25;

/// One page of history ("shown" | "applied" | "dismissed"), newest first.
#[tauri::command]
fn get_history(kind: String, offset: i64) -> Result<HistoryPage, String> {
    let store = Store::open(&default_db_path()).map_err(|e| e.to_string())?;
    Ok(HistoryPage {
        rows: store.history(&kind, HISTORY_PAGE, offset.max(0)).map_err(|e| e.to_string())?,
        counts: store.history_counts().map_err(|e| e.to_string())?,
    })
}

/// Undo a dismissal from the history view.
#[tauri::command]
fn restore_job(fingerprint: String) -> Result<(), String> {
    Store::open(&default_db_path())
        .and_then(|s| s.restore(&fingerprint))
        .map_err(|e| e.to_string())
}

/// Look up a company from a name or careers link and verify its board.
#[tauri::command]
async fn find_company(query: String) -> Result<blip_core::sources::Found, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let client = blip_core::http_client().map_err(|e| e.to_string())?;
        blip_core::sources::find_company(&client, &query).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Installed Ollama models, so the Model tab can offer a real list.
#[tauri::command]
async fn list_models() -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(|| -> anyhow::Result<Vec<String>> {
        let cfg = config::load_or_create()?;
        let v: serde_json::Value = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(3))
            .build()?
            .get(format!("{}/api/tags", cfg.ollama_url))
            .send()?
            .json()?;
        Ok(v["models"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|m| m["name"].as_str().map(String::from))
                    .filter(|n| !n.contains("embed"))
                    .collect()
            })
            .unwrap_or_default())
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|_| "Ollama isn't running".to_string())
}

#[tauri::command]
fn quit_app(app: AppHandle) {
    app.exit(0);
}

#[tauri::command]
fn open_link(app: AppHandle, url: String) -> Result<(), String> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("only web links allowed".into());
    }
    tauri_plugin_opener::open_url(&url, None::<&str>).map_err(|e| e.to_string())?;
    let _ = app; // AppHandle kept for future per-window opener scoping
    Ok(())
}

#[tauri::command]
fn set_hit_rect(shared: State<Arc<Shared>>, rect: HitRect) {
    *shared.hit.lock().unwrap() = rect;
}

/// Webviews can't do per-pixel click-through, so poll the cursor and make
/// the whole window click-through whenever it's outside the visible surface.
fn click_through_loop(app: AppHandle, shared: Arc<Shared>) {
    let mut ignoring: Option<bool> = None;
    loop {
        std::thread::sleep(Duration::from_millis(30));
        let Some(win) = app.get_webview_window("main") else { continue };
        let (Ok(cur), Ok(pos), Ok(scale)) =
            (win.cursor_position(), win.outer_position(), win.scale_factor())
        else {
            continue;
        };
        let lx = (cur.x - pos.x as f64) / scale;
        let ly = (cur.y - pos.y as f64) / scale;
        let r = *shared.hit.lock().unwrap();
        let inside = lx >= r.x && lx <= r.x + r.w && ly >= r.y && ly <= r.y + r.h;
        let ignore = !inside;
        if ignoring != Some(ignore) && win.set_ignore_cursor_events(ignore).is_ok() {
            ignoring = Some(ignore);
        }
    }
}

// ---------- the cycle ----------

fn run_cycle(app: &AppHandle, shared: &Shared) {
    set_state(app, shared, |ui| {
        ui.status = "scanning".into();
        ui.message = String::new();
    });

    shared.cancel.store(false, Ordering::SeqCst);
    let cancelled = || shared.cancel.load(Ordering::SeqCst);

    let started = Instant::now();
    let outcome = (|| -> anyhow::Result<(Vec<Scored>, StatsDto, config::Config)> {
        let cfg = config::load_or_create()?;
        let store = Store::open(&default_db_path())?;
        let llm = Llm::new(&cfg)?;
        let prof = profile::load_or_build(&llm, &cfg)?;

        let report = blip_core::run_scan(&store, &cfg, &cancelled)?;
        let candidates = store.unsurfaced(&score::score_key(&cfg, &prof))?;
        let ranked = score::rank(&llm, &cfg, &prof, &store, candidates, cfg.results_per_scan.clamp(1, 10), &cancelled)?;
        store.mark_surfaced(
            &ranked.iter().map(|s| s.posting.fingerprint()).collect::<Vec<_>>(),
        )?;
        let stats = StatsDto {
            scanned: report.scanned,
            new_count: report.new.len(),
            duration_secs: started.elapsed().as_secs(),
            sources_total: report.sources.len(),
            sources_failed: report.sources.iter().filter(|s| !s.ok).count(),
            errors: report.errors,
        };
        Ok((ranked, stats, cfg))
    })();

    match outcome {
        Ok((ranked, stats, cfg)) => {
            let results: Vec<JobDto> = ranked.into_iter().map(to_dto).collect();
            if cfg.notify_enabled {
                notify_strong_matches(app, &results, cfg.notify_threshold);
                send_deadline_reminders(app);
            }
            set_state(app, shared, |ui| {
                // Every source down = loud error; a few down = results plus a
                // footer note, so one flaky board doesn't hide good matches.
                if stats.sources_total > 0 && stats.sources_failed == stats.sources_total {
                    ui.status = "error".into();
                    ui.message = format!("All sources failed. {}", stats.errors.join("; "));
                } else {
                    ui.status = "complete".into();
                    ui.message = String::new();
                }
                ui.results = results;
                ui.stats = stats;
            });
        }
        Err(e) if e.is::<Cancelled>() => set_state(app, shared, |ui| {
            ui.status = if ui.paused { "paused" } else { "rest" }.into();
            ui.message = String::new();
        }),
        Err(e) => set_state(app, shared, |ui| {
            ui.status = "error".into();
            ui.message = format!("{e:#}");
        }),
    }
}

fn to_dto(s: Scored) -> JobDto {
    let deadline = s.deadline.unwrap_or_default();
    let closes_in = score::days_until(&deadline).filter(|d| (0..=CLOSING_SOON_DAYS).contains(d));
    JobDto {
        fingerprint: s.posting.fingerprint(),
        score: s.score,
        reason: s.reason,
        red_flags: s.red_flags,
        company: s.posting.company,
        title: s.posting.title,
        location: s.posting.location,
        url: s.posting.url,
        posted: humanize_age(&s.posting.posted),
        deadline,
        closes_in,
        applied: false,
    }
}

fn notify(app: &AppHandle, title: &str, body: &str) {
    let _ = app.notification().builder().title(title).body(body).show();
}

/// One notification per cycle, and only when something clears the bar.
fn notify_strong_matches(app: &AppHandle, results: &[JobDto], threshold: u8) {
    let strong: Vec<&JobDto> = results.iter().filter(|j| j.score >= threshold).collect();
    let Some(best) = strong.first() else { return };
    let title = if strong.len() == 1 {
        format!("Strong match · {}", best.score)
    } else {
        format!("{} strong matches", strong.len())
    };
    let mut body = format!("{} — {}", best.company, best.title);
    if strong.len() > 1 {
        body.push_str(&format!(" (+{} more)", strong.len() - 1));
    }
    notify(app, &title, &body);
}

/// Shown-but-not-acted-on roles whose stated deadline is near: remind once.
fn send_deadline_reminders(app: &AppHandle) {
    let Ok(store) = Store::open(&default_db_path()) else { return };
    let Ok(due) = store.due_reminders(REMIND_DAYS) else { return };
    for r in due {
        let when = match score::days_until(&r.deadline) {
            Some(0) => "today".to_string(),
            Some(1) => "tomorrow".to_string(),
            Some(d) => format!("in {d} days"),
            None => continue,
        };
        notify(app, &format!("Closes {when}: {}", r.company), &r.title);
        let _ = store.mark_reminded(&r.fingerprint);
    }
}

/// True when on battery power below the threshold (0 disables).
fn battery_low(threshold: u8) -> bool {
    if threshold == 0 {
        return false;
    }
    let Ok(manager) = starship_battery::Manager::new() else { return false };
    let Ok(batteries) = manager.batteries() else { return false };
    batteries.flatten().any(|b| {
        b.state() == starship_battery::State::Discharging
            && b.state_of_charge().value * 100.0 < threshold as f32
    })
}

fn humanize_age(posted: &str) -> String {
    match score::age_days(posted) {
        None => String::new(),
        Some(d) if d < 1.0 => "today".into(),
        Some(d) if d < 2.0 => "1 d ago".into(),
        Some(d) if d < 30.0 => format!("{} d ago", d as u64),
        Some(d) => format!("{} mo ago", (d / 30.0) as u64),
    }
}

fn scheduler(app: AppHandle, shared: Arc<Shared>, rx: Receiver<()>) {
    // First cycle shortly after launch, then on the configured interval.
    std::thread::sleep(Duration::from_secs(2));
    let mut manual = false;
    loop {
        let cfg = config::load_or_create().unwrap_or_default();
        // Manual scans run even while paused, off-hours, or on low battery;
        // automatic ones respect all three.
        // Until first-run setup is done there's nothing to score against.
        let allowed = manual || {
            let paused = shared.ui.lock().unwrap().paused;
            let in_hours = cfg.is_active_hour(chrono::Local::now().hour() as u8);
            !cfg.needs_setup() && !paused && in_hours && !battery_low(cfg.battery_pause_below)
        };
        if allowed {
            run_cycle(&app, &shared);
        }
        // Scan requests made while a cycle was running don't queue another.
        while rx.try_recv().is_ok() {}

        let minutes = config::load_or_create().map(|c| c.cycle_minutes).unwrap_or(30);
        shared.ui.lock().unwrap().cycle_minutes = minutes;
        // Exactly one cycle per wake-up: a scan_now message cuts the wait
        // short and *replaces* the automatic cycle, it doesn't add one.
        manual = match rx.recv_timeout(Duration::from_secs(minutes.max(1) * 60)) {
            Ok(()) => true,
            Err(mpsc::RecvTimeoutError::Timeout) => false,
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        };
    }
}

pub fn run() {
    let (tx, rx) = mpsc::channel::<()>();
    let cfg = config::load_or_create().unwrap_or_default();
    let shared = Arc::new(Shared {
        ui: Mutex::new(UiState {
            status: if cfg.needs_setup() { "setup" } else { "rest" }.into(),
            cycle_minutes: cfg.cycle_minutes,
            ..Default::default()
        }),
        scan_now: tx,
        hit: Mutex::new(INITIAL_HIT),
        cancel: AtomicBool::new(false),
    });

    let shared_for_setup = shared.clone();
    tauri::Builder::default()
        // Must be first: a second launch exits and leaves the running pill alone.
        .plugin(tauri_plugin_single_instance::init(|_app, _args, _cwd| {}))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .manage(shared)
        .invoke_handler(tauri::generate_handler![
            setup::setup_status,
            setup::start_ollama,
            setup::pull_models,
            setup::finish_setup,
            get_state,
            scan_now,
            cancel_scan,
            source_health,
            export_week,
            get_autostart,
            set_autostart,
            toggle_pause,
            dismiss_job,
            mark_applied,
            get_settings,
            save_settings,
            set_api_key,
            clear_api_key,
            set_usajobs_key,
            clear_usajobs_key,
            pick_resume,
            rebuild_profile,
            pick_applied_log,
            open_applied_log,
            get_history,
            restore_job,
            find_company,
            list_models,
            open_link,
            quit_app,
            set_hit_rect
        ])
        .setup(move |app| {
            // A menu-bar-style utility: no Dock icon, no app menu takeover.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            // Pin to the top-right of the monitor, under the menu bar. The
            // surface sits 16px inside the canvas, so the pill lands ~20px
            // from the screen edge.
            if let Some(win) = app.get_webview_window("main") {
                if let Ok(Some(mon)) = win.current_monitor() {
                    let scale = mon.scale_factor();
                    // The canvas is transparent and click-through, so make it
                    // as tall as the screen allows: settings never scroll, and
                    // the tallest card (a long company list) still fits.
                    let screen_h = mon.size().height as f64 / scale;
                    let height = (screen_h - 32.0 - 24.0).clamp(540.0, 1100.0);
                    let _ = win.set_size(tauri::LogicalSize::new(WIN_W, height));
                    let pad = (4.0 * scale) as i32;
                    let top = (32.0 * scale) as i32;
                    let width = (WIN_W * scale).round() as i32;
                    let x = mon.position().x + mon.size().width as i32 - width - pad;
                    let y = mon.position().y + top;
                    let _ = win.set_position(tauri::PhysicalPosition::new(x, y));
                }
            }
            let handle = app.handle().clone();
            let hit_handle = app.handle().clone();
            let hit_shared = shared_for_setup.clone();
            std::thread::spawn(move || click_through_loop(hit_handle, hit_shared));
            std::thread::spawn(move || scheduler(handle, shared_for_setup, rx));
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Blip");
}
