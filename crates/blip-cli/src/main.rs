use anyhow::Result;
use blip_core::store::{default_db_path, Store};
use blip_core::{config, llm::Llm, profile, score};

const USAGE: &str = "usage:
  blip scan [--top N] [--limit N] [--db PATH]   run a cycle; --top scores and ranks
  blip profile --resume PATH                    set resume and (re)build the profile";

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
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

    let report = blip_core::run_scan(&store, &|| false)?;
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
