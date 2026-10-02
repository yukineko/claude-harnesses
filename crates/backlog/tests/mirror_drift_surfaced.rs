#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The SessionStart hook must surface GitHub-mirror drift.
//!
//! Why this exists, measured 2026-10-02 at measurement point `448ff46f`: this
//! repo's store held **467 done/cancelled tasks whose GitHub issue was never
//! closed** (and only 2 confirmed closes in 800 terminal rows). Every
//! individual failure WAS reported — `add` prints the degraded mirror on
//! stderr, `sync --apply` exits non-zero — but nothing ever re-stated the
//! accumulated total, so a `gh`-absent machine drifted for months without one
//! visible signal. A hook has no exit code and no stderr the agent ever sees,
//! so `additionalContext` is the only channel that can carry it
//! (CLAUDE.md §1: silence is not an acceptable degrade).
//!
//! The drift report is deliberately NOT gated on `gh` being installed: the
//! whole point is to be visible on the machine where the mirror is broken.
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn hook(home: &Path, repo: &Path) -> String {
    let payload = format!(
        r#"{{"session_id":"s","cwd":"{}","hook_event_name":"SessionStart"}}"#,
        repo.display()
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_backlog"))
        .arg("session-start")
        .current_dir(repo)
        .env_clear()
        .env("HOME", home)
        .env("PATH", std::env::var("PATH").unwrap())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        use std::io::Write;
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(payload.as_bytes())
            .unwrap();
    }
    let out = child.wait_with_output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

struct Env {
    home: PathBuf,
    repo: PathBuf,
}

fn setup(tag: &str) -> Env {
    let t = std::env::temp_dir().join(format!("bl-drift-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&t);
    let (home, repo) = (t.join("home"), t.join("repo"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(repo.join(".backlog")).unwrap();
    assert!(Command::new("git")
        .args(["init", "-q", "."])
        .current_dir(&repo)
        .env("HOME", &home)
        .status()
        .unwrap()
        .success());
    Env { home, repo }
}

fn write_store(repo: &Path, pending: &str, done: &str) {
    std::fs::write(repo.join(".backlog/tasks.toml"), pending).unwrap();
    std::fs::write(repo.join(".backlog/tasks.done.toml"), done).unwrap();
}

fn task(id: &str, status: &str, at: i64, issue: Option<u64>) -> String {
    let mut s = format!(
        "[[task]]\nid = \"{id}\"\ntitle = \"t-{id}\"\nproject = \"/repo\"\ntags = []\n\
         status = \"{status}\"\nnotes = \"\"\ncreated_at = {at}\nupdated_at = {at}\nweight = 0.0\n"
    );
    if let Some(n) = issue {
        s.push_str(&format!(
            "issue_number = {n}\nissue_url = \"https://github.com/o/r/issues/{n}\"\n"
        ));
    }
    s.push('\n');
    s
}

/// THE REGRESSION. A store in the drifted shape must have its totals stated in
/// `additionalContext`. Dies if the drift section is absent, or if it reports
/// the counts of only one arm.
#[test]
fn drift_counts_are_injected_into_additional_context() {
    let e = setup("report");
    write_store(
        &e.repo,
        // 3 pending: two unmirrored (creates), one already mirrored (not drift).
        &format!(
            "{}{}{}",
            task("p1", "pending", 1, None),
            task("p2", "pending", 2, None),
            task("p3", "pending", 3, Some(10))
        ),
        // 2 terminal rows with an unclosed issue (closes).
        &format!(
            "{}{}",
            task("d1", "done", 4, Some(11)),
            task("c1", "cancelled", 5, Some(12))
        ),
    );
    let out = hook(&e.home, &e.repo);
    eprintln!("stdout={out}");
    assert!(
        out.contains("mirror") || out.contains("GitHub"),
        "no drift section at all: {out:?}"
    );
    assert!(
        out.contains('2'),
        "the two unmirrored pending tasks must be counted: {out:?}"
    );
    assert!(
        out.contains("backlog sync"),
        "the report must name the command that fixes it: {out:?}"
    );
    // Both arms must be stated. The close count is the one that silently grew
    // to 467, so it is asserted explicitly rather than inferred from a total.
    let v: serde_json::Value = out
        .lines()
        .find_map(|l| serde_json::from_str(l).ok())
        .expect("hook must emit one JSON line");
    let ctx = v["additionalContext"].as_str().unwrap_or_default();
    assert!(
        ctx.contains("2") && ctx.to_lowercase().contains("close"),
        "the unclosed-issue count must be stated: {ctx:?}"
    );
    assert!(
        ctx.to_lowercase().contains("creat"),
        "the unfiled-issue count must be stated: {ctx:?}"
    );
    // The injected text must read as prose, not as source indentation.
    // Observed 2026-10-02 in the shipped 0.3.20 build: the section was authored
    // as one `format!` with `\`-continued lines, and `cargo fmt` collapsed it
    // onto a single line, which turns each continuation's leading indentation
    // into LITERAL content — runs of 13 spaces mid-sentence. The escape only
    // strips whitespace while the newline is still there, so the formatter
    // silently changed the rendered string. Assert the rendered shape instead
    // of trusting the escape, since the thing that broke it was a tool nobody
    // was going to re-read the output after.
    for line in ctx.lines() {
        assert!(
            !line.trim_start().contains("  "),
            "injected prose must not carry runs of spaces from source \
             indentation; offending line: {line:?}"
        );
    }
}

/// ANTI-NOISE CONTROL. A store that has never produced a single issue is not
/// "drifted" — it is a project with no mirror (e.g. a non-GitHub remote), where
/// `sync_plan` would still name every pending task as a create. Reporting there
/// would train the reader to ignore the line.
/// Dies if the report is unconditional on the plan being non-empty.
#[test]
fn a_store_that_never_mirrored_anything_is_silent() {
    let e = setup("virgin");
    write_store(
        &e.repo,
        &format!(
            "{}{}",
            task("p1", "pending", 1, None),
            task("p2", "pending", 2, None)
        ),
        "",
    );
    let out = hook(&e.home, &e.repo);
    eprintln!("stdout={out}");
    let v: serde_json::Value = out
        .lines()
        .find_map(|l| serde_json::from_str(l).ok())
        .expect("hook must still list the pending tasks");
    let ctx = v["additionalContext"].as_str().unwrap_or_default();
    assert!(
        !ctx.to_lowercase().contains("mirror drift"),
        "a store with no mirror in use must not report drift: {ctx:?}"
    );
}

/// ANTI-VACUITY CONTROL. A fully-reconciled store (every terminal row's close
/// confirmed, every pending row mirrored) must also be silent — otherwise the
/// first test would pass against an implementation that always prints the
/// section.
#[test]
fn a_fully_reconciled_store_is_silent() {
    let e = setup("clean");
    let mut done = task("d1", "done", 4, Some(11));
    done = done.replace(
        "issue_url = \"https://github.com/o/r/issues/11\"",
        "issue_url = \"https://github.com/o/r/issues/11\"\nissue_closed_at = 1700000000",
    );
    write_store(&e.repo, &task("p1", "pending", 1, Some(10)), &done);
    let out = hook(&e.home, &e.repo);
    eprintln!("stdout={out}");
    let ctx = out
        .lines()
        .find_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .map(|v| {
            v["additionalContext"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .unwrap_or_default();
    assert!(
        !ctx.to_lowercase().contains("mirror drift"),
        "a reconciled store must not report drift: {ctx:?}"
    );
}
