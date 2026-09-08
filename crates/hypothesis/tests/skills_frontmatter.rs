// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Every `skills/<name>/SKILL.md` must carry YAML frontmatter.
//!
//! A SKILL.md without frontmatter is not a broken skill, it is an ABSENT one:
//! Claude Code registers skills from the `name`/`description` keys, so a file
//! that opens with prose is silently skipped. The failure is invisible from
//! the plugin's side — the file is present, `git status` is clean, the rollout
//! reports success — and only observable as `/hypothesis:hypothesis` not
//! existing. That is the shape CLAUDE.md §3 names: "not registered" and
//! "nothing to register" collapsing into the same output.
//!
//! Measured 2026-08-26 at 58c779af: `skills/hypothesis/SKILL.md` was the only
//! SKILL.md in all 39 crates whose first line was not `---`
//! (`for f in $(find crates -path '*/skills/*/SKILL.md'); do
//!   [ "$(head -1 "$f")" = "---" ] || echo "$f"; done`).
//! It had been unregistered for as long as it existed.

use std::path::{Path, PathBuf};

fn skills_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("skills")
}

fn skill_files() -> Vec<PathBuf> {
    let dir = skills_dir();
    let entries = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("skills/ is unreadable ({e}): {}", dir.display()));
    let mut found: Vec<PathBuf> = entries
        .map(|e| e.expect("dir entry").path().join("SKILL.md"))
        .filter(|p| p.is_file())
        .collect();
    found.sort();
    // An empty list would make every assertion below vacuously true — the
    // "checked nothing, reported clean" failure this crate's own store module
    // exists to prevent. Refuse instead of passing.
    assert!(
        !found.is_empty(),
        "no skills/*/SKILL.md found under {} — this test would otherwise pass \
         by examining nothing",
        dir.display()
    );
    found
}

/// The frontmatter block delimited by the leading `---` line, or `None` when
/// the file does not open with one.
fn frontmatter(body: &str) -> Option<&str> {
    let rest = body.strip_prefix("---\n")?;
    let end = rest.find("\n---")?;
    Some(&rest[..end])
}

#[test]
fn every_skill_has_frontmatter_with_name_and_description() {
    for path in skill_files() {
        let body = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("unreadable ({e}): {}", path.display()));
        let front = frontmatter(&body).unwrap_or_else(|| {
            panic!(
                "{} has no YAML frontmatter (first line: {:?}). Claude Code \
                 registers skills from the frontmatter `name`/`description`, \
                 so this file is not a skill with a problem — it is not a \
                 skill at all, and the slash command reads as \"not found\".",
                path.display(),
                body.lines().next().unwrap_or("")
            )
        });
        for key in ["name:", "description:"] {
            assert!(
                front.lines().any(|l| l.starts_with(key)),
                "{} frontmatter is missing `{key}`, without which the skill \
                 cannot be addressed (name) or selected (description): \
                 {front:?}",
                path.display()
            );
        }
    }
}

#[test]
fn frontmatter_name_matches_the_skill_directory() {
    for path in skill_files() {
        let dir_name = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
            .expect("skill dir name")
            .to_string();
        let body = std::fs::read_to_string(&path).expect("read");
        let Some(front) = frontmatter(&body) else {
            // Covered, with its own diagnostic, by the test above.
            continue;
        };
        let declared = front
            .lines()
            .find_map(|l| l.strip_prefix("name:"))
            .map(|v| v.trim().to_string())
            .unwrap_or_default();
        assert_eq!(
            declared,
            dir_name,
            "{} declares name {declared:?} but lives in directory {dir_name:?}; \
             the invocable path is built from the directory, so a mismatch \
             means the skill answers to a name nobody types",
            path.display()
        );
    }
}
