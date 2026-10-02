#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! close-evidence: `needs-ruling` backlog rows surface in `overwatch
//! review-queue` as their own stream, and are never bridged back into backlog.
//!
//! Assumed backlog TOML shape (flat fields on the `[[task]]` row):
//!   status = "needs-ruling"
//!   ruling_kind = "judgment" | "untestable"
//!   rationale = "..."            (judgment)
//!   untestable_reason = "..."    (untestable)
//! Row JSON: kind = "needs-ruling", identifier = task id, summary contains the
//! title, the ruling kind and the rationale / untestable reason.
//!
//! Hermetic: temp HOME + temp git repo as cwd, real binary.
#![cfg(unix)]
use serde_json::Value;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static N: AtomicU64 = AtomicU64::new(0);

fn sandbox(tag: &str) -> (PathBuf, PathBuf) {
    let n = N.fetch_add(1, Ordering::Relaxed);
    let base =
        std::env::temp_dir().join(format!("ow-needsruling-{tag}-{}-{n}", std::process::id()));
    let home = base.join("home");
    let work = base.join("work");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(work.join(".backlog")).unwrap();
    let st = Command::new("git")
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

fn write_tasks(work: &Path, body: &str) {
    fs::write(work.join(".backlog/tasks.toml"), body).unwrap();
}

fn ow(home: &Path, work: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_overwatch"))
        .args(args)
        .env("HOME", home)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .current_dir(work)
        .output()
        .expect("spawn overwatch")
}

fn rows(out: &Output) -> Vec<Value> {
    let s = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str::<Value>(s.trim())
        .unwrap_or_else(|e| panic!("stdout not JSON ({e}): {s}"))
        .as_array()
        .expect("json array")
        .clone()
}

fn of_kind<'a>(rs: &'a [Value], kind: &str) -> Vec<&'a Value> {
    rs.iter().filter(|r| r["kind"] == kind).collect()
}

#[test]
fn needs_ruling_rows_appear_with_id_title_kind_rationale() {
    let (home, work) = sandbox("appear");
    let body = [
        task(
            "nr-judg-1",
            "judgment close of stale ticket",
            "needs-ruling",
            "ruling_kind = \"judgment\"\nrationale = \"value is gone after redesign\"",
        ),
        task(
            "nr-untest-1",
            "race in deployed hook",
            "needs-ruling",
            "ruling_kind = \"untestable\"\nuntestable_reason = \"needs a real TTY fleet\"",
        ),
    ]
    .concat();
    write_tasks(&work, &body);

    let out = ow(&home, &work, &["review-queue", "--json"]);
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rs = rows(&out);
    let nr = of_kind(&rs, "needs-ruling");
    assert_eq!(nr.len(), 2, "rows: {rs:?}");

    let j = nr
        .iter()
        .find(|r| r["identifier"] == "nr-judg-1")
        .expect("judgment row");
    let s = j["summary"].as_str().unwrap();
    assert!(s.contains("judgment close of stale ticket"), "title: {s}");
    assert!(s.contains("judgment"), "ruling kind: {s}");
    assert!(s.contains("value is gone after redesign"), "rationale: {s}");

    let u = nr
        .iter()
        .find(|r| r["identifier"] == "nr-untest-1")
        .expect("untestable row");
    let s = u["summary"].as_str().unwrap();
    assert!(s.contains("race in deployed hook"), "title: {s}");
    assert!(s.contains("untestable"), "ruling kind: {s}");
    assert!(
        s.contains("needs a real TTY fleet"),
        "untestable reason: {s}"
    );
}

#[test]
fn pending_and_done_rows_are_not_in_the_stream() {
    let (home, work) = sandbox("nonstream");
    let body = [
        task("p-1", "pending one", "pending", ""),
        task("d-1", "done one", "done", ""),
        task(
            "nr-1",
            "only ruling row",
            "needs-ruling",
            "ruling_kind = \"judgment\"\nrationale = \"r\"",
        ),
    ]
    .concat();
    write_tasks(&work, &body);

    let out = ow(&home, &work, &["review-queue", "--json"]);
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rs = rows(&out);
    let nr = of_kind(&rs, "needs-ruling");
    assert_eq!(nr.len(), 1, "exactly the needs-ruling row: {rs:?}");
    assert_eq!(nr[0]["identifier"], "nr-1");
    let all = serde_json::to_string(&rs).unwrap();
    assert!(
        !all.contains("p-1") && !all.contains("d-1"),
        "leaked: {all}"
    );
}

#[test]
fn unparseable_tasks_toml_is_in_band_undetermined_and_exit_3() {
    let (home, work) = sandbox("undet");
    write_tasks(&work, "this is [[[ not toml = = =\n");

    let out = ow(&home, &work, &["review-queue", "--json"]);
    assert_eq!(
        out.status.code(),
        Some(3),
        "undetermined source must exit 3; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let rs = rows(&out);
    let u = of_kind(&rs, "undetermined-source");
    assert!(
        u.iter().any(|r| r["identifier"]
            .as_str()
            .is_some_and(|i| i.contains("tasks.toml"))),
        "in-band undetermined-source row naming tasks.toml expected: {rs:?}"
    );
    assert!(of_kind(&rs, "needs-ruling").is_empty());
}

#[test]
fn to_backlog_never_bridges_a_needs_ruling_row() {
    let (home, work) = sandbox("bridge");
    write_tasks(
        &work,
        &task(
            "nr-bridge-1",
            "must not be re-added",
            "needs-ruling",
            "ruling_kind = \"untestable\"\nuntestable_reason = \"cannot observe\"",
        ),
    );
    let root = work.parent().unwrap();
    let log = root.join("adds.log");
    let script = root.join("fake-backlog");
    fs::write(
        &script,
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$FAKE_BACKLOG_LOG\"\nexit 0\n",
    )
    .unwrap();
    let mut p = fs::metadata(&script).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(&script, p).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_overwatch"))
        .args(["review-queue", "--to-backlog"])
        .env("HOME", &home)
        .env("FAKE_BACKLOG_LOG", &log)
        .env("OVERWATCH_BACKLOG_BIN", &script)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .current_dir(&work)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let logged = fs::read_to_string(&log).unwrap_or_default();
    assert!(
        !logged.contains("must not be re-added") && !logged.contains("nr-bridge-1"),
        "needs-ruling row was bridged into backlog: {logged}"
    );
    // Positive control so "not bridged" is not vacuous: the same store must
    // list the row in review-queue.
    let q = ow(&home, &work, &["review-queue", "--json"]);
    assert_eq!(of_kind(&rows(&q), "needs-ruling").len(), 1);
}
