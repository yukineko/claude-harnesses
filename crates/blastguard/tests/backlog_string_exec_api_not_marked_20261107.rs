// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro (2026-11-07, condukt run run-20261107-011838-71703 task t1b,
//! verifier finding; observed by the orchestrator with the blastguard binary
//! built at ba8a9525 vs the deployed 0.2.269):
//!   python3 stdin heredoc `import timeit, os` /
//!   `timeit.timeit("os.unlink('/tmp/x.tmp')", number=1)`   0.2.269: deny, ba8a9525: ask
//! `timeit.timeit` EXECUTES its string argument, but `timeit` is not in
//! DYNAMIC_EXECUTION_MARKERS (crates/blastguard/src/detect/interpreter/inert_literal.rs),
//! so the backlog 27795e6e inert-literal downgrade (Deny to Ask) applies.
//! Other string-executing APIs likely in the same gap (not observed): cProfile.run,
//! pdb.run, node setTimeout/setInterval with a string, vm.runIn*.
//! Two readings: (a) defect — a string that is run as code must keep Deny;
//! (b) accepted residual — the downgrade target is Ask (a refusal; hardened to
//! Deny with no human present), and the marker list is documented as a
//! heuristic. Tell apart: decide whether "string reaches an executor" must be
//! Deny (task statement: "a bare string literal that never reaches
//! exec/eval/compile must NOT be deny") — timeit is such an executor.

#[path = "support/bounded.rs"]
mod bounded;

use std::process::{Command, Stdio};

fn decision(command: &str) -> String {
    let bin = env!("CARGO_BIN_EXE_blastguard");
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": command },
    })
    .to_string();
    let mut hook = Command::new(bin);
    hook.env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut hook, payload.as_bytes(), bounded::HOOK_DEADLINE);
    assert_eq!(out.status.code(), Some(0), "hook must exit 0 ({command:?})");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return "allow".to_string();
    }
    let doc: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout not JSON for {command:?} ({e}): {stdout}"));
    doc.get("hookSpecificOutput")
        .and_then(|h| h.get("permissionDecision"))
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("no permissionDecision for {command:?}: {stdout}"))
        .to_string()
}

#[test]
#[ignore = "backlog b4c03877: timeit string argument not a dynamic-execution marker, gives Ask (RED)"]
fn timeit_string_argument_is_executed_so_stays_denied() {
    let cmd = "python3 - <<'PY'\nimport timeit, os\ntimeit.timeit(\"os.unlink('/tmp/x.tmp')\", number=1)\nPY";
    assert_eq!(decision(cmd), "deny", "{cmd:?}");
}
