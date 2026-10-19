#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro: an `evalkit run` whose cases are ALL drafts exits 0.
//!
//! `run.rs::execute` computes `total` over every parsed case, drafts included,
//! so the "no cases found -> exit 2" guard does not fire when every case is a
//! draft. Every outcome is then `skipped`, `failed == 0`, and the run exits 0
//! while printing "0/1 passed, 0 failed, 1 draft(s) skipped": nothing was
//! checked, yet the exit status says all clear.
//!
//! Asserts the correct behaviour: when no case was actually evaluated, the
//! run is a harness error (non-zero), not success.

use std::process::Command;

#[test]
#[ignore = "RED repro: all-draft evalkit run exits 0 having checked nothing"]
fn all_draft_run_does_not_exit_zero() {
    let dir = std::env::temp_dir().join(format!("evalkit-repro-alldraft-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("g.jsonl"),
        "{\"id\":\"d\",\"describe\":\"pending\",\"draft\":true}\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_evalkit"))
        .args(["run", "--dir", dir.to_str().unwrap()])
        .output()
        .unwrap();
    std::fs::remove_dir_all(&dir).ok();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("1 draft(s) skipped"),
        "setup: expected the draft to be skipped, got: {stdout}"
    );
    assert_ne!(
        out.status.code(),
        Some(0),
        "nothing was evaluated (all drafts) but evalkit exited 0:\n{stdout}"
    );
}
