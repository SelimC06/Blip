// Blip pill frontend. One surface morphs between pill and cards using a
// physically simulated spring; content layers crossfade inside it.
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const el = (id) => document.getElementById(id);
const surface = el("surface");
const LAYERS = { pill: el("pillLayer"), panel: el("panelLayer"), settings: el("setLayer") };
const wait = (ms) => new Promise((r) => setTimeout(r, ms));
const reduceMotion = matchMedia("(prefers-reduced-motion: reduce)").matches;

const PILL = { w: 136, h: 40, r: 20 };
const CARD_RADIUS = 16;
const SURFACE_RIGHT = 16, SURFACE_TOP = 8;

const ICONS = {
  play: '<svg viewBox="0 0 24 24" fill="currentColor"><path d="M6 4.6v14.8a1 1 0 0 0 1.52.85l12-7.4a1 1 0 0 0 0-1.7l-12-7.4A1 1 0 0 0 6 4.6z"></path></svg>',
  pause: '<svg viewBox="0 0 24 24" fill="currentColor"><rect x="5.5" y="4" width="4.5" height="16" rx="1.2"></rect><rect x="14" y="4" width="4.5" height="16" rx="1.2"></rect></svg>',
  check: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round"><polyline points="20 6 9 17 4 12"></polyline></svg>',
  cross: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round"><line x1="18" y1="6" x2="6" y2="18"></line><line x1="6" y1="6" x2="18" y2="18"></line></svg>',
};

const LABELS = { rest: "Resting", scanning: "Scanning", complete: "Complete", paused: "Paused", error: "Error" };

let state = { status: "rest", results: [], stats: {}, cycle_minutes: 30, message: "" };
let view = "pill";
let lastStatus = "";

// ---------- spring physics ----------

// Damped harmonic oscillator from 0 → 1, sampled at 120 Hz until settled.
function springCurve(stiffness, damping) {
  const dt = 1 / 120;
  let x = 0, v = 0;
  const pts = [0];
  for (let i = 0; i < 480; i++) {
    v += (stiffness * (1 - x) - damping * v) * dt;
    x += v * dt;
    pts.push(x);
    if (Math.abs(1 - x) < 0.0004 && Math.abs(v) < 0.004) break;
  }
  pts[pts.length - 1] = 1;
  return { pts, ms: (pts.length - 1) * dt * 1000 };
}
// Expand: lively, ~3% overshoot so the card lands with weight.
const SPRING_OPEN = springCurve(340, 28);
// Collapse: tighter and quicker, a hair of overshoot as it seats into the pill.
const SPRING_CLOSE = springCurve(460, 38);

let morphAnim = null;

function currentDims() {
  const r = surface.getBoundingClientRect();
  return { w: r.width, h: r.height, r: parseFloat(getComputedStyle(surface).borderTopLeftRadius) || 0 };
}

function setDims(d) {
  surface.style.width = `${d.w}px`;
  surface.style.height = `${d.h}px`;
  surface.style.borderRadius = `${d.r}px`;
}

// Animate the surface's shape along a spring. Interrupt-safe: starts from
// wherever the surface is right now, even mid-flight.
function morph(to, spring) {
  const from = currentDims();
  if (morphAnim) { setDims(from); morphAnim.cancel(); }
  if (reduceMotion) { setDims(to); return Promise.resolve(); }

  const step = 2; // 60 keyframes/s is plenty; WAAPI interpolates between
  const frames = [];
  for (let i = 0; i < spring.pts.length; i += step) {
    const p = spring.pts[i];
    frames.push({
      width: `${from.w + (to.w - from.w) * p}px`,
      height: `${from.h + (to.h - from.h) * p}px`,
      borderRadius: `${Math.max(0, from.r + (to.r - from.r) * p)}px`,
    });
  }
  frames.push({ width: `${to.w}px`, height: `${to.h}px`, borderRadius: `${to.r}px` });

  const anim = surface.animate(frames, { duration: spring.ms, easing: "linear", fill: "forwards" });
  morphAnim = anim;
  return anim.finished.then(() => {
    if (morphAnim !== anim) return;
    setDims(to);
    anim.cancel();
    morphAnim = null;
  }, () => {});
}

