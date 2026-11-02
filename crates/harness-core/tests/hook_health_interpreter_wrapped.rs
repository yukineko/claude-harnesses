#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Repro: `missing_hook_binaries` treats the first whitespace token of a hook
//! command as the binary path. For an interpreter-wrapped command such as
//! `bash /abs/script.sh` that token is the bare name `bash`, which is then
//! existence-checked as a path RELATIVE TO THE CWD — so the verdict depends on
//! where harness-status happens to run, and the script that actually matters
//! is never checked.
//!
//! Observed 2026-11-02: `harness-status hooks-health` flagged
//! `bash ~/.claude/hooks/foregone-ask-guard.sh` as missing from a repo cwd,
//! and did NOT flag it from `/usr/bin` (where `./bash` exists), while the
//! script itself was present and executable in both runs.

use harness_core::hook::missing_hook_binaries;
use serde_json::json;

fn settings_with(command: &str) -> serde_json::Value {
    json!({
        "hooks": {
            "PreToolUse": [
                { "hooks": [ { "type": "command", "command": command } ] }
            ]
        }
    })
}

/// An existing script run through a bare interpreter name is NOT missing.
#[test]
fn bash_wrapped_existing_script_is_not_reported_missing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("guard.sh");
    std::fs::write(&script, "#!/bin/sh\nexit 0\n").expect("write script");
    let cmd = format!("bash {}", script.display());

    let missing = missing_hook_binaries(&settings_with(&cmd), None);
    assert!(
        missing.is_empty(),
        "existing script wrapped in `bash` was reported missing: {missing:?}"
    );
}

/// An absent script run through a bare interpreter name IS missing, and the
/// reported path is the script, not the interpreter.
#[test]
fn bash_wrapped_absent_script_is_reported_with_script_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("gone.sh");
    let cmd = format!("bash {}", script.display());

    let missing = missing_hook_binaries(&settings_with(&cmd), None);
    assert_eq!(missing.len(), 1, "absent script not reported: {missing:?}");
    assert_eq!(missing[0].binary_path, script.display().to_string());
}
