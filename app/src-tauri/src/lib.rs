//! Blip pill app: a frameless always-on-top window whose Rust side runs the
//! blip-core pipeline on a schedule and feeds state to the HTML pill UI.

use blip_core::score::{self, Scored};
use blip_core::store::{default_db_path, Store};
use blip_core::{config, llm::Llm, profile};
use serde::Serialize;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, State};

const TOP_N: usize = 5;

#[derive(Clone, Serialize, Default)]
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
}

#[derive(Clone, Serialize, Default)]
struct StatsDto {
    scanned: usize,
    new_count: usize,
    duration_secs: u64,
    errors: Vec<String>,
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
}

struct Shared {
    ui: Mutex<UiState>,
    scan_now: Sender<()>,
    hit: Mutex<HitRect>,
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
const INITIAL_HIT: HitRect = HitRect { x: WIN_W - 16.0 - 136.0, y: 8.0, w: 136.0, h: 40.0 };

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
    let _ = shared.scan_now.send(());
}

#[tauri::command]
fn toggle_pause(app: AppHandle, shared: State<Arc<Shared>>) {
    set_state(&app, &shared, |ui| {
        if ui.status == "paused" {
            ui.status = "rest".into();
            ui.message = String::new();
        } else {
            ui.status = "paused".into();
        }
    });
}

#[tauri::command]
fn job_action(fingerprint: String, action: String) -> Result<(), String> {
    let status = match action.as_str() {
        "applied" => "applied",
        "dismissed" => "dismissed",
        "viewed" => return Ok(()), // Phase 3: track views too
        _ => return Err(format!("unknown action {action}")),
    };
    let store = Store::open(&default_db_path()).map_err(|e| e.to_string())?;
    store.set_status(&fingerprint, status).map_err(|e| e.to_string())
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

    let started = Instant::now();
    let outcome = (|| -> anyhow::Result<(Vec<Scored>, StatsDto)> {
        let cfg = config::load_or_create()?;
        let store = Store::open(&default_db_path())?;
        let llm = Llm::new(&cfg)?;
        let prof = profile::load_or_build(&llm, &cfg)?;

        let report = blip_core::run_scan(&store)?;
        let candidates = store.unsurfaced()?;
        let ranked = score::rank(&llm, &cfg, &prof, candidates, TOP_N)?;
        store.mark_surfaced(
            &ranked.iter().map(|s| s.posting.fingerprint()).collect::<Vec<_>>(),
        )?;
        Ok((
            ranked,
            StatsDto {
                scanned: report.scanned,
                new_count: report.new.len(),
                duration_secs: started.elapsed().as_secs(),
                errors: report.errors,
            },
        ))
    })();

    match outcome {
        Ok((ranked, stats)) => {
            let results: Vec<JobDto> = ranked
                .into_iter()
                .map(|s| JobDto {
                    fingerprint: s.posting.fingerprint(),
                    score: s.score,
                    reason: s.reason,
                    red_flags: s.red_flags,
                    company: s.posting.company,
                    title: s.posting.title,
                    location: s.posting.location,
                    url: s.posting.url,
                    posted: humanize_age(&s.posting.posted),
                })
                .collect();
            set_state(app, shared, |ui| {
                // Many failed sources with zero yield = loud error, not quiet success.
                if results.is_empty() && !stats.errors.is_empty() {
                    ui.status = "error".into();
                    ui.message = stats.errors.join("; ");
                } else {
                    ui.status = "complete".into();
                }
                ui.results = results;
                ui.stats = stats;
            });
        }
        Err(e) => set_state(app, shared, |ui| {
            ui.status = "error".into();
            ui.message = e.to_string();
        }),
    }
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
    loop {
        {
            let paused = shared.ui.lock().unwrap().status == "paused";
            if !paused {
                run_cycle(&app, &shared);
            }
        }
        let minutes = config::load_or_create().map(|c| c.cycle_minutes).unwrap_or(30);
        {
            let mut ui = shared.ui.lock().unwrap();
            ui.cycle_minutes = minutes;
        }
        // Wait out the interval; a scan_now message cuts it short. Manual
        // scans run even while paused.
        match rx.recv_timeout(Duration::from_secs(minutes.max(1) * 60)) {
            Ok(()) => {
                run_cycle(&app, &shared);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

pub fn run() {
    let (tx, rx) = mpsc::channel::<()>();
    let shared = Arc::new(Shared {
        ui: Mutex::new(UiState {
            status: "rest".into(),
            cycle_minutes: 30,
            ..Default::default()
        }),
        scan_now: tx,
        hit: Mutex::new(INITIAL_HIT),
    });

    let shared_for_setup = shared.clone();
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(shared)
        .invoke_handler(tauri::generate_handler![
            get_state,
            scan_now,
            toggle_pause,
            job_action,
            open_link,
            quit_app,
            set_hit_rect
        ])
        .setup(move |app| {
            // Pin to the top-right of the monitor, under the menu bar. The
            // surface sits 16px inside the canvas, so the pill lands ~20px
            // from the screen edge.
            if let Some(win) = app.get_webview_window("main") {
                if let (Ok(Some(mon)), Ok(size)) = (win.current_monitor(), win.outer_size()) {
                    let scale = win.scale_factor().unwrap_or(1.0);
                    let pad = (4.0 * scale) as i32;
                    let top = (32.0 * scale) as i32;
                    let x = mon.position().x + mon.size().width as i32 - size.width as i32 - pad;
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
