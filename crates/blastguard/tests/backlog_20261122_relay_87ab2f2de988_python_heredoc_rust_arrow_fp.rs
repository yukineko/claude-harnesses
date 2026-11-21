//! RED (false positive; verbatim relay replay; independent test author,
//! 2026-11-22). Observed in condukt run run-20261122-035251-84424 (t2 worker,
//! subagent), relay bgr-87ab2f2de988 (Deny), on the blastguard then running
//! (pre-5e99d94d4). The command was
//!   cd <task worktree> && python3 -I - <<'EOF'
//!   p='crates/condukt/src/testscope.rs' ... s=open(p).read() ... rep("""...""",
//!   """...""") with Rust source in the literals ... open(p,'w').write(s)
//!   EOF
//! Verbatim reason: "> target `<task worktree>/Ok(*b` is a glob the shell
//! expands to the files it matches, and blastguard cannot place it ..." -- the
//! `=> Ok(*b),` of the Rust line `Some(toml::Value::Boolean(b)) => Ok(*b),`
//! inside a Python ''' literal of a QUOTED heredoc was read as a shell
//! redirect.
//!
//! Fixture `support/relay_bgr_87ab2f2de988_20261122_exact.txt` is the exact
//! relayed command; the only rewrite is the `cd` target -> this test's cwd.
//!
//! Readings: (a) FP -- the heredoc is quoted (nothing in it is shell text) and
//! the program is stdin Python whose literals are data (backlog 8ff44387);
//! (b) intended -- some construct of the program is outside the supported
//! Python subset, so Ask/Deny is "undetermined". Even under (b) the reason
//! (a shell `>` glob target inside Python literal text) misnames the class.
//! Tell apart: the control (same write without the Rust-bearing literals).
//!
//! DOES NOT PROVE: which part of the body triggers it (not minimised); real
//! Python execution.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/git_env.rs"]
mod git_env;

use std::path::Path;
use std::process::{Command, Stdio};

fn git(dir: &Path, args: &[&str]) {
    let mut cmd = git_env::pinned(dir);
    cmd.args([
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@example.invalid",
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "init.defaultBranch=main",
    ])
    .args(args)
    .env("GIT_CONFIG_GLOBAL", "/dev/null")
    .env("GIT_CONFIG_NOSYSTEM", "1");
    let out =
        bounded::run_bounded_stdin(&mut cmd, bounded::ChildStdin::Null, bounded::TOOL_DEADLINE);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

const EXACT: &str = include_str!("support/relay_bgr_87ab2f2de988_20261122_exact.txt");
const ORIG_CD: &str = "/Users/yuki/.condukt/worktrees/run-20261122-035251-84424-t2";

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
        "bg-relay87ab2-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let src = dir.join("wt/crates/condukt/src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("testscope.rs"), "// fixture\n").unwrap();
    // The real cwd was a condukt task worktree: a git work tree in which the
    // edited file is tracked.
    let wt = dir.join("wt");
    git(&wt, &["init", "-q"]);
    git(&wt, &["add", "crates/condukt/src/testscope.rs"]);
    git(&wt, &["commit", "-q", "-m", "fixture"]);
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "relay-87ab2f2de988-20261122",
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

fn unallowed(cmd: &str, tag: &str) -> Vec<String> {
    let home = unique_home(tag);
    let wt = home.0.join("wt");
    let cmd = cmd.replace(ORIG_CD, wt.to_str().unwrap());
    let mut bad = Vec::new();
    for subagent in [false, true] {
        let (d, r) = run_as(&cmd, &home, subagent);
        if d != "allow" {
            bad.push(format!("subagent={subagent} => {d} ({r})"));
        }
    }
    bad
}

#[test]
#[ignore = "RED(1b84867e): relay bgr-87ab2f2de988 — `>` inside Python ''' literal text of a quoted python3 -I - heredoc is read as a shell redirect (Ask/Deny)"]
fn relay_87ab2f2de988_exact_command_is_allow() {
    assert!(EXACT.contains(ORIG_CD), "fixture: cd target present");
    let bad = unallowed(EXACT.trim_end_matches('\n'), "exact");
    assert!(
        bad.is_empty(),
        "expected Allow for the verbatim relayed command:\n{}",
        bad.join("\n")
    );
}

#[test]
fn control_same_write_without_rust_literals_is_allow() {
    let cmd = format!(
        "cd {ORIG_CD} && python3 -I - <<'EOF'\np='crates/condukt/src/testscope.rs'\ns=open(p).read()\nopen(p,'w').write(s)\nEOF"
    );
    let bad = unallowed(&cmd, "ctl");
    assert!(
        bad.is_empty(),
        "control expected Allow:\n{}",
        bad.join("\n")
    );
}
