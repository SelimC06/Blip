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
  return { w: layer.offsetWidth + 2, h: Math.min(layer.offsetHeight, 470) + 2, r: CARD_RADIUS };
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
}

// When the open card's content changes size (a row dismissed), glide to fit.
function refit() {
  if (view === "pill") return;
  const to = targetDims(view);
  reportHitRect(to);
  morph(to, SPRING_CLOSE);
}

// ---------- rendering ----------

function render() {
  surface.dataset.status = state.status;
  el("pilltext").textContent = LABELS[state.status] || state.status;
  el("cycletxt").textContent = `every ${state.cycle_minutes} min`;
  el("pausebtn").innerHTML = state.status === "paused" ? `${ICONS.play}resume` : `${ICONS.pause}pause`;

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
    title.onclick = () => {
      invoke("open_link", { url: j.url }).catch(() => {});
      invoke("job_action", { fingerprint: j.fingerprint, action: "viewed" }).catch(() => {});
    };
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
    yes.onclick = () => {
      invoke("job_action", { fingerprint: j.fingerprint, action: "applied" }).catch(() => {});
      row.classList.add("done");
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
  invoke("job_action", { fingerprint: j.fingerprint, action: "dismissed" }).catch(() => {});
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

function renderStats() {
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
el("gear").addEventListener("click", (e) => { e.stopPropagation(); setView("settings"); });
el("setclose").addEventListener("click", () => setView("pill"));
el("minbtn").addEventListener("click", () => setView("pill"));
el("scannow").addEventListener("click", () => { invoke("scan_now").catch(() => {}); setView("pill"); });
el("pausebtn").addEventListener("click", () => invoke("toggle_pause").catch(() => {}));
el("quitbtn").addEventListener("click", () => invoke("quit_app").catch(() => {}));
el("openall").addEventListener("click", () => {
  for (const j of state.results) invoke("open_link", { url: j.url }).catch(() => {});
});

// ---------- state sync ----------

function onState(next) {
  const wasStatus = lastStatus;
  state = next;
  lastStatus = state.status;
  render();
  if (state.status === "complete" && wasStatus === "scanning" && state.results.length) {
    setView("panel"); // fresh results: open up
  } else if (state.status === "scanning" && view === "panel") {
    setView("pill"); // a new scan folds the old results away
  } else if (view !== "pill") {
    refit(); // content changed under an open card
  }
}

setDims(PILL);
reportHitRect(PILL);
LAYERS.pill.classList.add("on");
LAYERS.pill.style.opacity = 1;

listen("blip-state", (e) => onState(e.payload));
invoke("get_state").then(onState).catch(() => render());