// Content crossfades. Each layer has at most one running fade.
const fades = new WeakMap();
function fade(layer, show, { delay = 0, duration } = {}) {
  const prev = fades.get(layer);
  const startOpacity = getComputedStyle(layer).opacity;
  if (prev) prev.cancel();
  if (reduceMotion) { layer.style.opacity = show ? 1 : 0; return Promise.resolve(); }
  const frames = show
    ? [{ opacity: startOpacity, transform: "translateY(-5px) scale(.985)", filter: "blur(2.5px)" },
       { opacity: 1, transform: "none", filter: "blur(0)" }]
    : [{ opacity: startOpacity, transform: "none", filter: "blur(0)" },
       { opacity: 0, transform: "translateY(-3px) scale(.99)", filter: "blur(2px)" }];
  const anim = layer.animate(frames, {
    duration: duration ?? (show ? 260 : 110),
    delay,
    easing: show ? "cubic-bezier(.2,.8,.2,1)" : "cubic-bezier(.4,0,1,1)",
    fill: "both",
  });
  fades.set(layer, anim);
  return anim.finished.then(() => {
    if (fades.get(layer) !== anim) return;
    layer.style.opacity = show ? 1 : 0;
    anim.cancel();
    fades.delete(layer);
  }, () => {});
}

// Results rows cascade in, a beat apart.
function staggerRows(baseDelay) {
  if (reduceMotion) return;
  [...el("jobs").children].forEach((row, i) => {
    row.animate(
      [{ opacity: 0, transform: "translateY(-6px)" }, { opacity: 1, transform: "none" }],
      { duration: 280, delay: baseDelay + i * 38, easing: "cubic-bezier(.2,.8,.2,1)", fill: "backwards" }
    );
  });
}

// ---------- view transitions ----------

function targetDims(v) {
  if (v === "pill") return PILL;
  const layer = LAYERS[v];
  // Layers are always laid out (just invisible), so this measures for real.
  let h = layer.offsetHeight;
  if (v === "settings") {
    // The layer may be stretched to the surface; use the visible pane's own
    // height instead of whatever the pane area currently happens to be.
    const panes = el("panes");
    const pane = panes.querySelector(`.pane[data-pane="${activeTab}"]`);
    h = h - panes.offsetHeight + pane.offsetHeight;
  }
  return { w: layer.offsetWidth + 2, h: Math.min(h, 470) + 2, r: CARD_RADIUS };
}

function reportHitRect(d) {
  const rect = { x: window.innerWidth - SURFACE_RIGHT - d.w, y: SURFACE_TOP, w: d.w, h: d.h };
  invoke("set_hit_rect", { rect }).catch(() => {});
}

let seq = 0;

async function setView(next) {
  if (next === view) return;
  const my = ++seq;
  const prev = view;
  view = next;

  // Un-stretch before leaving settings: the surface is at the card's natural
  // height at this point, so nothing visibly moves.
  if (prev === "settings") LAYERS.settings.classList.remove("stretch");

  const to = targetDims(next);
  const opening = next !== "pill";
  // Grow the clickable area up front when opening; shrink it only once the
  // surface has actually landed on the pill.
  if (opening) reportHitRect(to);

  for (const [name, layer] of Object.entries(LAYERS)) layer.classList.toggle("on", name === next);
  surface.dataset.view = next;

  fade(LAYERS[prev], false);
  if (opening) {
    const morphing = morph(to, SPRING_OPEN);
    fade(LAYERS[next], true, { delay: 70 });
    if (next === "panel") staggerRows(110);
    await morphing;
  } else {
    await wait(55); // let the card's content clear before it folds up
    if (my !== seq) return;
    const morphing = morph(to, SPRING_CLOSE);
    fade(LAYERS.pill, true, { delay: SPRING_CLOSE.ms * 0.35, duration: 200 });
    await morphing;
  }
  if (my !== seq) return;
  reportHitRect(to);
  if (next === "settings") LAYERS.settings.classList.add("stretch");
}

