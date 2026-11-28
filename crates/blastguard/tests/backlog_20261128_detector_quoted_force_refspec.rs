#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! RED: a quoted force refspec (`git push origin "+x"`) is weaker than the
//! unquoted one (`git push origin +x`).
//!
//! A leading `+` on a push refspec means "force this ref" (git-push(1):
//! "By having the optional leading +, you can tell Git to update the
//! <dst> ref even if it is not allowed by default"). Shell quoting removes
//! the quotes before git sees the word, so `"+x"`, `'+x'`, `\+x` and `+x`
//! are the same argv. The unquoted form is Ask (main) / Deny (subagent);
//! the quoted forms must not be weaker.
//!
//! Controls (GREEN): unquoted `+x` is Ask / Deny; a plain push (quoted or
//! not) is Allow; `rm -rf /etc` is Deny (the hook is live in this harness).
//!
//! Harness: payload `cwd` and hook `HOME` point at a fixture under
//! `/private/tmp/claude-askrules/` (not `/var/folders`, which is a system
//! directory for blastguard), with a git repo on `main` as the cwd.
//!
//! DOES NOT PROVE: which internal stage drops the quoted `+`; remotes other
//! than `origin`; `--force-with-lease` spellings.
#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/git_env.rs"]
mod git_env;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn base() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let b = PathBuf::from(format!(
        "/private/tmp/claude-askrules/detector-quoted-force-{}-{nanos}-{}",
        std::process::id(),
        // libtest names each test thread after its test: unique per test.
        std::thread::current().name().unwrap_or("main")
    ));
    std::fs::create_dir_all(b.join("home")).unwrap();
    let repo = b.join("src/repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    std::fs::write(repo.join("f.txt"), "one\n").unwrap();
    git(&repo, &["add", "f.txt"]);
    git(&repo, &["commit", "-q", "-m", "one"]);
    b
}

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
        "git {args:?} failed in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `subagent=false`: interactive cli session; `subagent=true`: no human.
fn verdict(b: &Path, cmd: &str, subagent: bool) -> String {
    let repo = b.join("src/repo");
    let payload = serde_json::json!({
        "session_id": "detector-quoted-force",
        "hook_event_name": "PreToolUse",
        "cwd": repo,
        "tool_name": "Bash",
        "tool_input": { "command": cmd }
    })
    .to_string();
    let mut hook = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    git_env::scrub(&mut hook)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("HOME", b.join("home"))
        .env("CLAUDE_PROJECT_DIR", &repo)
        .env("BLASTGUARD_APPROVALS_DIR", b.join("store"))
        .env_remove("BLASTGUARD_ASK")
        .env_remove("TMPDIR")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .current_dir(b);
    if subagent {
        hook.env_remove("CLAUDECODE")
            .env_remove("CLAUDE_CODE_ENTRYPOINT");
    } else {
        hook.env("CLAUDECODE", "1")
            .env("CLAUDE_CODE_ENTRYPOINT", "cli");
    }
    let out = bounded::run_bounded(&mut hook, payload.as_bytes(), bounded::HOOK_DEADLINE);
    assert_eq!(out.status.code(), Some(0), "hook must always exit 0");
    let s = String::from_utf8_lossy(&out.stdout).into_owned();
    if s.trim().is_empty() {
        return "allow".into();
    }
    let v: serde_json::Value = serde_json::from_str(s.trim()).expect("hook JSON");
    v["hookSpecificOutput"]["permissionDecision"]
        .as_str()
        .unwrap_or_else(|| panic!("no permissionDecision: {s}"))
        .to_string()
}

fn rank(v: &str) -> u8 {
    match v {
        "allow" => 0,
        "ask" => 1,
        "deny" => 2,
        other => panic!("unknown verdict {other}"),
    }
}

#[test]
fn control_unquoted_force_refspec_is_ask_main_deny_subagent() {
    let b = base();
    for c in ["git push origin +x", "git push origin +main"] {
        assert_eq!(verdict(&b, c, false), "ask", "main: {c}");
        assert_eq!(verdict(&b, c, true), "deny", "subagent: {c}");
    }
}

#[test]
fn control_plain_push_and_system_harm() {
    let b = base();
    for c in ["git push origin x", "git push origin \"x\""] {
        assert_eq!(verdict(&b, c, false), "allow", "over-block: {c}");
    }
    assert_eq!(verdict(&b, "rm -rf /etc", false), "deny");
}

#[test]
#[ignore = "RED(005d3a94): pending-id"]
fn quoted_force_refspec_is_never_weaker_than_unquoted() {
    let b = base();
    let pairs = [
        ("git push origin +x", "git push origin \"+x\""),
        ("git push origin +x", "git push origin '+x'"),
        ("git push origin +x", "git push origin \\+x"),
        ("git push origin +main", "git push origin \"+main\""),
        (
            "git push origin +HEAD:main",
            "git push origin \"+HEAD:main\"",
        ),
    ];
    let mut bad = Vec::new();
    for (bare, quoted) in pairs {
        for sub in [false, true] {
            let (bv, qv) = (verdict(&b, bare, sub), verdict(&b, quoted, sub));
            if rank(&qv) < rank(&bv) {
                bad.push(format!(
                    "{} unquoted={bv} quoted={qv}: {quoted}",
                    if sub { "subagent" } else { "main" }
                ));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "{} weaker than unquoted:\n{}",
        bad.len(),
        bad.join("\n")
    );
}
