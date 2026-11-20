#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (false positive): a `.rs` heredoc whose `//!` doc line contains
//! destructive-looking text is read as shell when a `sed -i` line follows.
//!
//! Observed 2026-11-20 (main, blastguard 0.2.784): with body line
//! `//! deny :: sudo bash -c 'rm -rf /etc'` followed by
//! `sed -i '' 's/a/b/' Cargo.toml` the decision is ask with reason
//! "`//!` is not a command blastguard recognises ...". The same heredoc alone,
//! a heredoc with a plain doc line + sed -i, and a heredoc + grep are allow.
//!
//! Written by an independent test author.

#[path = "support/bounded.rs"]
mod bounded;

use std::process::{Command, Stdio};

struct Home(std::path::PathBuf);
impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Home dir containing a git-initialised project `proj`, cwd = proj.
fn unique_home(tag: &str) -> Home {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "bg-rsheredoc-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let proj = dir.join("proj");
    std::fs::create_dir_all(proj.join("tests")).unwrap();
    std::fs::write(proj.join("Cargo.toml"), "[package]\nname = \"a\"\n").unwrap();
    let st = Command::new("git")
        .arg("init")
        .arg("-q")
        .arg(&proj)
        .status()
        .expect("git init must be runnable");
    assert!(st.success(), "git init failed");
    Home(dir)
}

fn run_as(command: &str, home: &Home) -> (String, String) {
    let p = serde_json::json!({
        "session_id": "rs-heredoc-doc-sed-fp-probe-20261120",
        "cwd": home.0.join("proj"),
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    });
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

fn assert_decision(tag: &str, command: &str, want: &str) {
    let home = unique_home(tag);
    let (d, r) = run_as(command, &home);
    assert_eq!(
        d, want,
        "{command:?} must be {want}, got decision={d} reason={r}"
    );
}

const HEREDOC_DANGEROUS_DOC: &str =
    "cat > tests/a.rs <<'EOF'\n//! deny :: sudo bash -c 'rm -rf /etc'\nfn main() {}\nEOF";
const HEREDOC_PLAIN_DOC: &str = "cat > tests/a.rs <<'EOF'\n//! plain doc line\nfn main() {}\nEOF";
const SED: &str = "sed -i '' 's/a/b/' Cargo.toml";

const IGNORE_MSG: &str = "RED: blastguard FP 2026-11-20 .rs heredoc body read as shell when a sed -i line follows — remove the ignore when fixed";

#[test]
#[ignore = "RED: blastguard FP 2026-11-20 .rs heredoc body read as shell when a sed -i line follows — remove the ignore when fixed"]
fn rs_heredoc_dangerous_doc_then_sed_is_allow() {
    let _ = IGNORE_MSG;
    assert_decision("r1", &format!("{HEREDOC_DANGEROUS_DOC}\n{SED}"), "allow");
}

#[test]
#[ignore = "RED: blastguard FP 2026-11-20 .rs heredoc body read as shell when a sed -i line follows — remove the ignore when fixed"]
fn cd_prefixed_rs_heredoc_dangerous_doc_then_sed_is_allow() {
    let home = unique_home("r2");
    let proj = home.0.join("proj");
    let cmd = format!("cd {} && {HEREDOC_DANGEROUS_DOC}\n{SED}", proj.display());
    let (d, r) = run_as(&cmd, &home);
    assert_eq!(
        d, "allow",
        "{cmd:?} must be allow, got decision={d} reason={r}"
    );
}

#[test]
fn ctl_dangerous_doc_heredoc_alone_is_allow() {
    assert_decision("c1", HEREDOC_DANGEROUS_DOC, "allow");
}

#[test]
fn ctl_plain_doc_heredoc_then_sed_is_allow() {
    assert_decision("c2", &format!("{HEREDOC_PLAIN_DOC}\n{SED}"), "allow");
}

#[test]
fn ctl_real_destructive_after_sed_is_deny() {
    assert_decision("c3", &format!("{SED}; rm -rf /etc"), "deny");
}
