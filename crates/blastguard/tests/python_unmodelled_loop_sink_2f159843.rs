#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Backlog 2f159843 (596ef57d R-python-unknown-sink, ruling A2a): a Python sink
//! with no effect model — the asyncio event-loop methods `subprocess_shell` /
//! `subprocess_exec`, reached as an attribute of any object — resolves to Ask
//! (subagent: deny), or Deny where the program also carries a system-harm
//! command string, never Allow, in every Python reading (stdin here-document,
//! `-c`, written `.py` file). "Unknown program = Allow" does not apply to a
//! Python sink.
//!
//! Controls: a program that only MENTIONS the name inside a string literal or a
//! comment keeps the decision it had before the change (measured on the base,
//! 2026-11-21: allow in main and subagent mode).

#[path = "support/git_env.rs"]
mod git_env;
#[path = "support/neutral_base.rs"]
mod neutral_base;

#[path = "support/bounded.rs"]
mod bounded;

use std::path::{Path, PathBuf};
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
    assert!(out.status.success(), "git {args:?} failed");
}

struct Fixture {
    base: PathBuf,
    home: PathBuf,
    repo: PathBuf,
    session: String,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let base = neutral_base::neutral_base()
            .join("python_unmodelled_loop_sink_2f159843")
            .join(name);
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        let repo = base.join("src/repo");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        std::fs::write(repo.join("f.txt"), "x\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "init"]);
        Fixture {
            base,
            home,
            repo,
            session: format!("u2f1-{name}-{}", std::process::id()),
        }
    }

    /// The permission decision for `command`: "allow" when blastguard prints nothing.
    fn decision(&self, command: &str, subagent: bool) -> String {
        let command = command.replace("{DIR}", &self.repo.display().to_string());
        let mut payload = serde_json::json!({
            "session_id": format!("{}-{}", self.session, if subagent { "sub" } else { "main" }),
            "hook_event_name": "PreToolUse",
            "cwd": self.repo.display().to_string(),
            "tool_name": "Bash",
            "tool_input": { "command": command },
        });
        if subagent {
            payload["agent_id"] = serde_json::json!("agent-u2f1");
            payload["agent_type"] = serde_json::json!("general-purpose");
        }
        let session = payload["session_id"].as_str().unwrap().to_string();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
        git_env::scrub(&mut cmd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("CLAUDE_PROJECT_DIR", &self.repo)
            .env("BLASTGUARD_APPROVALS_DIR", self.base.join("store"))
            .env("HOME", &self.home)
            .env("CLAUDE_CODE_ENTRYPOINT", "cli")
            .env("CLAUDE_CODE_SESSION_ID", &session)
            .env_remove("BLASTGUARD_ASK")
            .env_remove("TMPDIR")
            .current_dir(&self.base);
        let out = bounded::run_bounded(
            &mut cmd,
            payload.to_string().as_bytes(),
            bounded::HOOK_DEADLINE,
        );
        let out = String::from_utf8_lossy(&out.stdout).into_owned();
        if out.trim().is_empty() {
            return "allow".to_string();
        }
        let v = serde_json::from_str::<serde_json::Value>(out.trim()).unwrap();
        v.pointer("/hookSpecificOutput/permissionDecision")
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_else(|| "nodecision".into())
    }
}

fn stdin(b: &str) -> String {
    format!("python3 - <<'PY'\n{b}\nPY")
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Need {
    /// main = ask (the unmodelled sink is an Ask, not a Deny) and sub != allow.
    Ask,
    /// main != allow and sub != allow. Used where the program also carries a
    /// whole shell command string that blastguard judges as system harm
    /// (`"rm -rf /usr/lib"`): there Deny (stricter than Ask) is observed and
    /// correct; the requirement (A2a) is only "not Allow".
    NotAllow,
    /// allow in main and subagent mode.
    Allow,
}

fn check(cases: &[(&str, String, Need)]) {
    let mut bad: Vec<String> = Vec::new();
    for (name, cmd, need) in cases {
        let f = Fixture::new(name);
        let main = f.decision(cmd, false);
        let sub = f.decision(cmd, true);
        eprintln!("{name}: main={main} sub={sub} need={need:?}");
        let ok = match need {
            Need::Ask => main == "ask" && sub != "allow",
            Need::NotAllow => main != "allow" && sub != "allow",
            Need::Allow => main == "allow" && sub == "allow",
        };
        if !ok {
            bad.push(format!("{name}: main={main} sub={sub} need={need:?}"));
        }
    }
    assert!(
        bad.is_empty(),
        "cases on the wrong side:\n{}",
        bad.join("\n")
    );
}

#[test]
fn unmodelled_loop_sinks_ask() {
    check(&[
        (
            "stdin_get_event_loop_subprocess_shell",
            stdin("import asyncio\nasyncio.get_event_loop().subprocess_shell(None, \"rm -rf /usr/lib\")"),
            Need::NotAllow,
        ),
        (
            "stdin_loop_subprocess_exec",
            stdin("import asyncio\nloop = asyncio.new_event_loop()\nloop.subprocess_exec(None, \"rm\", \"-rf\", \"/usr/lib\")"),
            Need::Ask,
        ),
        (
            // No effect model: even a harmless-looking argument is not vouched for.
            "stdin_loop_subprocess_exec_ls",
            stdin("import asyncio\nloop = asyncio.new_event_loop()\nloop.subprocess_exec(None, \"ls\")"),
            Need::Ask,
        ),
        (
            "inline_c_subprocess_shell",
            "python3 -c 'import asyncio; asyncio.get_event_loop().subprocess_shell(None, \"rm -rf /usr/lib\")'".to_string(),
            Need::NotAllow,
        ),
        (
            "written_file_subprocess_exec",
            "cat > {DIR}/x.py <<'EOF'\nimport asyncio\nloop = asyncio.new_event_loop()\nloop.subprocess_exec(None, \"rm\", \"-rf\", \"/usr/lib\")\nEOF\npython3 {DIR}/x.py".to_string(),
            Need::Ask,
        ),
    ]);
}

#[test]
fn mentions_in_strings_and_comments_keep_their_decision() {
    check(&[
        (
            "stdin_string_mention",
            stdin("print(\"loop.subprocess_shell\")"),
            Need::Allow,
        ),
        (
            "stdin_comment_mention",
            stdin("# loop.subprocess_shell(None, \"ls\")\nprint(1)"),
            Need::Allow,
        ),
        (
            "inline_c_string_mention",
            "python3 -c 'print(\"loop.subprocess_exec\")'".to_string(),
            Need::Allow,
        ),
    ]);
}
