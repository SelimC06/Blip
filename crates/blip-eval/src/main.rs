//! Offline evaluation: how well did the former top-20 embedding prefilter
//! keep the postings that full LLM scoring would rank highest? Production
//! now scores everything that passes the hard filters (up to
//! `max_scored_per_scan`); `report`/`summary` still model the old top-20
//! shortlist with its 0.45 similarity floor, which is what they measured.
//!
//! The piece that can't be called directly — the shortlist's similarity
//! ordering, private inside `score::score_new` — is mirrored here, and
//! `verify` checks the mirror against production's own `score::rank`.
//!
//! Which preference profile is used comes from $HOME, exactly like Blip:
//! run each profile with HOME pointing at a folder holding its own
//! `Library/Application Support/Blip/{config.json, profile.json}`.
//!
//!   blip-eval snapshot <dir>          freeze one real scan (+ descriptions)
//!   blip-eval embed <dir>             prefilter embeddings for every posting
//!   blip-eval estimate <dir> [n]      time n scoring calls, twice (determinism)
//!   blip-eval baseline <dir> <name>   score every posting (resumable)
//!   blip-eval verify <dir> <name>     production rank() vs the mirrored prefilter
//!   blip-eval report <dir> <name>     metrics + raw CSV for one profile
//!   blip-eval summary <dir> <names…>  table across profiles
//!   blip-eval bench <dir> <name> [runs]  time production rank() end to end

use anyhow::{bail, Context, Result};
use blip_core::model::Posting;
use blip_core::store::Store;
use blip_core::{config, describe, llm::Llm, profile, score};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

/// The former production floor, kept to report on the old prefilter.
const SIMILARITY_FLOOR: f32 = 0.45;
const KS: [usize; 3] = [5, 10, 20];
const CURVE: [usize; 4] = [20, 50, 100, 200];

#[derive(Serialize, Deserialize, Clone)]
struct Snap {
    idx: usize,
    fingerprint: String,
    company: String,
    title: String,
    location: String,
    url: String,
    source: String,
    season: String,
    posted: String,
    deadline: String,
    /// Description as stored when the prefilter runs (only some sources
    /// provide one up front). Drives the prefilter text and hard filters.
    db_description: String,
    /// Description the LLM sees: the stored one, or the page production
    /// would fetch for a shortlisted posting. None until fetched.
    llm_description: Option<String>,
}

impl Snap {
    fn posting(&self, description: &str) -> Posting {
        Posting {
            company: self.company.clone(),
            title: self.title.clone(),
            location: self.location.clone(),
            url: self.url.clone(),
            source: self.source.clone(),
            season: self.season.clone(),
            posted: self.posted.clone(),
            description: description.to_string(),
            deadline: self.deadline.clone(),
        }
    }
    fn at_prefilter(&self) -> Posting {
        self.posting(&self.db_description)
    }
    fn at_scoring(&self) -> Posting {
        self.posting(self.llm_description.as_deref().unwrap_or(&self.db_description))
    }
}

#[derive(Serialize, Deserialize)]
struct Snapshot {
    created_at: String,
    postings: Vec<Snap>,
}

#[derive(Serialize, Deserialize, Clone)]
struct ScoreLine {
    fingerprint: String,
    score: Option<u8>,
    reason: String,
    error: String,
    seconds: f64,
}

fn load_snapshot(dir: &Path) -> Result<Snapshot> {
    let raw = fs::read_to_string(dir.join("snapshot.json")).context("no snapshot.json — run `snapshot` first")?;
    Ok(serde_json::from_str(&raw)?)
}

fn save_snapshot(dir: &Path, s: &Snapshot) -> Result<()> {
    let tmp = dir.join("snapshot.json.tmp");
    fs::write(&tmp, serde_json::to_string(s)?)?;
    fs::rename(tmp, dir.join("snapshot.json"))?;
    Ok(())
}

// ---------------------------------------------------------------- snapshot

