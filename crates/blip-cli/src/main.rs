use anyhow::Result;
use blip_core::store::{default_db_path, Store};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut db_path = default_db_path();
    let mut limit: usize = 25;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--db" if i + 1 < args.len() => {
                db_path = args[i + 1].clone().into();
                i += 1;
            }
            "--limit" if i + 1 < args.len() => {
                limit = args[i + 1].parse().unwrap_or(25);
                i += 1;
            }
            "scan" => {}
            other => {
                eprintln!("unknown arg: {other}\nusage: blip [scan] [--db PATH] [--limit N]");
                std::process::exit(2);
            }
        }
        i += 1;
    }

    let store = Store::open(&db_path)?;
    println!("blip scan · db: {}", db_path.display());

    let report = blip_core::run_scan(&store)?;

    for err in &report.errors {
        eprintln!("  ⚠ {err}");
    }
    println!(
        "scanned {} postings · {} new · {} total remembered",
        report.scanned,
        report.new.len(),
        store.total_postings()?
    );

    for p in report.new.iter().take(limit) {
        let posted = if p.posted.is_empty() {
            String::new()
        } else {
            format!(" · posted {}", p.posted)
        };
        println!("\n  {} — {}", p.company, p.title);
        println!("    {}{} [{}]", p.location, posted, p.source);
        println!("    {}", p.url);
    }
    if report.new.len() > limit {
        println!("\n  … and {} more (raise with --limit)", report.new.len() - limit);
    }
    Ok(())
}
