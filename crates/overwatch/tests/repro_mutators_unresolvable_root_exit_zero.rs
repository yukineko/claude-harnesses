#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Repro: lease mutators other than `end` exit 0 when the project root is
//! unresolvable (`.git` file containing garbage). `end` already bails with a
//! non-zero exit in the same setup; `heartbeat` / `run` / `reap` / `reassign`
//! take the `acquire_or_skip -> None` arm and `return Ok(())`, which callers
//! read as success ("refreshed 0", "reaped 0", silence). Undetermined must be
//! a non-zero exit like `end`.
use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_overwatch"))
}

fn run_in_bad_root(tag: &str, args: &[&str]) -> (Option<i32>, String, String) {
    let t = std::env::temp_dir().join(format!("ow-repro-mut-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&t);
    let home = t.join("home");
    let cwd = t.join("proj");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::write(cwd.join(".git"), "garbage").unwrap();
    let out = Command::new(bin())
        .current_dir(&cwd)
        .env("HOME", &home)
        .args(args)
        .output()
        .unwrap();
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

#[test]
fn control_end_is_nonzero_on_unresolvable_root() {
    // Not ignored: documents the reference behaviour the others must match.
    let (code, _, _) = run_in_bad_root("end", &["end", "--key", "k1"]);
    assert_ne!(code, Some(0), "control: end must already be non-zero");
}

#[test]
#[ignore = "repro: heartbeat exits 0 on unresolvable root; remove ignore when fixed"]
fn heartbeat_on_unresolvable_root_must_be_nonzero() {
    let (code, so, se) = run_in_bad_root("hb", &["heartbeat", "--key", "k1"]);
    assert_ne!(
        code,
        Some(0),
        "heartbeat exited 0 on an unresolvable root.\nstdout={so}\nstderr={se}"
    );
}

#[test]
#[ignore = "repro: run exits 0 on unresolvable root; remove ignore when fixed"]
fn run_on_unresolvable_root_must_be_nonzero() {
    let (code, so, se) = run_in_bad_root("run", &["run", "--key", "k1"]);
    assert_ne!(
        code,
        Some(0),
        "run exited 0 on an unresolvable root.\nstdout={so}\nstderr={se}"
    );
}

#[test]
#[ignore = "repro: reap exits 0 on unresolvable root; remove ignore when fixed"]
fn reap_on_unresolvable_root_must_be_nonzero() {
    let (code, so, se) = run_in_bad_root("reap", &["reap"]);
    assert_ne!(
        code,
        Some(0),
        "reap exited 0 on an unresolvable root.\nstdout={so}\nstderr={se}"
    );
}

#[test]
#[ignore = "repro: reassign exits 0 on unresolvable root; remove ignore when fixed"]
fn reassign_on_unresolvable_root_must_be_nonzero() {
    let (code, so, se) = run_in_bad_root("reassign", &["reassign", "--key", "k1", "--to", "x"]);
    assert_ne!(
        code,
        Some(0),
        "reassign exited 0 on an unresolvable root.\nstdout={so}\nstderr={se}"
    );
}