fn cmd_snapshot(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir)?;
    let cfg = config::load_or_create()?;
    if !dir.join("snapshot.json").exists() {
        let db = dir.join("scan.db");
        if db.exists() {
            bail!("{} exists from a partial run; delete it to re-snapshot", db.display());
        }
        // A fresh store, so the whole scan comes back as new, deduplicated
        // exactly as production dedupes (fingerprint + job ID).
        let store = Store::open(&db)?;
        let started = Instant::now();
        let report = blip_core::run_scan(&store, &cfg, &|| false)?;
        eprintln!(
            "scan: {} postings fetched, {} after dedupe, {} source errors, {:.0}s",
            report.scanned,
            report.new.len(),
            report.errors.len(),
            started.elapsed().as_secs_f64()
        );
        let postings: Vec<Snap> = store
            .unsurfaced("snapshot")? // every row is new; rowid = production's input order
            .into_iter()
            .enumerate()
            .map(|(idx, (p, _))| Snap {
                idx,
                fingerprint: p.fingerprint(),
                llm_description: (!p.description.is_empty()).then(|| p.description.clone()),
                db_description: p.description.clone(),
                company: p.company,
                title: p.title,
                location: p.location,
                url: p.url,
                source: p.source,
                season: p.season,
                posted: p.posted,
                deadline: p.deadline,
            })
            .collect();
        save_snapshot(dir, &Snapshot { created_at: chrono_now(), postings })?;
    }
    // Fetch the page production would fetch for each posting, in batches,
    // saving after every batch so an interrupted fetch resumes.
    let mut snap = load_snapshot(dir)?;
    let pending: Vec<usize> = snap.postings.iter().filter(|s| s.llm_description.is_none()).map(|s| s.idx).collect();
    eprintln!("{} postings in snapshot, {} descriptions to fetch", snap.postings.len(), pending.len());
    let started = Instant::now();
    for (n, batch) in pending.chunks(32).enumerate() {
        let urls: Vec<(usize, String)> = batch.iter().map(|&i| (i, snap.postings[i].url.clone())).collect();
        let got: HashMap<usize, String> = describe::fetch_missing(&urls, &|| false).into_iter().collect();
        for &i in batch {
            snap.postings[i].llm_description = Some(got.get(&i).cloned().unwrap_or_default());
        }
        save_snapshot(dir, &snap)?;
        if n % 10 == 0 {
            eprintln!("  fetched {}/{} ({:.0}s)", (n + 1) * 32, pending.len(), started.elapsed().as_secs_f64());
        }
    }
    let with = snap.postings.iter().filter(|s| s.llm_description.as_deref().is_some_and(|d| !d.is_empty())).count();
    println!("snapshot ready: {} postings, {} with a description for scoring", snap.postings.len(), with);
    Ok(())
}

fn chrono_now() -> String {
    // Avoids a chrono dependency here; seconds since epoch is enough to date it.
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    format!("unix:{secs}")
}

// ------------------------------------------------------------------- embed

/// Production's prefilter text for a posting (see `score_new`).
fn prefilter_text(p: &Posting) -> String {
    let lead: String = p.description.chars().take(300).collect();
    format!("{} at {} — {}. {lead}", p.title, p.company, p.location)
}

fn cmd_embed(dir: &Path) -> Result<()> {
    let snap = load_snapshot(dir)?;
    let cfg = config::load_or_create()?;
    let llm = Llm::new(&cfg)?;
    let texts: Vec<String> = snap.postings.iter().map(|s| prefilter_text(&s.at_prefilter())).collect();
    let started = Instant::now();
    let embs = llm.embed(&texts)?;
    fs::write(dir.join("embeddings.json"), serde_json::to_string(&embs)?)?;
    println!("embedded {} postings with {} in {:.0}s", embs.len(), cfg.embed_model, started.elapsed().as_secs_f64());
    Ok(())
}

