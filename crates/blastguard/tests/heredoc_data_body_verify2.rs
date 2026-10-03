// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Second independent verifier test for backlog fa1fce21 (condukt verifier,
//! not the implementing worker), against the rework f3bb1be9.
//!
//! Contract: a here-document body fed to a data-only consumer is data for
//! EVERY rule, so a body that merely describes a destructive command is
//! allowed; the same text as a real command line, or a body that the shell
//! (or a later command on the same line) will execute, stays judged.
//!
//! `contract_*` rows are the task's done_criteria. `smuggle_*` rows are
//! commands the verifier found where the stripper removes text that bash
//! really executes. Every `smuggle_*` command was checked in real bash: the
//! "body" line runs.

use std::io::Write;
use std::process::{Command, Stdio};

const RM: &str = "rm -rf /usr/lib";

/// Non-interactive session (an Ask hardens to Deny), isolated HOME.
fn verdict(command: &str) -> String {
    let home = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("heredoc_data_body_verify2");
    std::fs::create_dir_all(&home).unwrap();
    let payload = serde_json::json!({
        "session_id": "heredoc-data-body-verify2",
        "cwd": "/private/tmp",
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    })
    .to_string();
    let mut child = Command::new(env!("CARGO_BIN_EXE_blastguard"))
        .env_clear()
        .env("HOME", &home)
        .env("PATH", "/usr/bin:/bin")
        .env("CLAUDECODE", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "hook must exit 0 for {command:?}"
    );
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if stdout.is_empty() {
        return "allow".into();
    }
    let doc: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    doc["hookSpecificOutput"]["permissionDecision"]
        .as_str()
        .unwrap()
        .to_string()
}

fn assert_all(rows: &[String], want_allow: bool) {
    let wrong: Vec<(String, String)> = rows
        .iter()
        .map(|c| (c.clone(), verdict(c)))
        .filter(|(_, d)| (d == "allow") != want_allow)
        .collect();
    assert!(
        wrong.is_empty(),
        "expected {} for every row; wrong: {wrong:#?}",
        if want_allow { "allow" } else { "not-allow" }
    );
}

/// The mentioned-elsewhere rule is a literal substring test on the basename,
/// so any spelling of the path that bash resolves to the same file without
/// containing that substring runs the body. Each was run in real bash (the
/// body `echo RAN` printed): glob, `?`, `[t]`, quote splitting, empty quotes,
/// backslash, a variable prefix, and upper case on the case-insensitive
/// default macOS filesystem.
#[test]
fn smuggle_written_file_reached_by_another_spelling() {
    let w = format!("cat <<'EOF' > x.txt\n{RM}\nEOF\n");
    let rows: Vec<String> = [
        "sh *.txt",
        "sh x.t?t",
        "sh x.[t]xt",
        "sh \"x\"\".txt\"",
        "sh x''.txt",
        "sh x\\.txt",
        "f=x; sh $f.txt",
        "sh X.TXT",
        "for f in *.txt; do sh \"$f\"; done",
        "cat *.txt | sh",
        "sh ~+/x*",
    ]
    .iter()
    .map(|tail| format!("{w}{tail}"))
    .collect();
    assert_all(&rows, false);
}

/// The redefinition check matches literal `cat()` / keywords, so a function
/// defined through `eval` with a split name, or by sourcing a file, is missed.
/// Both run the body through `sh` in real bash.
#[test]
fn smuggle_consumer_redefined_out_of_sight() {
    let rows = [
        format!("eval 'ca''t() {{ sh; }}'\ncat <<'EOF'\n{RM}\nEOF"),
        format!(". ./defs.sh\ncat <<'EOF'\n{RM}\nEOF"),
        format!("source ./defs.sh\ncat <<'EOF'\n{RM}\nEOF"),
    ];
    assert_all(&rows, false);
}

/// Non-regression for the rework: the data contract still holds.
#[test]
fn contract_data_body_still_allowed() {
    assert_all(
        &[
            format!("cat <<'EOF' > notes.txt\n{RM}\nEOF"),
            format!("git commit -F - <<'EOF'\nfix: do not {RM}\nEOF"),
        ],
        true,
    );
}
