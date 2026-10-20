#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog 4a1cf1ad: a backtick-quoted `<...>/claims.json` inside a QUOTED
//! python heredoc body was DENIED as a `> /claims.json` redirect. Under user
//! ruling R2 (2026-10-14, see heredoc_quoted_body_redirect_false_positive.rs)
//! program-text `>` must be at most Ask, never Deny.

use std::io::Write;
use std::process::{Command, Stdio};

fn verdict(command: &str) -> (String, String) {
    let bin = env!("CARGO_BIN_EXE_blastguard");
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": command },
    })
    .to_string();
    let mut child = Command::new(bin)
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary spawns");
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(payload.as_bytes());
    }
    let out = child.wait_with_output().expect("binary runs");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return ("allow".to_string(), String::new());
    }
    let doc: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let hso = &doc["hookSpecificOutput"];
    (
        hso["permissionDecision"]
            .as_str()
            .unwrap_or("?")
            .to_string(),
        hso["permissionDecisionReason"]
            .as_str()
            .unwrap_or("")
            .to_string(),
    )
}

/// Measured at 0.2.127: the deny fires when the backticked span in a python
/// COMMENT contains `)>` (e.g. `<k(r)>/claims.json`); the same text inside a
/// string literal gets Ask, and `<k>/claims.json` is allowed.
/// Restrictive controls for the fix (`interpreter_program_body_segments`):
/// the downgrade is Ask carrying the R2 marker, never Allow; and it does not
/// reach a body the SHELL parses (unquoted delimiter, shell consumer), code
/// after the closing delimiter, or a definite target.
#[test]
fn controls_downgrade_is_ask_and_stays_inside_interpreter_program_text() {
    let downgraded = "python3 - <<'PY'\n# writes `<k(r)>/claims.json`\nprint(1)\nPY";
    let (d, r) = verdict(downgraded);
    assert_eq!(d, "ask", "downgraded case must be Ask, not {d}: {r}");
    assert!(
        r.contains("Recommended: do not run this."),
        "the Ask must carry the R2 marker: {r}"
    );
    for cmd in [
        "python3 - <<PY\n# writes `<k(r)>/claims.json`\nPY",
        "bash <<'EOF'\necho hi > `x`\nEOF",
        "python3 - <<'PY'\nprint(1)\nPY\necho hi > `x`",
        "python3 - <<'PY'\n# writes `<k(r)>/claims.json`\nPY\necho hi >/claims.json`x`",
        "python3 - <<'PY'\n# a(b)> /etc/hosts\nPY",
        // No closing delimiter line: where the body ends was not observed, so
        // nothing is downgraded (restrictive on doubt).
        "python3 - <<'PY'\n# writes `<k(r)>/claims.json`",
        // A `;` after the opener: the rest of that LINE is shell code.
        "python3 - <<'PY'; echo hi > `x`\nprint(1)\nPY",
    ] {
        let (d, r) = verdict(cmd);
        assert_eq!(
            d,
            "deny",
            "{:?} must stay Deny; reason={r}",
            cmd.replace('\n', "\\n")
        );
    }
}

#[test]
fn backtick_claims_json_in_quoted_python_heredoc_is_not_denied() {
    let cases = [
        "python3 - <<'EOF'\n# state lives in `<state_dir>/claims.json`\nprint(1)\nEOF",
        "python3 - <<'EOF'\ns = '''/// `<project_key(main_worktree_root(root))>/claims.json`'''\nprint(len(s))\nEOF",
        "python3 - <<'PY'\nimport pathlib\n# writes `<project_key(main_worktree_root(root))>/claims.json`\np = pathlib.Path('x')\nPY",
        "python3 - <<'PY'\n# writes `<project_key(main_worktree_root(root))>/claims.json`\nprint(1)\nPY",
        "python3 - <<'EOF'\nimport pathlib\n# writes `<k>/claims.json`\nprint(1)\nEOF",
        "python3 - <<'EOF'\n# writes `<k>/claims.json`\np = pathlib.Path('x')\nEOF",
        "python3 - <<'EOF'\n# writes `<k(r)>/claims.json`\nprint(1)\nEOF",
    ];
    let mut bad = Vec::new();
    for c in cases {
        let (d, r) = verdict(c);
        eprintln!("{d} :: {r} :: {:?}", c.replace('\n', "\\n"));
        if d == "deny" {
            bad.push(format!("{d}: {r} for {:?}", c.replace('\n', "\\n")));
        }
    }
    assert!(bad.is_empty(), "denied:\n{}", bad.join("\n"));
}