fn load_embeddings(dir: &Path) -> Result<Vec<Vec<f32>>> {
    Ok(serde_json::from_str(&fs::read_to_string(dir.join("embeddings.json")).context("run `embed` first")?)?)
}

/// Mirrors `score::cosine` (private) exactly, including operation order.
fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

// ---------------------------------------------------------------- scoring

struct Scorer {
    llm: Llm,
    cfg: config::Config,
    system: String,
    profile_str: String,
}

impl Scorer {
    fn new() -> Result<(Self, profile::Profile)> {
        let cfg = config::load_or_create()?;
        let llm = Llm::new(&cfg)?;
        let prof = profile::load_or_build(&llm, &cfg)?;
        let system = score::scoring_prompt(&cfg);
        let profile_str = serde_json::to_string(&prof.data)?;
        Ok((Scorer { llm, cfg, system, profile_str }, prof))
    }
    fn score(&self, s: &Snap) -> (Result<score::Scored>, f64) {
        let t = Instant::now();
        let r = score::score_one(&self.llm, &self.system, &self.profile_str, &self.cfg, s.at_scoring());
        (r, t.elapsed().as_secs_f64())
    }
}

fn cmd_estimate(dir: &Path, n: usize) -> Result<()> {
    let snap = load_snapshot(dir)?;
    let (scorer, _) = Scorer::new()?;
    // An even spread across the snapshot, not just its first rows.
    let step = (snap.postings.len() / n.max(1)).max(1);
    let sample: Vec<&Snap> = snap.postings.iter().step_by(step).take(n).collect();
    let mut first = Vec::new();
    let mut total = 0.0;
    for s in &sample {
        let (r, secs) = scorer.score(s);
        total += secs;
        first.push(r.map(|x| x.score).ok());
    }
    let per_call = total / sample.len() as f64;
    let mut same = 0;
    for (s, a) in sample.iter().zip(&first) {
        let (r, _) = scorer.score(s);
        if r.map(|x| x.score).ok() == *a {
            same += 1;
        }
    }
    let all = snap.postings.len();
    println!(
        "{:.2}s per scoring call over {} postings → about {:.1} hours for all {} postings in this snapshot",
        per_call,
        sample.len(),
        per_call * all as f64 / 3600.0,
        all
    );
    println!("determinism: {same}/{} identical scores when re-scored (temperature 0, no seed — as production)", sample.len());
    Ok(())
}

fn scores_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("scores-{name}.jsonl"))
}

fn load_scores(dir: &Path, name: &str) -> Result<HashMap<String, ScoreLine>> {
    let mut out = HashMap::new();
    if let Ok(f) = fs::File::open(scores_path(dir, name)) {
        for line in std::io::BufReader::new(f).lines() {
            if let Ok(l) = serde_json::from_str::<ScoreLine>(&line?) {
                out.insert(l.fingerprint.clone(), l); // later lines (retries) win
            }
        }
    }
    Ok(out)
}

