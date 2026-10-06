// Blip pill frontend: renders UiState from Rust, drives window resizing.
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const SIZES = {
  pill: { width: 230, height: 52 },
  panel: { width: 380, height: 470 },
  settings: { width: 330, height: 250 },
};

const el = (id) => document.getElementById(id);
const pill = el("pill"), panel = el("panel"), setpanel = el("setpanel");

let state = { status: "rest", results: [], stats: {}, cycle_minutes: 30, message: "" };
let view = "pill"; // "pill" | "panel" | "settings"
let lastStatus = "";

const LABELS = {
  rest: "Resting",
  scanning: "Scanning",
  complete: "Complete",
  paused: "Paused",
  error: "Error",
};

async function setView(next) {
  view = next;
  const size = next === "pill" ? SIZES.pill : next === "panel" ? SIZES.panel : SIZES.settings;
  pill.classList.toggle("hide", next !== "pill");
  panel.classList.toggle("hide", next !== "panel");
  setpanel.classList.toggle("hide", next !== "settings");
  try { await invoke("resize_window", size); } catch (_) {}
}

function render() {
  pill.dataset.s = state.status;
  el("pilltext").textContent = LABELS[state.status] || state.status;
  el("cycletxt").textContent = `every ${state.cycle_minutes} min`;
  el("pausebtn").textContent = state.status === "paused" ? "▶ resume" : "⏸ pause";

  const hasErr = state.status === "error" && state.message;
  el("errrow").hidden = !hasErr;
  if (hasErr) el("errtxt").textContent = state.message;

  renderJobs();
  renderStats();
}

function renderJobs() {
  const jobs = el("jobs");
  jobs.textContent = "";
  if (!state.results.length) {
    const d = document.createElement("div");
    d.className = "empty";
    d.textContent = state.status === "scanning"
      ? "Scanning…"
      : "No matches this cycle. Next scan will look again.";
    jobs.appendChild(d);
    return;
  }
  for (const j of state.results) {
    const row = document.createElement("div");
    row.className = "job";

    const score = document.createElement("span");
    score.className = "score";
    score.textContent = j.score;

    const main = document.createElement("div");
    main.className = "jmain";
    const title = document.createElement("div");
    title.className = "jtitle";
    title.textContent = j.title;
    title.title = "Open posting";
    title.onclick = () => {
      invoke("open_link", { url: j.url }).catch(() => {});
      invoke("job_action", { fingerprint: j.fingerprint, action: "viewed" }).catch(() => {});
    };
    const meta = document.createElement("div");
    meta.className = "jmeta";
    meta.textContent = [j.company, j.location, j.posted && `posted ${j.posted}`]
      .filter(Boolean).join(" · ");
    main.append(title, meta);
    if (j.reason) {
      const r = document.createElement("div");
      r.className = "jreason";
      r.textContent = j.reason;
      main.appendChild(r);
    }
    for (const f of j.red_flags || []) {
      const fl = document.createElement("div");
      fl.className = "jflag";
      fl.textContent = `⚑ ${f}`;
      main.appendChild(fl);
    }

    const acts = document.createElement("div");
    acts.className = "acts";
    const yes = document.createElement("button");
    yes.textContent = "✓";
    yes.title = "Applied — log it";
    yes.onclick = () => {
      invoke("job_action", { fingerprint: j.fingerprint, action: "applied" }).catch(() => {});
      row.classList.add("done");
    };
    const no = document.createElement("button");
    no.textContent = "✕";
    no.title = "Dismiss — never show again";
    no.onclick = () => {
      invoke("job_action", { fingerprint: j.fingerprint, action: "dismissed" }).catch(() => {});
      row.remove();
    };
    acts.append(yes, no);

    row.append(score, main, acts);
    jobs.appendChild(row);
  }
}

function renderStats() {
  const s = state.stats || {};
  const bits = [];
  if (s.duration_secs != null) bits.push(`cycle ${s.duration_secs}s`);
  if (s.scanned != null) bits.push(`${s.scanned} scanned`);
  if (s.new_count != null) bits.push(`${s.new_count} new`);
  if (s.errors && s.errors.length) bits.push(`⚠ ${s.errors.length} source error(s)`);
  el("stats").textContent = bits.join(" · ") || "no cycle yet";
}

// ---- interactions ----

pill.addEventListener("click", (e) => {
  if (e.target.id === "gear") return;
  if (state.status === "complete" && state.results.length) setView("panel");
  else if (state.status !== "scanning") invoke("scan_now").catch(() => {});
});
el("gear").addEventListener("click", (e) => { e.stopPropagation(); setView("settings"); });
el("setclose").addEventListener("click", () => setView("pill"));
el("minbtn").addEventListener("click", () => setView("pill"));
el("scannow").addEventListener("click", () => { invoke("scan_now").catch(() => {}); setView("pill"); });
el("pausebtn").addEventListener("click", () => invoke("toggle_pause").catch(() => {}));
el("quitbtn").addEventListener("click", () => invoke("quit_app").catch(() => {}));
el("openall").addEventListener("click", () => {
  for (const j of state.results) invoke("open_link", { url: j.url }).catch(() => {});
});

// ---- state sync ----

function onState(next) {
  state = next;
  // Fresh results arriving: auto-expand, like the mockup.
  if (state.status === "complete" && lastStatus === "scanning" && state.results.length) {
    setView("panel");
  }
  // A new scan starting collapses any open panel back to the pill.
  if (state.status === "scanning" && view === "panel") setView("pill");
  lastStatus = state.status;
  render();
}

listen("blip-state", (e) => onState(e.payload));
invoke("get_state").then(onState).catch(() => render());
