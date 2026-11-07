// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! No shipped agent/skill prompt may contain a whole-tree stage command.
//!
//! Background (user ruling 2026-11-07): blastguard 0.2.300 Asks (and denies
//! inside subagents) on whole-tree staging, so a prompt that tells a worker to
//! run one stalls every commit it drives.
//!
//! RULE (deliberately simple and deterministic): the *literal command* must not
//! appear ANYWHERE in a shipped prompt (`crates/*/agents/**`, `crates/*/skills/**`),
//! **including prose that prohibits it**. Prohibitions must be phrased without
//! the literal command (e.g. "stage by explicit path only").
//!
//! Flagged: `git add` / `git stage` (global options such as `-C <dir>` allowed
//! in between) followed by `-A`, `--all`, `-u`, `--update`, a short-flag cluster
//! containing `A`/`u`, or a bare `.` / `./` / `:/` pathspec; and `git commit`
//! with `-a`, `--all`, or a short-flag cluster containing `a` (`-am`, `-qa`).
//! Not flagged: `git add <explicit path>`, `git commit -m msg -- path`.
//!
//! Fail closed (CLAUDE.md §3): zero prompt files found, or any unreadable
//! directory/file, fails the test. The matcher is self-tested below so a
//! matcher that matches nothing cannot pass silently.

use std::path::{Path, PathBuf};

fn strip(t: &str) -> &str {
    t.trim_matches(|c: char| matches!(c, '`' | '"' | '\'' | '(' | ')'))
}

fn is_sep(raw: &str) -> bool {
    let s = strip(raw);
    matches!(s, "&&" | "||" | ";" | "|") || raw.ends_with(';')
}

fn is_cluster(s: &str, letters: &[char]) -> bool {
    s.len() >= 2
        && s.starts_with('-')
        && !s.starts_with("--")
        && s[1..].chars().all(|c| c.is_ascii_alphabetic())
        && s[1..].chars().any(|c| letters.contains(&c))
}

/// True if `line` contains a whole-tree stage/commit command.
fn line_hits(line: &str) -> bool {
    let toks: Vec<&str> = line.split_whitespace().collect();
    for (i, raw) in toks.iter().enumerate() {
        if strip(raw) != "git" {
            continue;
        }
        let mut j = i + 1;
        // skip global options
        while j < toks.len() {
            let t = strip(toks[j]);
            if t == "-C" || t == "-c" {
                j += 2;
            } else if t.starts_with('-') {
                j += 1;
            } else {
                break;
            }
        }
        let Some(sub) = toks.get(j).map(|t| strip(t)) else {
            continue;
        };
        if sub != "add" && sub != "stage" && sub != "commit" {
            continue;
        }
        let is_commit = sub == "commit";
        for raw in &toks[j + 1..] {
            if is_sep(raw) {
                let s = strip(raw.trim_end_matches(';'));
                if arg_hits(s, is_commit) {
                    return true;
                }
                break;
            }
            // commit: a quoted token is a message value, stop scanning
            if is_commit && (raw.starts_with('"') || raw.starts_with('\'')) {
                break;
            }
            let s = strip(raw);
            if s == "--" {
                break;
            }
            if arg_hits(s, is_commit) {
                return true;
            }
        }
    }
    false
}

fn arg_hits(s: &str, is_commit: bool) -> bool {
    if is_commit {
        s == "-a" || s == "--all" || is_cluster(s, &['a'])
    } else {
        matches!(s, "-A" | "--all" | "-u" | "--update" | "." | "./" | ":/")
            || is_cluster(s, &['A', 'u'])
    }
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let rd = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("cannot read dir {dir:?}: {e}"));
    for ent in rd {
        let ent = ent.unwrap_or_else(|e| panic!("cannot read entry in {dir:?}: {e}"));
        let p = ent.path();
        let ft = ent
            .file_type()
            .unwrap_or_else(|e| panic!("cannot stat {p:?}: {e}"));
        if ft.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

#[test]
fn matcher_self_test() {
    for pos in [
        "git add -A && git commit -m x",
        "run `git add -A` now",
        "git add --all",
        "git add .",
        "git add ./",
        "git add :/",
        "git add -u",
        "git add --update",
        "git -C /tmp/wt add -A",
        "git -C dir stage .",
        "git commit -a -m x",
        "git commit --all",
        "git commit -am \"msg\"",
        "git commit -qa",
        "git add foo.rs && git add .",
    ] {
        assert!(line_hits(pos), "matcher must flag: {pos}");
    }
    for neg in [
        "git add crates/condukt/src/main.rs",
        "git add foo.rs bar.rs",
        "git add ./foo.rs",
        "git commit -m msg -- path",
        "git commit -m \"fix -a thing\"",
        "git commit --amend --no-edit --trailer \"X: y\"",
        "git status --short",
        "stage everything with care",
        "git commit -m x",
    ] {
        assert!(!line_hits(neg), "matcher must NOT flag: {neg}");
    }
}

#[test]
fn shipped_prompts_do_not_instruct_whole_tree_staging() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let crates = root.join("crates");
    let mut files = Vec::new();
    let rd = std::fs::read_dir(&crates).unwrap_or_else(|e| panic!("cannot read {crates:?}: {e}"));
    for ent in rd {
        let c = ent
            .unwrap_or_else(|e| panic!("cannot read crates entry: {e}"))
            .path();
        for sub in ["agents", "skills"] {
            let d = c.join(sub);
            if d.is_dir() {
                walk(&d, &mut files);
            }
        }
    }
    assert!(
        !files.is_empty(),
        "found zero prompt files under crates/*/agents and crates/*/skills; \
         an empty set is not a pass"
    );
    files.sort();
    let mut hits = Vec::new();
    for f in &files {
        let bytes = std::fs::read(f).unwrap_or_else(|e| panic!("cannot read {f:?}: {e}"));
        let text = String::from_utf8_lossy(&bytes);
        for (n, line) in text.lines().enumerate() {
            if line_hits(line) {
                let rel = f.strip_prefix(&root).unwrap_or(f);
                hits.push(format!("{}:{}: {}", rel.display(), n + 1, line.trim()));
            }
        }
    }
    assert!(
        hits.is_empty(),
        "shipped prompts contain whole-tree stage commands ({} of {} files scanned):\n{}",
        hits.len(),
        files.len(),
        hits.join("\n")
    );
}