fn cmd_baseline(dir: &Path, name: &str) -> Result<()> {
    let snap = load_snapshot(dir)?;
    if snap.postings.iter().any(|s| s.llm_description.is_none()) {
        bail!("snapshot descriptions not fully fetched — finish `snapshot` first");
    }
    let (scorer, _) = Scorer::new()?;
    let done = load_scores(dir, name)?;
    let todo: Vec<&Snap> = snap
        .postings
        .iter()
        .filter(|s| done.get(&s.fingerprint).map_or(true, |l| l.score.is_none()))
        .collect();
    eprintln!("[{name}] {} scored already, {} to go (model {})", snap.postings.len() - todo.len(), todo.len(), scorer.cfg.chat_model);
    let mut out = fs::OpenOptions::new().create(true).append(true).open(scores_path(dir, name))?;
    let started = Instant::now();
    for (n, s) in todo.iter().enumerate() {
        let (r, secs) = scorer.score(s);
        let line = match r {
            Ok(sc) => ScoreLine { fingerprint: s.fingerprint.clone(), score: Some(sc.score), reason: sc.reason, error: String::new(), seconds: secs },
            Err(e) => ScoreLine { fingerprint: s.fingerprint.clone(), score: None, reason: String::new(), error: format!("{e:#}"), seconds: secs },
        };
        writeln!(out, "{}", serde_json::to_string(&line)?)?;
        out.flush()?;
        if (n + 1) % 50 == 0 || n + 1 == todo.len() {
            let rate = started.elapsed().as_secs_f64() / (n + 1) as f64;
            eprintln!("[{name}] {}/{}  {:.2}s/call  ~{:.0} min left", n + 1, todo.len(), rate, rate * (todo.len() - n - 1) as f64 / 60.0);
        }
    }
    let after = load_scores(dir, name)?;
    let failed = after.values().filter(|l| l.score.is_none()).count();
    println!("[{name}] baseline done: {} scored, {failed} failed (re-run to retry failures)", after.len() - failed);
    Ok(())
}

// ------------------------------------------------------------------ verify

/// Mirror of the production prefilter over a candidate set, in production's
/// input order: floor, then a stable sort by similarity (ties keep input
/// order, i.e. snapshot index).
fn prefilter(candidates: &[usize], cos: &[f32], keep: usize, floor: bool) -> Vec<usize> {
    let mut v: Vec<usize> = candidates.iter().copied().filter(|&i| !floor || cos[i] >= SIMILARITY_FLOOR).collect();
    v.sort_by(|&a, &b| cos[b].partial_cmp(&cos[a]).unwrap_or(std::cmp::Ordering::Equal).then(a.cmp(&b)));
    v.truncate(keep);
    v
}

fn cmd_verify(dir: &Path, name: &str) -> Result<()> {
    let snap = load_snapshot(dir)?;
    let embs = load_embeddings(dir)?;
    let (scorer, prof) = Scorer::new()?;
    let cfg = &scorer.cfg;
    let authz = score::effective_authorization(cfg, &prof);
    let cos: Vec<f32> = embs.iter().map(|e| cosine(e, &prof.embedding)).collect();
    let eligible: Vec<usize> = snap.postings.iter().filter(|s| score::hard_filter(cfg, &s.at_prefilter(), &authz)).map(|s| s.idx).collect();
    let mirror: HashSet<String> = prefilter(&eligible, &cos, cfg.max_scored_per_scan, false)
        .into_iter()
        .map(|i| snap.postings[i].fingerprint.clone())
        .collect();

    // Production: insert the frozen postings, in order, into a fresh store
    // and let score::rank do the whole pipeline (prefilter → fetch → score).
    let db = dir.join(format!("verify-{name}.db"));
    let _ = fs::remove_file(&db);
    let store = Store::open(&db)?;
    for s in &snap.postings {
        store.insert_if_new(&s.at_prefilter())?;
    }
    let key = score::score_key(cfg, &prof);
    let candidates = store.unsurfaced(&key)?;
    score::rank(&scorer.llm, cfg, &prof, &store, candidates, cfg.results_per_scan, &|| false)?;
    let production: HashSet<String> = store
        .unsurfaced(&key)?
        .into_iter()
        .filter(|(_, cached)| cached.is_some())
        .map(|(p, _)| p.fingerprint())
        .collect();
    // Production drops work-authorization blocks after fetching; compare
    // against the mirror's shortlist with the same drop applied.
    let mirror_after_auth: HashSet<String> = mirror
        .iter()
        .filter(|fp| {
            let s = snap.postings.iter().find(|s| &&s.fingerprint == fp).unwrap();
            !blip_core::auth::blocks(s.at_scoring().auth_requirement(), &authz)
        })
        .cloned()
        .collect();
    let agree = production.intersection(&mirror_after_auth).count();
    println!(
        "[{name}] production scored {} postings; mirrored prefilter shortlisted {} ({} after the work-authorization check); overlap {agree}",
        production.len(),
        mirror.len(),
        mirror_after_auth.len()
    );
    if production == mirror_after_auth {
        println!("[{name}] MATCH: the mirrored prefilter selects exactly what production does");
    } else {
        println!("[{name}] MISMATCH — only in production: {:?}", production.difference(&mirror_after_auth).collect::<Vec<_>>());
        println!("[{name}]            only in mirror: {:?}", mirror_after_auth.difference(&production).collect::<Vec<_>>());
    }
    let _ = fs::remove_file(&db);
    Ok(())
}