// Card resizes in place (tab switch, row dismissed): a firm spring with a
// whisper of overshoot. Keep the larger click area until it lands.
const SPRING_FIT = springCurve(420, 32);
async function refit() {
  if (view === "pill") return;
  const to = targetDims(view);
  const now = currentDims();
  reportHitRect({ w: Math.max(now.w, to.w), h: Math.max(now.h, to.h) });
  const landedOn = view;
  await morph(to, SPRING_FIT);
  if (view === landedOn) reportHitRect(to);
}

// ---------- rendering ----------

function render() {
  surface.dataset.status = state.status;
  el("pilltext").textContent = LABELS[state.status] || state.status;
  el("pausebtn").innerHTML = state.status === "paused" ? `${ICONS.play}resume` : `${ICONS.pause}pause`;

  const hasErr = state.status === "error" && state.message;
  el("errrow").hidden = !hasErr;
  if (hasErr) el("errtxt").textContent = state.message;

  renderJobs();
  renderStats();
  renderSettings();
}

function renderJobs() {
  const jobs = el("jobs");
  jobs.textContent = "";
  if (!state.results.length) {
    const d = document.createElement("div");
    d.className = "empty";
    d.textContent = state.status === "scanning" ? "Scanning…" : "No new matches this cycle.";
    jobs.appendChild(d);
    return;
  }
  for (const j of state.results) {
    const row = document.createElement("div");
    row.className = "job";

    const score = document.createElement("span");
    score.className = j.score >= 90 ? "score hot" : "score";
    score.textContent = j.score;

    const main = document.createElement("div");
    main.className = "jmain";
    const title = document.createElement("div");
    title.className = "jtitle";
    title.textContent = j.title;
    title.title = "Open posting";
    title.onclick = () => invoke("open_link", { url: j.url }).catch(() => {});
    const meta = document.createElement("div");
    meta.className = "jmeta";
    meta.textContent = [j.company, j.location, j.posted && `posted ${j.posted}`].filter(Boolean).join(" · ");
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
    yes.className = "yes";
    yes.title = "Applied — log it";
    yes.setAttribute("aria-label", "Mark applied");
    yes.innerHTML = ICONS.check;
    yes.onclick = async () => {
      if (row.classList.contains("done")) return;
      row.classList.add("done");
      try {
        const file = await invoke("mark_applied", { job: j });
        notice(`added to ${file}`);
      } catch (err) {
        row.classList.remove("done");
        notice(String(err), true);
      }
    };
    const no = document.createElement("button");
    no.className = "no";
    no.title = "Dismiss — never show again";
    no.setAttribute("aria-label", "Dismiss");
    no.innerHTML = ICONS.cross;
    no.onclick = () => dismissRow(row, j);
    acts.append(yes, no);

    row.append(score, main, acts);
    jobs.appendChild(row);
  }
}

// Dismissed rows slide out, collapse their height, and the card refits.
async function dismissRow(row, j) {
  invoke("dismiss_job", { fingerprint: j.fingerprint }).catch(() => {});
  state.results = state.results.filter((r) => r.fingerprint !== j.fingerprint);
  if (!reduceMotion) {
    const h = row.offsetHeight;
    await row.animate(
      [{ opacity: 1, transform: "none", height: `${h}px` },
       { opacity: 0, transform: "translateX(18px)", height: `${h}px`, offset: 0.45 },
       { opacity: 0, transform: "translateX(18px)", height: "0px", paddingTop: "0px", paddingBottom: "0px" }],
      { duration: 300, easing: "cubic-bezier(.4,0,.2,1)" }
    ).finished.catch(() => {});
  }
  row.remove();
  if (!state.results.length) renderJobs();
  refit();
}

// Briefly replaces the stats footer with a confirmation or an error.
let noticeTimer = null;
function notice(text, bad = false) {
  const f = el("stats");
  clearTimeout(noticeTimer);
  f.textContent = text;
  f.title = text;
  f.className = bad ? "note bad" : "note";
  noticeTimer = setTimeout(() => { f.className = ""; f.title = ""; renderStats(); }, bad ? 6000 : 2200);
}

