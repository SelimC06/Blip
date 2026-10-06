//! Weekly digest: everything Blip found in a window, as CSV — including roles
//! that never made a top 5, as a safety net.

use crate::store::ExportRow;
use anyhow::Result;
use std::path::Path;

fn field(s: &str) -> String {
    // Job titles come from third parties; a leading = + - @ would make Excel
    // run the cell as a formula. A leading apostrophe keeps it as text.
    let s = if s.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        format!("'{s}")
    } else {
        s.to_string()
    };
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s
    }
}

pub fn to_csv(rows: &[ExportRow]) -> String {
    let mut out =
        String::from("first_seen,status,match,company,role,location,posted,deadline,link\n");
    for r in rows {
        let score = r.score.map(|s| s.to_string()).unwrap_or_default();
        let cols = [
            &r.first_seen, &r.status, &score, &r.company, &r.title,
            &r.location, &r.posted, &r.deadline, &r.url,
        ];
        out.push_str(&cols.iter().map(|c| field(c)).collect::<Vec<_>>().join(","));
        out.push('\n');
    }
    out
}

pub fn write_csv(path: &Path, rows: &[ExportRow]) -> Result<()> {
    std::fs::write(path, to_csv(rows))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_fields_with_commas_and_quotes() {
        let rows = vec![ExportRow {
            first_seen: "2026-10-06 12:00:00".into(),
            status: "surfaced".into(),
            score: Some(92),
            company: "Acme, Inc.".into(),
            title: "Intern \"Platform\"".into(),
            location: "NYC".into(),
            posted: "0d".into(),
            deadline: String::new(),
            url: "https://x.test/1".into(),
        }];
        let csv = to_csv(&rows);
        assert!(csv.contains("\"Acme, Inc.\""));
        assert!(csv.contains("\"Intern \"\"Platform\"\"\""));
        assert!(csv.contains(",92,"));
    }

    #[test]
    fn neutralizes_formula_cells() {
        assert_eq!(field("=HYPERLINK(\"x\")"), "\"'=HYPERLINK(\"\"x\"\")\"");
        assert_eq!(field("+1 555"), "'+1 555");
        assert_eq!(field("@SUM(A1)"), "'@SUM(A1)");
        assert_eq!(field("Software Intern"), "Software Intern");
    }
}