// ------------------------------------------------------------------ report

#[derive(Serialize, Deserialize)]
struct Variant {
    label: String,
    candidates: usize,
    above_floor: usize,
    scored: usize,
    recall: Vec<(usize, f64)>,
    tie_at_k: Vec<(usize, u8, usize)>, // (k, score at rank k, how many share it)
    top1_title: String,
    top1_score: u8,
    top1_prefilter_rank: usize, // 1-based, among candidates by similarity
    top1_above_floor: bool,
    curve: Vec<(usize, usize, f64)>, // (K kept, how many actually kept, top-20 recall)
}

#[derive(Serialize, Deserialize)]
struct Report {
    name: String,
    looking_for: String,
    targets: Vec<String>,
    snapshot_postings: usize,
    variants: Vec<Variant>,
}

fn llm_order(cands: &[usize], snap: &Snapshot, scores: &HashMap<String, ScoreLine>) -> Vec<usize> {
    // Production's final order: score desc, then freshness; then snapshot
    // index so ties are deterministic.
    let mut v: Vec<usize> = cands.iter().copied().filter(|&i| scores.get(&snap.postings[i].fingerprint).and_then(|l| l.score).is_some()).collect();
    let sc = |i: usize| scores[&snap.postings[i].fingerprint].score.unwrap();
    let age = |i: usize| score::age_days(&snap.postings[i].posted).unwrap_or(f64::MAX);
    v.sort_by(|&a, &b| {
        sc(b).cmp(&sc(a))
            .then(age(a).partial_cmp(&age(b)).unwrap_or(std::cmp::Ordering::Equal))
            .then(a.cmp(&b))
    });
    v
}

fn variant(label: &str, cands: &[usize], snap: &Snapshot, cos: &[f32], scores: &HashMap<String, ScoreLine>) -> Variant {
    let llm = llm_order(cands, snap, scores);
    let sc = |i: usize| scores[&snap.postings[i].fingerprint].score.unwrap();
    let pre20: HashSet<usize> = prefilter(cands, cos, 20, true).into_iter().collect();
    let recall = KS
        .iter()
        .map(|&k| {
            let top: Vec<usize> = llm.iter().take(k).copied().collect();
            (k, top.iter().filter(|i| pre20.contains(i)).count() as f64 / top.len().max(1) as f64)
        })
        .collect();
    let tie_at_k = KS
        .iter()
        .filter(|&&k| llm.len() >= k)
        .map(|&k| {
            let s = sc(llm[k - 1]);
            (k, s, llm.iter().filter(|&&i| sc(i) == s).count())
        })
        .collect();
    let by_sim = prefilter(cands, cos, usize::MAX, false);
    let top1 = llm[0];
    let curve = CURVE
        .iter()
        .map(|&k| {
            let kept: HashSet<usize> = prefilter(cands, cos, k, true).into_iter().collect();
            let top20: Vec<usize> = llm.iter().take(20).copied().collect();
            (k, kept.len(), top20.iter().filter(|i| kept.contains(i)).count() as f64 / top20.len().max(1) as f64)
        })
        .collect();
    Variant {
        label: label.into(),
        candidates: cands.len(),
        above_floor: cands.iter().filter(|&&i| cos[i] >= SIMILARITY_FLOOR).count(),
        scored: llm.len(),
        recall,
        tie_at_k,
        top1_title: format!("{} — {}", snap.postings[top1].company, snap.postings[top1].title),
        top1_score: sc(top1),
        top1_prefilter_rank: by_sim.iter().position(|&i| i == top1).map(|p| p + 1).unwrap_or(0),
        top1_above_floor: cos[top1] >= SIMILARITY_FLOOR,
        curve,
    }
}

