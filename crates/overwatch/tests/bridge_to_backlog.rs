// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Integration test for `overwatch review-queue --to-backlog`: the
//! Continuous-Audit finding→backlog bridge.
//!
//! Isolation (mirrors `lifecycle.rs`): HOME is repointed at a temp dir so the
//! overwatch storage root (`~/.overwatch/<project-key>/overwatch/`) and the
//! bridged-findings ledger live under the temp tree and never touch the real
//! session state. A fake `backlog` executable on the child's PATH records each
//! `add` so we can assert the bridge's shell-out contract deterministically,
//! without building/running the real backlog binary.
//!
//! This whole test is unix-specific: it writes and `chmod +x`es a `#!/bin/sh`
//! fake `backlog` and uses `PermissionsExt::set_mode`. Guard the entire test
//! binary to `cfg(unix)` (repo convention; non-unix is off the WSL/Mac/Linux
//! target). On non-unix this compiles to an empty test binary.
#![cfg(unix)]
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_overwatch")
}

/// Write a fake `backlog` script that appends `--title` and `--notes` of each
/// `add` invocation to `$FAKE_BACKLOG_LOG` (one tab-separated line per add),
/// then exits 0.
fn write_fake_backlog(dir: &Path) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let script = dir.join("backlog");
    let body = r#"#!/bin/sh
if [ "$1" = "add" ]; then
  shift
  title=""
  notes=""
  while [ $# -gt 0 ]; do
    case "$1" in
      --title) title="$2"; shift 2 ;;
      --notes) notes="$2"; shift 2 ;;
      *) shift ;;
    esac
  done
  printf '%s\t%s\n' "$title" "$notes" >> "$FAKE_BACKLOG_LOG"
fi
exit 0
"#;
    fs::write(&script, body).unwrap();
    let mut perm = fs::metadata(&script).unwrap().permissions();
    perm.set_mode(0o755);
    fs::set_permissions(&script, perm).unwrap();
    script
}

/// Count the lines (= number of `backlog add` invocations) in the fake log.
fn add_count(log: &Path) -> usize {
    match fs::read_to_string(log) {
        Ok(txt) => txt.lines().filter(|l| !l.is_empty()).count(),
        Err(_) => 0,
    }
}

/// Look up the `--notes` value recorded for the `add` invocation whose
/// `--title` matches `title` (tab-separated log written by the fake backlog).
fn notes_for(log: &Path, title: &str) -> Option<String> {
    let txt = fs::read_to_string(log).ok()?;
    txt.lines().find_map(|line| {
        let (t, notes) = line.split_once('\t')?;
        (t == title).then(|| notes.to_string())
    })
}

