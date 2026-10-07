use anyhow::Result;
use blip_core::store::{default_db_path, Store};
use blip_core::{config, llm::Llm, profile, score};

const USAGE: &str = "usage:
  blip scan [--top N] [--limit N] [--db PATH]   run a cycle; --top scores and ranks
  blip profile --resume PATH                    set resume and (re)build the profile
  blip find <company name or careers link>      look up a company's job board
  blip rescore                                  re-judge roles already shown under the current scoring (saves nothing)";

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("find") {
        return cmd_find(&args[1..].join(" "));
    }
    if args.first().map(String::as_str) == Some("rescore") {
        return cmd_rescore();
    }
    let mut cmd = "scan";
    let mut db_path = default_db_path();
    let mut limit: usize = 25;
    let mut top: Option<usize> = None;
    let mut resume: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "scan" | "profile" => cmd = Box::leak(args[i].clone().into_boxed_str()),
            "--db" if i + 1 < args.len() => { db_path = args[i + 1].clone().into(); i += 1; }
            "--limit" if i + 1 < args.len() => { limit = args[i + 1].parse().unwrap_or(25); i += 1; }
            "--top" if i + 1 < args.len() => { top = Some(args[i + 1].parse().unwrap_or(5)); i += 1; }
            "--resume" if i + 1 < args.len() => { resume = Some(args[i + 1].clone()); i += 1; }
            other => { eprintln!("unknown arg: {other}\n{USAGE}"); std::process::exit(2); }
        }
        i += 1;
    }

    match cmd {
        "profile" => cmd_profile(resume),
        _ => cmd_scan(db_path, limit, top),
    }
}

/// Compare current scoring against the scores roles were shown with.
/// Prints one line per role as it goes, then a summary. Saves nothing.
fn cmd_rescore() -> Result<()> {
    let cfg = config::load_or_create()?;
    let store = Store::open(&default_db_path())?;
    let llm = Llm::new(&cfg)?;
    let prof = profile::load_or_build(&llm, &cfg)?;
    let system = score::scoring_prompt(&cfg);
    let profile_str = serde_json::to_string(&prof.data)?;
    let rows = store.shown_with_scores()?;
    let (mut filtered, mut still, mut now_hidden, mut failed) = (0, 0, 0, 0);
    println!("re-judging {} roles already shown (bar: {}+)\n", rows.len(), cfg.min_score);
    for (p, old) in rows {
        let title = format!("{} — {}", p.company, p.title);
        if !blip_core::fields::in_targets(&p.title, &cfg.target_fields) {
            filtered += 1;
            println!("FIELD-FILTERED  {old:>3} → --   {title}");
            continue;
        }
        match score::score_one(&llm, &system, &profile_str, &cfg, p) {
            Ok(s) => {
                let shown = s.score >= cfg.min_score;
                if shown { still += 1 } else { now_hidden += 1 }
                println!("{}  {old:>3} → {:>3}  {title}\n                          {}", if shown { "shown         " } else { "now hidden    " }, s.score, s.reason);
            }
            Err(e) => {
                failed += 1;
                println!("ERROR           {old:>3}        {title}: {e}");
            }
        }
    }
    println!("\nstill shown: {still} · now hidden by the rubric: {now_hidden} · dropped by the field filter: {filtered} · errors: {failed}");
    Ok(())
}

fn cmd_find(query: &str) -> Result<()> {
    let client = blip_core::http_client()?;
    let found = blip_core::sources::find_company(&client, query)?;
    let e = &found.entry;
    println!("{} — {} board \"{}\" · {} early-career roles open", e.name, e.platform, e.board, found.roles);
    Ok(())
}

fn cmd_profile(resume: Option<String>) -> Result<()> {
    let mut cfg = config::load_or_create()?;
    if let Some(path) = resume {
        cfg.resume_path = std::fs::canonicalize(&path)?.to_string_lossy().into_owned();
        config::save(&cfg)?;
    }
    let llm = Llm::new(&cfg)?;
    let p = profile::build(&llm, &cfg)?;
    println!("profile built from {}", cfg.resume_path);
    println!("{}", serde_json::to_string_pretty(&p.data)?);
    println!("\nconfig: {}", config::config_path().display());
    Ok(())
}

fn cmd_scan(db_path: std::path::PathBuf, limit: usize, top: Option<usize>) -> Result<()> {
    let store = Store::open(&db_path)?;
    let cfg = config::load_or_create()?;
    println!("blip scan · db: {}", db_path.display());

    let report = blip_core::run_scan(&store, &cfg, &|| false)?;
    for err in &report.errors {
        eprintln!("  ⚠ {err}");
    }
    println!(
        "scanned {} postings · {} new · {} total remembered",
        report.scanned,
        report.new.len(),
        store.total_postings()?
    );

    let Some(top_n) = top else {
        // Phase 0 behavior: just list what's new.
        for p in report.new.iter().take(limit) {
            let posted = if p.posted.is_empty() { String::new() } else { format!(" · posted {}", p.posted) };
            println!("\n  {} — {}", p.company, p.title);
            println!("    {}{} [{}]", p.location, posted, p.source);
            println!("    {}", p.url);
        }
        if report.new.len() > limit {
            println!("\n  … and {} more (raise with --limit)", report.new.len() - limit);
        }
        return Ok(());
    };

    // Phase 1: score everything not yet surfaced, print the top N.
    let llm = Llm::new(&cfg)?;
    let prof = profile::load_or_build(&llm, &cfg)?;
    let candidates = store.unsurfaced(&score::score_key(&cfg, &prof))?;
    let ranked = score::rank(&llm, &cfg, &prof, &store, candidates, top_n, &|| false)?;

    if ranked.is_empty() {
        println!(
            "\nno matches scoring {}+ (filters: season {}, max age {} days)",
            cfg.min_score, cfg.season, cfg.max_age_days
        );
        return Ok(());
    }
    println!("\nTOP {} MATCHES", ranked.len());
    for s in &ranked {
        let p = &s.posting;
        let posted = if p.posted.is_empty() { String::new() } else { format!(" · posted {}", p.posted) };
        println!("\n  [{:>3}] {} — {}", s.score, p.company, p.title);
        println!("        {}{}", p.location, posted);
        println!("        {}", p.url);
        if !s.reason.is_empty() {
            println!("        ↳ {}", s.reason);
        }
        for flag in &s.red_flags {
            println!("        ⚑ {flag}");
        }
        if let Some(d) = &s.deadline {
            println!("        ⏳ applications close {d}");
        }
    }
    store.mark_surfaced(&ranked.iter().map(|s| s.posting.fingerprint()).collect::<Vec<_>>())?;
    Ok(())
}
