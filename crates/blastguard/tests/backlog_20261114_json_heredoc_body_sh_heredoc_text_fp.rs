#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (false positive, observed 2026-11-14 at main b823ffa5 in a subagent, run
//! run-20261114-130020-46995): a quoted heredoc that writes a `.json` file is
//! denied because its body — JSON data — contains the TEXT `sh <<'EOF'`, which
//! is read as a real shell taking its program from stdin:
//! "`sh <<'EOF'` takes its program from stdin, and that program is not on the
//! command line — blastguard has no text to read, so it cannot tell whether
//! anything here is irreversible".
//!
//! The observed command was `cat > <scratch>/decomp.json <<'JSON'` with a
//! condukt decomposition whose done_criteria string mentioned
//! "(e.g. `sh <<'EOF'`, `bash <<'EOF'`)", followed by `cd <wt>; ... condukt
//! validate ...`. The shapes below are minimised from it.
//!
//! Asserted: the data-heredoc shapes are Allow in subagent mode.
//! Controls (GREEN at HEAD):
//!  - the same heredoc whose body has no `sh <<` text is Allow;
//!  - a REAL `sh <<'EOF'` whose program removes a system dir is not Allow.
//!
//! DOES NOT PROVE: anything about main (non-subagent) mode, nor which verdict
//! the real `sh` control should get beyond not-Allow.

#[path = "support/bounded.rs"]
mod bounded;

use std::process::{Command, Stdio};

struct Home(std::path::PathBuf);
impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn unique_home(tag: &str) -> Home {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "bg-jsonsh-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "json-heredoc-sh-text-fp-20261114",
        "cwd": home.0.join("wt"),
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    });
    if subagent {
        p["agent_id"] = serde_json::json!("test-subagent");
    }
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    cmd.env_clear()
        .env("HOME", &home.0)
        .env("PATH", "/usr/bin:/bin")
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut cmd, p.to_string().as_bytes(), bounded::HOOK_DEADLINE);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return ("allow".into(), String::new());
    }
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let h = &v["hookSpecificOutput"];
    (
        h["permissionDecision"].as_str().unwrap_or("?").to_string(),
        h["permissionDecisionReason"]
            .as_str()
            .unwrap_or("")
            .to_string(),
    )
}

/// Quoted heredoc writing JSON data whose string mentions `sh <<'EOF'`.
const JSON_DATA_SHAPES: &[&str] = &[
    "cat > /tmp/bg-jsonsh-d.json <<'JSON'\n{\"d\":\"a body a shell runs (e.g. `sh <<'EOF'`, `bash <<'EOF'`) stays not-Allow\"}\nJSON\necho done",
    "cat > /tmp/bg-jsonsh-d.json <<'JSON'\n{\"d\":\"e.g. sh <<'EOF' here\"}\nJSON\necho done",
];

// test-weakening-justified: new RED for an unfixed defect (side finding of run
// run-20261114-130020-46995, 2026-11-14); the ignore is removed in the fixing commit.
#[test]
#[ignore = "RED: 2026-11-14 `sh <<'EOF'` text inside a quoted .json heredoc body is read as a real stdin-program shell — remove the ignore when fixed"]
fn json_heredoc_body_with_sh_heredoc_text_is_allowed() {
    let home = unique_home("red");
    let mut failures = Vec::new();
    for c in JSON_DATA_SHAPES {
        let (d, r) = run_as(c, &home, true);
        if d != "allow" {
            failures.push(format!("{c:?} -> {d} | {r}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} json-data heredoc command(s) were not Allow:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

// Control 1: the same heredoc without the `sh <<` text is Allow.
#[test]
fn control_json_heredoc_without_sh_text_is_allowed() {
    let home = unique_home("ctl1");
    let c = "cat > /tmp/bg-jsonsh-d.json <<'JSON'\n{\"d\":\"plain data\"}\nJSON\necho done";
    let (d, r) = run_as(c, &home, true);
    assert_eq!(d, "allow", "{c:?} -> {d} | {r}");
}

// Control 2: a REAL shell reading a destructive program from a heredoc is not Allow.
#[test]
fn control_real_sh_heredoc_program_is_not_allowed() {
    let home = unique_home("ctl2");
    let c = "sh <<'EOF'\nrm -rf /usr/lib\nEOF";
    let (d, r) = run_as(c, &home, true);
    assert_ne!(d, "allow", "{c:?} -> {d} | {r}");
}
