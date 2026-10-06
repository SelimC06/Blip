//! ✓ Applied → one new row at the bottom of the user's spreadsheet. The
//! file is read and re-saved so rows, columns and edits the user added
//! survive; SQLite stays the source of truth, this is a write-only mirror.

use anyhow::{bail, Context, Result};
use std::path::Path;

pub struct AppliedRow {
    pub date: String,
    pub company: String,
    pub role: String,
    pub location: String,
    pub score: u8,
    pub posted: String,
    pub url: String,
}

const HEADERS: [&str; 7] = ["Date applied", "Company", "Role", "Location", "Match", "Posted", "Link"];

/// Excel keeps a `~$Name.xlsx` owner file next to any workbook it has open.
/// On a Mac it doesn't lock the workbook itself, so writing would succeed and
/// then be overwritten when the user saves in Excel. Refuse instead.
pub fn open_in_excel(path: &Path) -> bool {
    match (path.parent(), path.file_name()) {
        (Some(dir), Some(name)) => dir.join(format!("~${}", name.to_string_lossy())).exists(),
        _ => false,
    }
}

pub fn append(path: &Path, row: &AppliedRow) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if open_in_excel(path) {
        bail!(
            "{} is open in Excel. Close it, then press ✓ again.",
            path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
        );
    }
    let is_new = !path.exists();
    let mut book = if is_new {
        umya_spreadsheet::new_file()
    } else {
        umya_spreadsheet::reader::xlsx::read(path)
            .with_context(|| format!("reading {}", path.display()))?
    };
    let sheet = book
        .get_sheet_mut(&0)
        .context("spreadsheet has no worksheets")?;

    let next_row = if is_new {
        sheet.set_name("Applied");
        for (i, h) in HEADERS.iter().enumerate() {
            let cell = sheet.get_cell_mut((i as u32 + 1, 1));
            cell.set_value(*h);
            cell.get_style_mut().get_font_mut().set_bold(true);
        }
        sheet.get_column_dimension_mut("A").set_width(14.0);
        sheet.get_column_dimension_mut("B").set_width(22.0);
        sheet.get_column_dimension_mut("C").set_width(44.0);
        sheet.get_column_dimension_mut("D").set_width(24.0);
        sheet.get_column_dimension_mut("E").set_width(8.0);
        sheet.get_column_dimension_mut("F").set_width(12.0);
        sheet.get_column_dimension_mut("G").set_width(60.0);
        2
    } else {
        sheet.get_highest_row() + 1
    };

    let values = [
        row.date.as_str(),
        row.company.as_str(),
        row.role.as_str(),
        row.location.as_str(),
        "",
        row.posted.as_str(),
        row.url.as_str(),
    ];
    for (i, v) in values.iter().enumerate() {
        sheet.get_cell_mut((i as u32 + 1, next_row)).set_value(*v);
    }
    sheet
        .get_cell_mut((5, next_row))
        .set_value_number(row.score as f64);

    umya_spreadsheet::writer::xlsx::write(&book, path).with_context(|| {
        format!(
            "couldn't save {} — if it's open in Excel, close it and try again",
            path.display()
        )
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(company: &str) -> AppliedRow {
        AppliedRow {
            date: "2026-10-06".into(),
            company: company.into(),
            role: "Software Engineer Intern".into(),
            location: "Remote".into(),
            score: 91,
            posted: "today".into(),
            url: "https://example.com/job".into(),
        }
    }

    #[test]
    fn appends_below_existing_rows_and_keeps_user_edits() {
        let dir = std::env::temp_dir().join(format!("blip-test-{}", std::process::id()));
        let path = dir.join("Applied.xlsx");
        let _ = std::fs::remove_file(&path);

        append(&path, &row("Acme")).unwrap();
        append(&path, &row("Globex")).unwrap();

        // Simulate the user adding a note column and editing a cell.
        let mut book = umya_spreadsheet::reader::xlsx::read(&path).unwrap();
        let sheet = book.get_sheet_mut(&0).unwrap();
        sheet.get_cell_mut((8, 1)).set_value("My notes");
        sheet.get_cell_mut((8, 2)).set_value("Referral from Sam");
        umya_spreadsheet::writer::xlsx::write(&book, &path).unwrap();

        append(&path, &row("Initech")).unwrap();

        let book = umya_spreadsheet::reader::xlsx::read(&path).unwrap();
        let sheet = book.get_sheet(&0).unwrap();
        assert_eq!(sheet.get_highest_row(), 4);
        assert_eq!(sheet.get_value((1, 1)), "Date applied");
        assert_eq!(sheet.get_value((2, 2)), "Acme");
        assert_eq!(sheet.get_value((2, 3)), "Globex");
        assert_eq!(sheet.get_value((2, 4)), "Initech");
        assert_eq!(sheet.get_value((5, 4)), "91");
        assert_eq!(sheet.get_value((8, 2)), "Referral from Sam");

        // With Excel's owner file present, the append is refused untouched.
        std::fs::write(dir.join("~$Applied.xlsx"), b"").unwrap();
        let err = append(&path, &row("Umbrella")).unwrap_err().to_string();
        assert!(err.contains("open in Excel"), "{err}");
        let book = umya_spreadsheet::reader::xlsx::read(&path).unwrap();
        assert_eq!(book.get_sheet(&0).unwrap().get_highest_row(), 4);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
