//! A raw backslash (regex `\b`, `\s`) in a title/notes value.
//!
//! 1. Through the CLI (`add` / `edit`) the value must be escaped so the store
//!    re-parses and round-trips the exact text.
//! 2. A hand-edited store with an invalid escape must fail to load with an
//!    error that names the line and column, not only "failed to parse <path>".
mod common;
use common::*;

const NASTY: &str = r#"regex \bfoo\s+ and C:\path\ "q" \u12 \"#;

#[test]
fn cli_written_backslashes_round_trip() {
    let f = Fixture::new("bs-cli");
    let id = f.add(NASTY);
    let o = f.run(&["edit", &id, "--notes", NASTY]);
    assert_eq!(o.code, 0, "edit failed: {}", o.both());
    let row = f.row(&id);
    assert_eq!(row["title"], NASTY, "title must round-trip");
    assert_eq!(row["notes"], NASTY, "notes must round-trip");
}

#[test]
fn hand_edited_invalid_escape_reports_line_and_column() {
    let f = Fixture::new("bs-hand");
    f.add("plain");
    let text = std::fs::read_to_string(f.tasks_path()).unwrap();
    let bad = text.replacen("title = \"plain\"", "title = \"a \\s b\"", 1);
    assert_ne!(bad, text, "fixture: title line not found");
    let line = bad
        .lines()
        .position(|l| l.starts_with("title = \"a \\s"))
        .unwrap()
        + 1;
    std::fs::write(f.tasks_path(), &bad).unwrap();
    let o = f.run(&["list", "--all"]);
    assert_ne!(o.code, 0, "unparseable store must fail: {}", o.both());
    let msg = o.both();
    assert!(
        msg.contains(&format!("line {line}")) && msg.contains("column"),
        "error must name line {line} and a column: {msg}"
    );
}