function renderStats() {
  if (el("stats").classList.contains("note")) return;
  const s = state.stats || {};
  const bits = [];
  if (s.duration_secs != null) bits.push(`cycle ${s.duration_secs}s`);
  if (s.scanned != null) bits.push(`${s.scanned} scanned`);
  if (s.new_count != null) bits.push(`${s.new_count} new`);
  if (s.errors && s.errors.length) bits.push(`${s.errors.length} source error${s.errors.length > 1 ? "s" : ""}`);
  el("stats").textContent = bits.join("  ·  ") || "no cycle yet";
}

// ---------- interactions ----------

LAYERS.pill.addEventListener("click", (e) => {
  if (e.target.closest("#gear")) return;
  if (state.status === "complete" && state.results.length) setView("panel");
  else if (state.status !== "scanning") invoke("scan_now").catch(() => {});
});
el("gear").addEventListener("click", async (e) => {
  e.stopPropagation();
  await loadSettings(); // render the card before measuring it for the morph
  setView("settings");
});
el("setclose").addEventListener("click", () => setView("pill"));
el("minbtn").addEventListener("click", () => setView("pill"));
el("scannow").addEventListener("click", () => { invoke("scan_now").catch(() => {}); setView("pill"); });
el("pausebtn").addEventListener("click", () => invoke("toggle_pause").catch(() => {}));
el("quitbtn").addEventListener("click", () => invoke("quit_app").catch(() => {}));
el("openall").addEventListener("click", () => {
  for (const j of state.results) invoke("open_link", { url: j.url }).catch(() => {});
});

// ---------- settings ----------

let settings = null; // { config, has_api_key, applied_log_resolved, applied_log_exists, profile_summary }
let activeTab = "profile";
let saveTimer = null;

const hourLabel = (h) => (h === 0 || h === 24 ? "12 am" : h === 12 ? "12 pm" : h < 12 ? `${h} am` : `${h - 12} pm`);
for (const id of ["hstart", "hend"]) {
  for (let h = 0; h < 24; h++) el(id).add(new Option(hourLabel(h), h));
}

const basename = (p) => (p || "").split(/[\\/]/).pop();

async function loadSettings() {
  try { settings = await invoke("get_settings"); } catch (err) { console.error(err); return; }
  renderSettings();
}

function setSeg(id, value) {
  for (const b of el(id).children) b.classList.toggle("on", b.dataset.v === String(value));
}

function renderSettings() {
  if (!settings) return;
  const c = settings.config;

  el("resumename").textContent = c.resume_path ? basename(c.resume_path) : "none yet — choose your resume";
  el("resumesummary").textContent = settings.profile_summary;
  if (document.activeElement !== el("lookingfor")) el("lookingfor").value = c.looking_for;

  for (const b of el("roletypes").children) b.classList.toggle("on", c.role_types.includes(b.dataset.v));
  if (document.activeElement !== el("season")) el("season").value = c.season;
  setSeg("maxage", c.max_age_days);
  el("skipadv").classList.toggle("on", c.exclude_advanced_degree);
  el("skipadv").setAttribute("aria-checked", c.exclude_advanced_degree);

  setSeg("interval", c.cycle_minutes);
  el("hstart").value = c.active_start_hour;
  el("hend").value = c.active_end_hour;

  setSeg("backend", c.backend);
  el("ollamafields").hidden = c.backend !== "ollama";
  el("anthropicfields").hidden = c.backend !== "anthropic";
  if (document.activeElement !== el("amodel")) el("amodel").value = c.anthropic_model;
  const ks = el("keystatus");
  ks.textContent = settings.has_api_key ? "Saved in your Keychain." : "No key saved.";
  ks.className = settings.has_api_key ? "fhint ok" : "fhint";
  el("savekey").textContent = settings.has_api_key ? "replace" : "save";

  el("logpath").textContent = settings.applied_log_resolved;
  el("openlog").disabled = !settings.applied_log_exists;

  const hasErr = state.status === "error" && state.message;
  el("errrow").hidden = !hasErr;
  if (hasErr) el("errtxt").textContent = state.message;
}