fn csv(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s.to_string() }
}

fn cmd_report(dir: &Path, name: &str) -> Result<()> {
    let snap = load_snapshot(dir)?;
    let embs = load_embeddings(dir)?;
    let scores = load_scores(dir, name)?;
    let (scorer, prof) = Scorer::new()?;
    let cfg = &scorer.cfg;
    let authz = score::effective_authorization(cfg, &prof);
    let cos: Vec<f32> = embs.iter().map(|e| cosine(e, &prof.embedding)).collect();
    let all: Vec<usize> = (0..snap.postings.len()).collect();
    let eligible: Vec<usize> = snap.postings.iter().filter(|s| score::hard_filter(cfg, &s.at_prefilter(), &authz)).map(|s| s.idx).collect();
    let missing = all.iter().filter(|&&i| scores.get(&snap.postings[i].fingerprint).and_then(|l| l.score).is_none()).count();
    if missing > 0 {
        eprintln!("[{name}] warning: {missing} postings have no score (failed or not yet scored); they are left out of rankings");
    }
    let report = Report {
        name: name.into(),
        looking_for: cfg.looking_for.clone(),
        targets: cfg.target_fields.clone(),
        snapshot_postings: snap.postings.len(),
        variants: vec![
            variant("as production (hard filters first)", &eligible, &snap, &cos, &scores),
            variant("embedding only (all postings)", &all, &snap, &cos, &scores),
        ],
    };
    fs::write(dir.join(format!("report-{name}.json")), serde_json::to_string_pretty(&report)?)?;

    // Raw per-posting CSV.
    let prod_llm = llm_order(&eligible, &snap, &scores);
    let all_llm = llm_order(&all, &snap, &scores);
    let prod_pre = prefilter(&eligible, &cos, usize::MAX, false);
    let all_pre = prefilter(&all, &cos, usize::MAX, false);
    let rank_of = |v: &[usize]| -> HashMap<usize, usize> { v.iter().enumerate().map(|(r, &i)| (i, r + 1)).collect() };
    let (pl, al, pp, ap) = (rank_of(&prod_llm), rank_of(&all_llm), rank_of(&prod_pre), rank_of(&all_pre));
    let pre20_prod: HashSet<usize> = prefilter(&eligible, &cos, 20, true).into_iter().collect();
    let pre20_all: HashSet<usize> = prefilter(&all, &cos, 20, true).into_iter().collect();
    let eligible_set: HashSet<usize> = eligible.iter().copied().collect();
    let mut out = String::from("idx,fingerprint,company,title,location,source,passes_hard_filters,cosine,above_floor,llm_score,llm_rank_production,llm_rank_all,similarity_rank_production,similarity_rank_all,in_prefilter20_production,in_prefilter20_all,has_scoring_description,reason,error\n");
    for s in &snap.postings {
        let i = s.idx;
        let l = scores.get(&s.fingerprint);
        let opt = |m: &HashMap<usize, usize>| m.get(&i).map(|r| r.to_string()).unwrap_or_default();
        out.push_str(&format!(
            "{},{},{},{},{},{},{},{:.5},{},{},{},{},{},{},{},{},{},{},{}\n",
            i,
            csv(&s.fingerprint),
            csv(&s.company),
            csv(&s.title),
            csv(&s.location),
            csv(&s.source),
            eligible_set.contains(&i),
            cos[i],
            cos[i] >= SIMILARITY_FLOOR,
            l.and_then(|l| l.score).map(|x| x.to_string()).unwrap_or_default(),
            opt(&pl),
            opt(&al),
            opt(&pp),
            opt(&ap),
            pre20_prod.contains(&i),
            pre20_all.contains(&i),
            s.llm_description.as_deref().is_some_and(|d| !d.is_empty()),
            csv(&l.map(|l| l.reason.clone()).unwrap_or_default()),
            csv(&l.map(|l| l.error.clone()).unwrap_or_default()),
        ));
    }
    fs::write(dir.join(format!("raw-{name}.csv")), out)?;
    print_report(&report);
    Ok(())
}

