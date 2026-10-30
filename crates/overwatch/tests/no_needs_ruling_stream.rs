#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! User ruling 2026-10-15: `needs-ruling` no longer exists, so
//! `overwatch review-queue` must have no needs-ruling stream. Even with a
//! legacy `status = "needs-ruling"` row in tasks.toml, no row of kind
//! "needs-ruling" and no "needs-ruling" source appears in either output form.
//! (A stray row is backlog's fail-closed concern; overwatch must not resurrect
//! the stream.) Hermetic: temp HOME + temp git repo as cwd, real binary.
//! Written by an independent test writer (CLAUDE.md §2(a)).
#![cfg(unix)]
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static N: AtomicU64 = AtomicU64::new(0);

fn sandbox(tag: &str) -> (PathBuf, PathBuf) {
    let n = N.fetch_add(1, Ordering::Relaxed);
    let base = std::env::temp_dir().join(format!("ow-nonr-{tag}-{}-{n}", std::process::id()));
    let home = base.join("home");
    let work = base.join("work");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(work.join(".backlog")).unwrap();
    let st = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(["init", "-q"])
        .current_dir(&work)
        .status()
        .expect("git init");
    assert!(st.success(), "git init failed");
    (home, work)
}

fn task(id: &str, title: &str, status: &str, extra: &str) -> String {
    format!(
        "[[task]]\nid = \"{id}\"\ntitle = \"{title}\"\nproject = \"/x\"\nproject_unresolved = false\n\
         tags = []\ntouched_files = []\nstatus = \"{status}\"\n{extra}\n"
    )
}

fn ow(home: &Path, work: &Path, args: &[&str]) -> Output {
    harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_overwatch")))
        .args(args)
        .env("HOME", home)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .current_dir(work)
        .output()
        .expect("spawn overwatch")
}

fn seed(work: &Path) {
    let body = [
        task("p-1", "pending one", "pending", ""),
        task(
            "nr-1",
            "legacy ruling row",
            "needs-ruling",
            "ruling_kind = \"judgment\"\nrationale = \"r\"",
        ),
    ]
    .concat();
    fs::write(work.join(".backlog/tasks.toml"), body).unwrap();
}

#[test]
fn json_output_has_no_needs_ruling_row() {
    let (home, work) = sandbox("json");
    seed(&work);
    let out = ow(&home, &work, &["review-queue", "--json"]);
    let s = String::from_utf8_lossy(&out.stdout);
    let v: Value = serde_json::from_str(s.trim()).unwrap_or_else(|e| {
        panic!(
            "stdout not JSON ({e}): {s} stderr={}",
            String::from_utf8_lossy(&out.stderr)
        )
    });
    let rows = v.as_array().expect("json array");
    let nr: Vec<_> = rows
        .iter()
        .filter(|r| r["kind"] == "needs-ruling")
        .collect();
    assert!(nr.is_empty(), "needs-ruling stream must not exist: {nr:?}");
    let all = serde_json::to_string(rows).unwrap();
    assert!(
        !all.contains("nr-1") && !all.contains("legacy ruling row"),
        "legacy needs-ruling row leaked into the queue: {all}"
    );
}

#[test]
fn human_output_names_no_needs_ruling_source() {
    let (home, work) = sandbox("text");
    seed(&work);
    let out = ow(&home, &work, &["review-queue"]);
    let both = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !both.contains("needs-ruling") && !both.contains("legacy ruling row"),
        "human review-queue output still names a needs-ruling source: {both}"
    );
}
