//! Integration test for the *consolidation* half of `overwatch review-queue
//! --to-backlog`: draining the non-finding streams (systemic / rollback /
//! escalation) into the backlog. Exercised here via the rollback stream, which
//! needs no cross-binary state to seed.
//!
//! Isolation mirrors `bridge_to_backlog.rs`: HOME is repointed at a temp dir so
//! the overwatch storage root and the bridged-*entries* ledger live under the
//! temp tree, and a fake `backlog` on PATH records each `add` (title + notes)
//! so the shell-out contract is asserted deterministically. Lives in its own
//! test binary so its process-global HOME mutation can't race the finding
//! bridge test. Unix-only for the same `#!/bin/sh` + chmod reasons.
#![cfg(unix)]
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_overwatch")
}

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

fn add_count(log: &Path) -> usize {
    match fs::read_to_string(log) {
        Ok(txt) => txt.lines().filter(|l| !l.is_empty()).count(),
        Err(_) => 0,
    }
}

/// The single `--notes` value recorded (this test seeds exactly one add).
fn only_notes(log: &Path) -> Option<String> {
    let txt = fs::read_to_string(log).ok()?;
    txt.lines()
        .next()
        .and_then(|line| line.split_once('\t').map(|(_, n)| n.to_string()))
}

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
fn bridge_forwards_rollback_entries_idempotently() {
    let uniq = format!("overwatch-bridge-entries-{}", std::process::id());
    let root = std::env::temp_dir().join(uniq);
    let home = root.join("home");
    let fake_bin_dir = root.join("bin");
    let log = root.join("backlog-add.log");
    fs::create_dir_all(&home).unwrap();

    // The lib resolves the storage root from the *process* HOME, so seeding and
    // the child must agree on the path.
    std::env::set_var("HOME", &home);
    write_fake_backlog(&fake_bin_dir);

    let project = root.join("project");
    fs::create_dir_all(&project).unwrap();

    // Seed two rollback events for the SAME plugin — build_queue collapses them
    // to one row, so the drain must produce exactly one backlog add.
    for ts in [1_000_i64, 2_000] {
        overwatch::store::append_rollback(
            &project,
            &overwatch::rollback::RollbackEvent::new(
                "specguard".into(),
                Some("0.1.0".into()),
                "0.2.0".into(),
                0,
                overwatch::rollback::RollbackReason::Raw,
                ts,
                None,
            ),
        )
        .unwrap();
    }

    // First drain: one collapsed rollback row => exactly one add.
    let out = run_bridge(&project, &home, &fake_bin_dir, &log);
    assert!(
        out.status.success(),
        "drain must exit 0, stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        add_count(&log),
        1,
        "two same-plugin rollbacks collapse to one add"
    );

    // The notes carry the composite-key provenance for the rollback stream.
    let notes = only_notes(&log).expect("the rollback add must be logged");
    assert!(
        notes.contains("kind:rollback") && notes.contains("identifier:specguard"),
        "notes must carry the rollback provenance: {notes}"
    );

    // The stdout summary reports the entry counts.
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"entries_bridged\":1") && stdout.contains("\"entries_considered\":1"),
        "summary must report the drained entry counts: {stdout}"
    );

    // Second drain: already bridged => no new add (idempotent via the ledger).
    let out2 = run_bridge(&project, &home, &fake_bin_dir, &log);
    assert!(out2.status.success());
    assert_eq!(
        add_count(&log),
        1,
        "re-running must not re-add a bridged entry"
    );

    // The entries ledger records the composite key exactly once...
    let entries = overwatch::store::read_bridged_entries(&project).unwrap();
    assert_eq!(entries, vec!["rollback:specguard".to_string()]);
    // ...and it did NOT leak into the finding-resolution ledger.
    assert!(overwatch::store::read_bridged_findings(&project)
        .unwrap()
        .is_empty());

    let _ = fs::remove_dir_all(&root);
    let _ = writeln!(std::io::stderr(), "bridge entries test complete");
}