/// Run the bridge over `project_dir` with HOME/PATH/log wired to the temp tree.
fn run_bridge(
    project_dir: &Path,
    home: &Path,
    fake_bin_dir: &Path,
    log: &Path,
) -> std::process::Output {
    let path = format!(
        "{}:{}",
        fake_bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    Command::new(bin())
        .arg("review-queue")
        .arg("--to-backlog")
        .current_dir(project_dir)
        .env("HOME", home)
        .env("PATH", path)
        .env("FAKE_BACKLOG_LOG", log)
        .output()
        .expect("spawn overwatch")
}

#[test]
fn bridge_forwards_confirmed_findings_idempotently() {
    let uniq = format!("overwatch-bridge-{}", std::process::id());
    let root = std::env::temp_dir().join(uniq);
    let home = root.join("home");
    let fake_bin_dir = root.join("bin");
    let log = root.join("backlog-add.log");
    fs::create_dir_all(&home).unwrap();

    // The overwatch lib resolves the storage root from the *process* HOME, so
    // set it here too — seeding (below) and the child must agree on the path.
    std::env::set_var("HOME", &home);
    write_fake_backlog(&fake_bin_dir);

    // --- fail-soft: a project with NO findings store still exits 0 ----------
    let empty_project = root.join("empty-project");
    fs::create_dir_all(&empty_project).unwrap();
    let out = run_bridge(&empty_project, &home, &fake_bin_dir, &log);
    assert!(
        out.status.success(),
        "missing findings store must be fail-soft (exit 0), stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(add_count(&log), 0, "no findings -> no backlog adds");

    // --- happy path: seed findings, then bridge -----------------------------
    let project = root.join("project");
    fs::create_dir_all(&project).unwrap();

    // Same finding-id recorded twice at different ts (audit re-record), plus a
    // distinct finding-id once. Seed via the lib so ts is controlled.
    overwatch::store::append_review_finding(
        &project,
        &overwatch::review_finding::ReviewFinding::new(
            "F-1".into(),
            "reviewgate".into(),
            Some("high".into()),
            "unchecked unwrap in foo.rs".into(),
            Some("src/foo.rs".into()),
            None,
            100,
        ),
    )
    .unwrap();
    overwatch::store::append_review_finding(
        &project,
        &overwatch::review_finding::ReviewFinding::new(
            "F-1".into(),
            "reviewgate".into(),
            Some("high".into()),
            "unchecked unwrap in foo.rs (rerun)".into(),
            Some("src/foo.rs".into()),
            None,
            200,
        ),
    )
    .unwrap();
    overwatch::store::append_review_finding(
        &project,
        &overwatch::review_finding::ReviewFinding::new(
            "F-2".into(),
            "auditmap".into(),
            Some("low".into()),
            "missing test coverage in bar.rs".into(),
            None,
            Some("bar.rs:10 has no test covering the error branch".into()),
            150,
        ),
    )
    .unwrap();

    // First bridge: F-1 (collapsed from 2 records) + F-2 => exactly 2 adds.
    let out = run_bridge(&project, &home, &fake_bin_dir, &log);
    assert!(
        out.status.success(),
        "bridge must exit 0, stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        add_count(&log),
        2,
        "same finding-id collapses to one; distinct id is its own => 2 adds"
    );

    // --- notes carry the triage signals: elapsed days, rationale (if any), --
    // --- and a regression-test freshness verdict (fail-soft "no test" here) -
    let f1_notes = notes_for(&log, "unchecked unwrap in foo.rs (rerun)")
        .expect("F-1's backlog add must be logged");
    assert!(
        f1_notes.contains("confirmed:") && f1_notes.contains("日前"),
        "notes must include elapsed days: {f1_notes}"
    );
    assert!(
        f1_notes.contains("regression test: 該当テストなし"),
        "no matching #[ignore] test exists under the temp project dir: {f1_notes}"
    );

    let f2_notes = notes_for(&log, "missing test coverage in bar.rs")
        .expect("F-2's backlog add must be logged");
    assert!(
        f2_notes.contains("rationale: bar.rs:10 has no test covering the error branch"),
        "notes must include the finding's rationale: {f2_notes}"
    );

    // Second bridge: everything already bridged => NO new adds (idempotent).
    let out = run_bridge(&project, &home, &fake_bin_dir, &log);
    assert!(out.status.success());
    assert_eq!(
        add_count(&log),
        2,
        "re-running the bridge must not re-add already-bridged findings"
    );

    // The ledger records both distinct finding-ids exactly once.
    let bridged = overwatch::store::read_bridged_findings(&project).unwrap();
    assert!(bridged.contains(&"F-1".to_string()));
    assert!(bridged.contains(&"F-2".to_string()));
    assert_eq!(
        bridged.iter().filter(|id| *id == "F-1").count(),
        1,
        "F-1 recorded exactly once across two runs"
    );
    assert_eq!(bridged.len(), 2);

    // Best-effort cleanup (ignore errors).
    let _ = fs::remove_dir_all(&root);
    let _ = writeln!(std::io::stderr(), "bridge test complete");
}
