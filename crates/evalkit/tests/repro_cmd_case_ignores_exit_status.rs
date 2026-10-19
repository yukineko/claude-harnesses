#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro: a `cmd` case with no `assert.exit` passes when the command fails.
//!
//! `run.rs::check_exit` returns immediately when `assert.exit` is `None`, and
//! the remaining checks look at stdout only. A command that crashes (exit 101,
//! empty stdout) therefore satisfies `not_contains` and the case is reported
//! `ok`. The subprocess exit status is not used in the verdict.
//!
//! Asserts the correct behaviour: a cmd that exited non-zero, with no explicit
//! exit expectation, must not be reported as a pass.

use std::process::Command;

#[test]
fn crashing_cmd_without_exit_assertion_is_not_a_pass() {
    let dir = std::env::temp_dir().join(format!("evalkit-repro-exit-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("g.jsonl"),
        "{\"id\":\"crashy\",\"cmd\":[\"sh\",\"-c\",\"exit 101\"],\"assert\":{\"not_contains\":[\"ERROR\"]}}\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_evalkit"))
        .args(["run", "--dir", dir.to_str().unwrap()])
        .output()
        .unwrap();
    std::fs::remove_dir_all(&dir).ok();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_ne!(
        out.status.code(),
        Some(0),
        "command exited 101 but the case passed:\n{stdout}"
    );
}