fn print_report(r: &Report) {
    println!("\n[{}] looking for {:?}, fields {:?}, {} postings", r.name, r.looking_for, r.targets, r.snapshot_postings);
    for v in &r.variants {
        println!("  {} — {} candidates, {} above floor, {} scored", v.label, v.candidates, v.above_floor, v.scored);
        let rec: Vec<String> = v.recall.iter().map(|(k, x)| format!("R@{k}={:.0}%", x * 100.0)).collect();
        println!("    {}", rec.join("  "));
        let ties: Vec<String> = v.tie_at_k.iter().map(|(k, s, n)| format!("#{k} scores {s} ({n} postings share it)")).collect();
        println!("    ties: {}", ties.join("; "));
        println!(
            "    LLM #1: {} ({}) — similarity rank {}{}",
            v.top1_title,
            v.top1_score,
            v.top1_prefilter_rank,
            if v.top1_above_floor { "" } else { ", below the 0.45 floor" }
        );
        let c: Vec<String> = v.curve.iter().map(|(k, kept, x)| format!("K={k} (kept {kept}): {:.0}%", x * 100.0)).collect();
        println!("    top-20 recall by K: {}", c.join("  "));
    }
}

fn cmd_summary(dir: &Path, names: &[String]) -> Result<()> {
    let mut reports = Vec::new();
    for n in names {
        let raw = fs::read_to_string(dir.join(format!("report-{n}.json"))).with_context(|| format!("no report for {n}"))?;
        reports.push(serde_json::from_str::<Report>(&raw)?);
    }
    let mut out = String::from("profile,variant,candidates,above_floor,recall_at_5,recall_at_10,recall_at_20,llm_top1_similarity_rank,llm_top1_above_floor,top20_recall_K20,top20_recall_K50,top20_recall_K100,top20_recall_K200\n");
    for vi in 0..2 {
        let label = &reports[0].variants[vi].label;
        println!("\n{label}");
        println!("  {:<12} {:>5} {:>6} {:>6} {:>6} {:>9} {:>6} {:>6} {:>6} {:>6}", "profile", "cands", "R@5", "R@10", "R@20", "#1 rank", "K=20", "K=50", "K=100", "K=200");
        let mut sums = [0.0f64; 7];
        for r in &reports {
            let v = &r.variants[vi];
            let rc: Vec<f64> = v.recall.iter().map(|x| x.1).collect();
            let cv: Vec<f64> = v.curve.iter().map(|x| x.2).collect();
            println!(
                "  {:<12} {:>5} {:>5.0}% {:>5.0}% {:>5.0}% {:>9} {:>5.0}% {:>5.0}% {:>5.0}% {:>5.0}%",
                r.name, v.candidates, rc[0] * 100.0, rc[1] * 100.0, rc[2] * 100.0,
                format!("{}{}", v.top1_prefilter_rank, if v.top1_above_floor { "" } else { "*" }),
                cv[0] * 100.0, cv[1] * 100.0, cv[2] * 100.0, cv[3] * 100.0
            );
            for (j, x) in rc.iter().chain(cv.iter()).enumerate() {
                sums[j] += x;
            }
            out.push_str(&format!(
                "{},{},{},{},{:.4},{:.4},{:.4},{},{},{:.4},{:.4},{:.4},{:.4}\n",
                r.name, csv(&v.label), v.candidates, v.above_floor, rc[0], rc[1], rc[2], v.top1_prefilter_rank, v.top1_above_floor, cv[0], cv[1], cv[2], cv[3]
            ));
        }
        let n = reports.len() as f64;
        println!(
            "  {:<12} {:>5} {:>5.0}% {:>5.0}% {:>5.0}% {:>9} {:>5.0}% {:>5.0}% {:>5.0}% {:>5.0}%",
            "average", "", sums[0] / n * 100.0, sums[1] / n * 100.0, sums[2] / n * 100.0, "",
            sums[3] / n * 100.0, sums[4] / n * 100.0, sums[5] / n * 100.0, sums[6] / n * 100.0
        );
    }
    println!("\n  * = the LLM's #1 posting falls below the 0.45 similarity floor, so production could never shortlist it");
    fs::write(dir.join("summary.csv"), out)?;
    Ok(())
}