// Every change saves itself shortly after; the footer confirms.
function changed(mutate) {
  mutate(settings.config);
  renderSettings();
  clearTimeout(saveTimer);
  saveTimer = setTimeout(async () => {
    try {
      await invoke("save_settings", { cfg: settings.config });
      flashSaved("saved");
    } catch (err) {
      flashSaved(`couldn't save: ${err}`, true);
    }
  }, 350);
}

let savedTimer = null;
function flashSaved(text, bad = false) {
  const s = el("savedtxt");
  s.textContent = text;
  s.style.color = bad ? "var(--red)" : "";
  s.style.opacity = 1;
  clearTimeout(savedTimer);
  savedTimer = setTimeout(() => { s.style.opacity = 0; }, bad ? 5000 : 1400);
}

const TAB_ORDER = ["profile", "search", "cycle", "model", "log"];
const paneFor = (t) => document.querySelector(`.pane[data-pane="${t}"]`);

function moveTabIndicator(instant = false) {
  const btn = el("tabs").querySelector(`button[data-tab="${activeTab}"]`);
  const ind = el("tabind");
  ind.classList.toggle("instant", instant);
  ind.style.width = `${btn.offsetWidth}px`;
  ind.style.transform = `translateX(${btn.offsetLeft}px)`;
  if (instant) requestAnimationFrame(() => ind.classList.remove("instant"));
}

function showTab(tab) {
  if (tab === activeTab) return;
  const prevTab = activeTab;
  const dir = TAB_ORDER.indexOf(tab) > TAB_ORDER.indexOf(prevTab) ? 1 : -1;
  activeTab = tab;

  for (const b of el("tabs").querySelectorAll("button")) {
    b.classList.toggle("on", b.dataset.tab === tab);
    b.setAttribute("aria-selected", b.dataset.tab === tab);
  }
  moveTabIndicator();

  // Settle any half-finished switch from a fast double click.
  for (const p of document.querySelectorAll(".pane")) {
    p.getAnimations().forEach((a) => a.cancel());
    p.style.position = "";
    p.hidden = p.dataset.pane !== prevTab;
  }

  const out = paneFor(prevTab);
  const inn = paneFor(tab);
  if (!reduceMotion) {
    // Outgoing pane floats out of the flow so it can fade over the incoming one.
    Object.assign(out.style, { position: "absolute", top: "0", left: "0", right: "0" });
    out.animate(
      [{ opacity: 1, transform: "none" }, { opacity: 0, transform: `translateX(${-dir * 14}px)` }],
      { duration: 150, easing: "cubic-bezier(.4,0,1,1)", fill: "forwards" }
    ).finished.then(() => {
      if (activeTab !== prevTab) out.hidden = true;
      out.style.position = "";
      out.getAnimations().forEach((a) => a.cancel());
    }, () => {});
    inn.hidden = false;
    inn.animate(
      [{ opacity: 0, transform: `translateX(${dir * 18}px)`, filter: "blur(2px)" },
       { opacity: 1, transform: "none", filter: "blur(0)" }],
      { duration: 300, delay: 70, easing: "cubic-bezier(.2,.8,.2,1)", fill: "backwards" }
    );
  } else {
    out.hidden = true;
    inn.hidden = false;
  }

  if (tab === "model") refreshModels();
  refit();
}

async function refreshModels() {
  const sel = el("chatmodel");
  const current = settings?.config.chat_model;
  try {
    const models = await invoke("list_models");
    sel.textContent = "";
    for (const m of models) sel.add(new Option(m, m));
    if (current && !models.includes(current)) sel.add(new Option(`${current} (not installed)`, current));
    sel.value = current;
    el("modelhint").textContent = "Runs on this machine. Nothing leaves it.";
    el("modelhint").className = "fhint";
  } catch (err) {
    sel.textContent = "";
    sel.add(new Option(current || "—", current || ""));
    el("modelhint").textContent = `${err}. Start it with: ollama serve`;
    el("modelhint").className = "fhint bad";
  }
  refit();
}

