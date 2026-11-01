//! Every blastguard test that runs a child process must bound the wait.
//!
//! `support/bounded.rs::run_bounded` is the one place that waits on a child
//! under a deadline (stdin write, wait and drain, process group killed on
//! timeout). A test file that waits some other way hangs forever if the child
//! hangs, and leaves the child behind as an orphan.
//!
//! This scan walks every `crates/blastguard/tests/**/*.rs` except
//! `support/bounded.rs` and fails, naming `file:line`, for each unbounded
//! child wait. There is no allow-list: a site that is bounded some other way
//! is still reported, so a person decides, not this file.
//!
//! One test fn per construct. The needles are assembled at run time so this
//! file does not contain the constructs it searches for.
//!
//! Scope of the line match:
//! - A line whose first non-blank characters are `//` is skipped (docs and
//!   comments name the constructs without running them).
//! - The child-wait construct matches only when the receiver identifier
//!   contains `child`; a `Barrier::wait` on `b` is not a child wait.

use std::path::{Path, PathBuf};

const THIS_FILE: &str = "no_unbounded_child_wait.rs";

fn tests_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests")
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let rd = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()));
    for entry in rd {
        let entry = entry.unwrap_or_else(|e| panic!("dir entry in {}: {e}", dir.display()));
        let path = entry.path();
        let ft = entry
            .file_type()
            .unwrap_or_else(|e| panic!("file_type {}: {e}", path.display()));
        if ft.is_dir() {
            collect(&path, out);
        } else if path.extension().is_some_and(|x| x == "rs") {
            out.push(path);
        }
    }
}

/// Every scanned file, relative to `tests/`, with `support/bounded.rs` removed.
fn scanned_files() -> Vec<(String, String)> {
    let root = tests_dir();
    let mut paths = Vec::new();
    collect(&root, &mut paths);
    let mut files = Vec::new();
    let mut saw_helper = false;
    for p in paths {
        let rel = p
            .strip_prefix(&root)
            .expect("under tests/")
            .to_string_lossy()
            .replace('\\', "/");
        if rel == "support/bounded.rs" {
            saw_helper = true;
            continue;
        }
        let src = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {rel}: {e}"));
        files.push((rel, src));
    }
    // An empty or helper-less scan proves nothing: refuse to pass on it.
    assert!(
        saw_helper,
        "support/bounded.rs not found: the scan root is wrong"
    );
    assert!(
        files.iter().any(|(rel, _)| rel == THIS_FILE),
        "{THIS_FILE} not among the scanned files: the scan root is wrong"
    );
    files
}

/// The identifier immediately before `needle_at` in `line`.
fn receiver(line: &str, needle_at: usize) -> &str {
    let head = &line[..needle_at];
    let start = head
        .rfind(|c: char| !(c.is_alphanumeric() || c == '_'))
        .map_or(0, |i| i + 1);
    &head[start..]
}

/// `file:line: text` for every non-comment line that `hit` accepts.
fn flagged(hit: impl Fn(&str) -> bool) -> Vec<String> {
    let mut found = Vec::new();
    for (rel, src) in scanned_files() {
        for (i, line) in src.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            if hit(line) {
                found.push(format!("{rel}:{}: {}", i + 1, line.trim()));
            }
        }
    }
    found
}

fn assert_none(construct: &str, found: Vec<String>) {
    assert!(
        found.is_empty(),
        "{} unbounded child wait(s) via `{construct}`; use support/bounded.rs run_bounded:\n{}",
        found.len(),
        found.join("\n")
    );
}

#[test]
fn no_wait_with_output_outside_the_bounded_helper() {
    let needle = format!("wait_with{}(", "_output");
    assert_none(&needle, flagged(|l| l.contains(&needle)));
}

#[test]
fn no_child_wait_outside_the_bounded_helper() {
    let needle = format!(".wait{}", "()");
    let found = flagged(|l| {
        l.match_indices(&needle)
            .any(|(at, _)| receiver(l, at).contains("child"))
    });
    assert_none(&format!("child{needle}"), found);
}

#[test]
fn no_command_output_outside_the_bounded_helper() {
    let needle = format!(".out{}", "put()");
    assert_none(&needle, flagged(|l| l.contains(&needle)));
}

#[test]
fn no_command_status_outside_the_bounded_helper() {
    let needle = format!(".sta{}", "tus()");
    assert_none(&needle, flagged(|l| l.contains(&needle)));
}
