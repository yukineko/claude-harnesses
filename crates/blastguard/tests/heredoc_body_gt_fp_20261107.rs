// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! False positives seen in subagents: a `>` inside a quoted-heredoc body is
//! data, not a redirect.
//!
//! 1. `cd <dir> && git add -A && git commit -q -F - <<'EOF'\n<msg with -> and
//!    a "> quoted" line>\nEOF` was Deny ("'> EOF' is a RELATIVE redirect after
//!    an earlier cd ...").
//! 2. `python3 - <<'EOF'\n...s.replace("fn x() -> Decision", ...)...\nEOF` was
//!    Deny/Ask ("the program text contains a `>` that a shell would read as a
//!    redirect ...").
//!
//! `target_*`: must be Allow (ignored while RED). `control_*`: a real redirect
//! must stay restrictive. Each command runs as the main agent AND as a
//! subagent through the real binary. Allow is exactly empty stdout. `{DIR}` in
//! a command is replaced by the fixture's absolute repo path.

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
            .join("heredoc_body_gt_fp_20261107")
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
            session: format!("hd-gt-{name}-{}", std::process::id()),
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
            payload["agent_id"] = serde_json::json!("agent-hd-gt");
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

    fn assert_allow_both(&self, command: &str) {
        for sub in [false, true] {
            let d = self.decision(command, sub);
            assert_eq!(
                d, "allow",
                "`{command}` (subagent={sub}) only has `>` inside a quoted heredoc body \
                 (data, not a redirect) and must be Allow, got {d:?}"
            );
        }
    }

    fn assert_deny_both(&self, command: &str) {
        for sub in [false, true] {
            let d = self.decision(command, sub);
            assert_eq!(
                d, "deny",
                "`{command}` (subagent={sub}) has a real redirect and must stay Deny, got {d:?}"
            );
        }
    }

    fn assert_not_allow_both(&self, command: &str) {
        for sub in [false, true] {
            let d = self.decision(command, sub);
            assert_ne!(
                d, "allow",
                "`{command}` (subagent={sub}) runs a real redirect to /etc/hosts and must not be Allow"
            );
        }
    }
}

macro_rules! target {
    ($name:ident, $cmd:expr) => {
        #[test]
        #[ignore = "RED: quoted heredoc body > read as redirect"]
        fn $name() {
            Fixture::new(stringify!($name)).assert_allow_both($cmd);
        }
    };
}

macro_rules! control {
    ($name:ident, $check:ident, $cmd:expr) => {
        #[test]
        fn $name() {
            Fixture::new(stringify!($name)).$check($cmd);
        }
    };
}

// ---- FP 1: git commit -F - heredoc after a cd whose target is a runtime value ----
// (a cd to a literal absolute dir with the same body is already Allow.)

target!(
    target_cd_var_git_commit_heredoc,
    "WT=/private/tmp/hdgt-wt; cd \"$WT\" && git add -A && git commit -q -F - <<'EOF'\nfix: a -> b\n\n> quoted\nEOF"
);
target!(
    target_cd_subst_git_commit_heredoc,
    "cd \"$(git rev-parse --show-toplevel)\" && git commit -q -F - <<'EOF'\nfix: a -> b\n\n> quoted\nEOF"
);

// ---- FP 2: python heredoc with `>` in program text ----
// (`->` in a string, `a > b` with spaces, and open(p,'w').write(s) are
// already Allow; the unspaced `a>b` / bare '>' form is the RED one.)

target!(
    target_python_gt_no_space,
    "python3 - <<'EOF'\na = 3\nb = 2\nprint(a>b, \"x->y\", '>')\nEOF"
);

// ---- controls: same commands without any `>` in the body are Allow today,
// so the `>` is the sole cause of the targets' verdicts ----

control!(
    control_nogt_cd_var_git_commit_heredoc,
    assert_allow_both,
    "WT=/private/tmp/hdgt-wt; cd \"$WT\" && git commit -q -F - <<'EOF'\nfix: a to b\nEOF"
);
control!(
    control_nogt_cd_subst_git_commit_heredoc,
    assert_allow_both,
    "cd \"$(git rev-parse --show-toplevel)\" && git commit -q -F - <<'EOF'\nfix: a to b\nEOF"
);
control!(
    control_nogt_python,
    assert_allow_both,
    "python3 - <<'EOF'\na = 3\nb = 2\nprint(a, \"x to y\")\nEOF"
);

// ---- controls: a real redirect stays restrictive ----

control!(
    control_real_redirect_after_heredoc_etc_hosts,
    assert_deny_both,
    "cat <<'EOF' > /etc/hosts\nx\nEOF"
);
control!(
    control_python_os_system_redirect_etc_hosts,
    assert_not_allow_both,
    "python3 - <<'EOF'\nimport os\nos.system('echo x > /etc/hosts')\nEOF"
);