el("tabs").addEventListener("click", (e) => {
  const b = e.target.closest("button[data-tab]");
  if (b) showTab(b.dataset.tab);
});

el("pickresume").addEventListener("click", async () => {
  const btn = el("pickresume");
  const sum = el("resumesummary");
  btn.disabled = true;
  try {
    const picked = await invoke("pick_resume");
    if (picked === null) return;
    await loadSettings();
    sum.textContent = "Reading your resume… about 10 seconds";
    sum.className = "fhint";
    refit();
    await invoke("rebuild_profile");
    await loadSettings();
    el("resumesummary").className = "fhint ok";
    flashSaved("profile rebuilt");
  } catch (err) {
    sum.textContent = String(err);
    sum.className = "fhint bad";
  } finally {
    btn.disabled = false;
    refit();
  }
});

el("lookingfor").addEventListener("input", (e) => changed((c) => { c.looking_for = e.target.value; }));
el("season").addEventListener("input", (e) => changed((c) => { c.season = e.target.value.trim(); }));
el("amodel").addEventListener("input", (e) => changed((c) => { c.anthropic_model = e.target.value.trim(); }));

el("roletypes").addEventListener("click", (e) => {
  const b = e.target.closest("button");
  if (!b) return;
  changed((c) => {
    const v = b.dataset.v;
    c.role_types = c.role_types.includes(v) ? c.role_types.filter((x) => x !== v) : [...c.role_types, v];
  });
});
el("maxage").addEventListener("click", (e) => {
  const b = e.target.closest("button");
  if (b) changed((c) => { c.max_age_days = Number(b.dataset.v); });
});
el("skipadv").addEventListener("click", () => changed((c) => { c.exclude_advanced_degree = !c.exclude_advanced_degree; }));
el("interval").addEventListener("click", (e) => {
  const b = e.target.closest("button");
  if (b) changed((c) => { c.cycle_minutes = Number(b.dataset.v); });
});
el("hstart").addEventListener("change", (e) => changed((c) => { c.active_start_hour = Number(e.target.value); }));
el("hend").addEventListener("change", (e) => changed((c) => { c.active_end_hour = Number(e.target.value); }));
el("backend").addEventListener("click", (e) => {
  const b = e.target.closest("button");
  if (!b) return;
  changed((c) => { c.backend = b.dataset.v; });
  refit();
});
el("chatmodel").addEventListener("change", (e) => changed((c) => { c.chat_model = e.target.value; }));

el("savekey").addEventListener("click", async () => {
  const input = el("apikey");
  const ks = el("keystatus");
  if (!input.value.trim()) {
    ks.textContent = "Paste a key first.";
    ks.className = "fhint bad";
    return;
  }
  try {
    await invoke("set_api_key", { key: input.value });
    input.value = "";
    await loadSettings();
    flashSaved("key saved to Keychain");
  } catch (err) {
    ks.textContent = `Couldn't save key: ${err}`;
    ks.className = "fhint bad";
  }
});

el("picklog").addEventListener("click", async () => {
  try {
    const picked = await invoke("pick_applied_log");
    if (picked) { await loadSettings(); flashSaved("log file set"); }
  } catch (err) { flashSaved(String(err), true); }
});
el("openlog").addEventListener("click", () => invoke("open_applied_log").catch((err) => flashSaved(String(err), true)));

// ---------- state sync ----------

function onState(next) {
  const wasStatus = lastStatus;
  state = next;
  lastStatus = state.status;
  render();
  if (state.status === "complete" && wasStatus === "scanning" && state.results.length && view === "pill") {
    setView("panel"); // fresh results: open up (but never yank away settings mid-edit)
  } else if (state.status === "scanning" && view === "panel") {
    setView("pill"); // a new scan folds the old results away
  } else if (view !== "pill") {
    refit(); // content changed under an open card
  }
}

setDims(PILL);
reportHitRect(PILL);
document.fonts.ready.then(() => moveTabIndicator(true));
LAYERS.pill.classList.add("on");
LAYERS.pill.style.opacity = 1;

listen("blip-state", (e) => onState(e.payload));
invoke("get_state").then(onState).catch(() => render());
