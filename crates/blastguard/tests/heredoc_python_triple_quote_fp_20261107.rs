// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! False positive observed 2026-11-07 in session bf349dd5: a subagent's
//! `python3 - <<'EOF'` edit of crates/backlog was Ask with "the program text
//! contains a `>` that a shell would read as a redirect whose target cannot
//! be determined". Every `>` in it is a `=>` inside a python `"""..."""`
//! literal. The target command is the verbatim one minimized line by line
//! while keeping the body valid Python and keeping the verdict.
//!
//! `target_*` must be Allow; `control_*` keeps a real redirect restrictive.
//! Each command runs as the main agent AND as a subagent through the real
//! binary. Allow is exactly empty stdout. `{DIR}` is the fixture repo path.
//! (Same harness as heredoc_body_gt_fp_20261107.rs.)

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
    assert!(
        out.status.success(),
        "git {args:?} failed in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
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
            .join("heredoc_python_triple_quote_fp_20261107")
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
            session: format!("hd-tq-{name}-{}", std::process::id()),
        }
    }

    /// `"allow"` for empty stdout, else the emitted `permissionDecision`.
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
            payload["agent_id"] = serde_json::json!("agent-hd-tq");
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
            .env("CLAUDECODE", "1")
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
        assert_eq!(
            out.status.code(),
            Some(0),
            "hook must exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let out = String::from_utf8_lossy(&out.stdout).into_owned();
        if out.trim().is_empty() {
            return "allow".to_string();
        }
        serde_json::from_str::<serde_json::Value>(out.trim())
            .unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {out}"))
            .pointer("/hookSpecificOutput/permissionDecision")
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_else(|| panic!("no permissionDecision in {out}"))
    }
}

#[test]
fn target_python_triple_quoted_fat_arrow() {
    let f = Fixture::new("target_python_triple_quoted_fat_arrow");
    let command = "cd {DIR} && python3 - <<'EOF'\ns=s.replace(\"\"\"    Claimed,\n\"\"\",1)\ns=s.replace(\"\"\"    /// Every status, in lifecycle order (the derived `Claimed` last).\n    ];\"\"\",1)\ns=s.replace(\"\"\"            Status::Claimed => \"claimed\",\n\"\"\",1)\nEOF";
    for sub in [false, true] {
        let d = f.decision(command, sub);
        assert_eq!(
            d, "allow",
            "`{command}` (subagent={sub}): every `>` is a `=>` inside a python \
             triple-quoted literal in a quoted heredoc (data, not a redirect); \
             must be Allow, got {d:?}"
        );
    }
}

/// Control: the same shape with a real shell redirect to /etc/hosts after
/// the heredoc must not be Allow.
#[test]
fn control_same_shape_with_real_redirect_is_not_allow() {
    let f = Fixture::new("control_same_shape_with_real_redirect_is_not_allow");
    let command = "cd {DIR} && python3 - <<'EOF' > /etc/hosts\ns=\"\"\"            Status::Claimed => \"claimed\",\n\"\"\"\nprint(s)\nEOF";
    for sub in [false, true] {
        let d = f.decision(command, sub);
        assert_ne!(
            d, "allow",
            "`{command}` (subagent={sub}) redirects to /etc/hosts and must not be Allow"
        );
    }
}