// ------------------------------------------------------------------- bench

/// Time production's `score::rank` on the frozen snapshot, from a fresh
/// store each run (so nothing is cached): hard filters, description fetch,
/// and LLM scoring. Appends one JSON line per run to bench-<name>.jsonl.
fn cmd_bench(dir: &Path, name: &str, runs: usize) -> Result<()> {
    let snap = load_snapshot(dir)?;
    let (scorer, prof) = Scorer::new()?;
    let cfg = scorer.cfg;
    let key = score::score_key(&cfg, &prof);
    let mut out = fs::OpenOptions::new().create(true).append(true).open(dir.join(format!("bench-{name}.jsonl")))?;
    let mut times = Vec::new();
    for run in 1..=runs {
        let db = dir.join(format!("bench-{name}.db"));
        let _ = fs::remove_file(&db);
        let store = Store::open(&db)?;
        for s in &snap.postings {
            store.insert_if_new(&s.at_prefilter())?;
        }
        let llm = Llm::new(&cfg)?;
        let candidates = store.unsurfaced(&key)?;
        let started = Instant::now();
        let shown = score::rank(&llm, &cfg, &prof, &store, candidates, cfg.results_per_scan, &|| false)?;
        let secs = started.elapsed().as_secs_f64();
        let (chat, embed) = llm.calls();
        let scored = store.unsurfaced(&key)?.iter().filter(|(_, c)| c.is_some()).count();
        println!("[{name}] run {run}: {secs:.1}s, {chat} chat calls, {embed} embedding requests, {scored} scored, {} shown", shown.len());
        writeln!(out, "{}", serde_json::json!({"run": run, "seconds": secs, "chat_calls": chat, "embed_calls": embed, "scored": scored, "shown": shown.len()}))?;
        times.push(secs);
        let _ = fs::remove_file(&db);
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!("[{name}] median {:.1}s over {runs} runs", times[times.len() / 2]);
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dir = |i: usize| -> Result<PathBuf> { args.get(i).map(PathBuf::from).context("missing <dir>") };
    let name = |i: usize| -> Result<String> { args.get(i).cloned().context("missing <name>") };
    match args.first().map(String::as_str) {
        Some("snapshot") => cmd_snapshot(&dir(1)?),
        Some("embed") => cmd_embed(&dir(1)?),
        Some("estimate") => cmd_estimate(&dir(1)?, args.get(2).and_then(|n| n.parse().ok()).unwrap_or(20)),
        Some("baseline") => cmd_baseline(&dir(1)?, &name(2)?),
        Some("verify") => cmd_verify(&dir(1)?, &name(2)?),
        Some("report") => cmd_report(&dir(1)?, &name(2)?),
        Some("summary") => cmd_summary(&dir(1)?, &args[2..]),
        Some("bench") => cmd_bench(&dir(1)?, &name(2)?, args.get(3).and_then(|n| n.parse().ok()).unwrap_or(3)),
        _ => bail!("usage: blip-eval snapshot|embed|estimate|baseline|verify|report|summary|bench <dir> [name…]"),
    }
}
