#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro for backlog 03f563ba: `specguard audit` on an EMPTY spec map exits 0,
//! the same status as a clean audit of a populated map, so a caller reading the
//! exit code cannot tell "checked, zero findings" from "nothing was checked"
//! (CLAUDE.md §3: an empty set is not clean).

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn scratch(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "specguard-backlog-03f563ba-{tag}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
#[ignore = "backlog 03f563ba: open defect, remove ignore when fixed"]
fn backlog_03f563ba_empty_map_audit_does_not_exit_like_clean() {
    let dir = scratch("empty");
    fs::write(
        dir.join("specguard.toml"),
        "[project]\nname = \"Demo\"\nroot = \".\"\n\n[output]\nreport_dir = \"reports\"\nsentinel = \".pending\"\n\n[[area]]\nname = \"src\"\nglobs = [\"src/**\"]\ncanon = [\"docs/spec.md\"]\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_specguard"))
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .current_dir(&dir)
        .args([
            "--config",
            "specguard.toml",
            "audit",
            "--date",
            "2026-01-01",
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let _ = fs::remove_dir_all(&dir);
    // Anti-vacuity: we really are on the empty-map path.
    assert!(
        stdout.contains("0 entries mapped") || stderr.contains("0 entries mapped"),
        "precondition: expected the empty-map summary; stdout={stdout:?} stderr={stderr:?}"
    );
    assert_ne!(
        out.status.code(),
        Some(0),
        "empty spec map audit exited 0 — indistinguishable from a clean audit. stdout={stdout:?}"
    );
}
