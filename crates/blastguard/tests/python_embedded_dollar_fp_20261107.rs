// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! False positive on fix/bg-harm: ordinary Python heredoc shell strings that
//! merely contain `$VAR` (`os.system('echo "$HOME"')`, ...) became Ask (main
//! agent) / Deny (subagent); on base 8fb4e3a9 they were Allow. Worst case:
//! `subprocess.run(['echo', '$HOME'])` (argv list, never reaches a shell).
//!
//! `target_*`: must be Allow (RED on the branch). `control_*`: genuinely
//! destructive embedded commands that must stay restrictive (pass now). Each
//! command is driven as the main agent AND as a subagent (`agent_id` set),
//! through the real binary. Allow is exactly empty stdout.

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
            .join("python_embedded_dollar_fp_20261107")
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
            session: format!("py-dollar-{name}-{}", std::process::id()),
        }
    }

    /// `"allow"` for empty stdout, else the emitted `permissionDecision`.
    fn decision(&self, command: &str, subagent: bool) -> String {
        let mut payload = serde_json::json!({
            "session_id": format!("{}-{}", self.session, if subagent { "sub" } else { "main" }),
            "hook_event_name": "PreToolUse",
            "cwd": self.repo.display().to_string(),
            "tool_name": "Bash",
            "tool_input": { "command": command },
        });
        if subagent {
            payload["agent_id"] = serde_json::json!("agent-py-dollar");
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
                "`{command}` (subagent={sub}) only embeds a literal $VAR in a Python string \
                 and must be Allow (base 8fb4e3a9 allowed it), got {d:?}"
            );
        }
    }

    fn assert_deny_both(&self, command: &str) {
        for sub in [false, true] {
            let d = self.decision(command, sub);
            assert_eq!(
                d, "deny",
                "`{command}` (subagent={sub}) runs a destructive embedded command and must stay Deny, got {d:?}"
            );
        }
    }
}

macro_rules! target {
    ($name:ident, $cmd:expr) => {
        #[test]
        fn $name() {
            Fixture::new(stringify!($name)).assert_allow_both($cmd);
        }
    };
}

macro_rules! control {
    ($name:ident, $cmd:expr) => {
        #[test]
        fn $name() {
            Fixture::new(stringify!($name)).assert_deny_both($cmd);
        }
    };
}

// ---- targets: literal $VAR inside Python strings is Allow ----

target!(
    target_os_system_echo_dq_home,
    "python3 - <<'PY'\nimport os\nos.system('echo \"$HOME\"')\nPY"
);
target!(
    target_os_system_echo_sq_home,
    "python3 - <<'PY'\nimport os\nos.system(\"echo '$HOME'\")\nPY"
);
target!(
    target_os_system_echo_braced_home,
    "python3 - <<'PY'\nimport os\nos.system('echo \"${HOME}\"')\nPY"
);
target!(
    target_os_system_echo_cmd_subst_pwd,
    "python3 - <<'PY'\nimport os\nos.system('echo \"$(pwd)\"')\nPY"
);
target!(
    target_os_system_cd_home_ls,
    "python3 - <<'PY'\nimport os\nos.system('cd \"$HOME\" && ls')\nPY"
);
target!(
    target_subprocess_run_shell_true_echo_home,
    "python3 - <<'PY'\nimport subprocess\nsubprocess.run('echo \"$HOME\"', shell=True)\nPY"
);
target!(
    target_subprocess_run_argv_list_echo_home,
    "python3 - <<'PY'\nimport subprocess\nsubprocess.run(['echo', '$HOME'])\nPY"
);

// ---- controls: destructive embedded commands stay Deny ----

control!(
    control_os_system_rm_rf_etc_x,
    "python3 - <<'PY'\nimport os\nos.system('rm -rf /etc/x')\nPY"
);
control!(
    control_os_system_rm_etc_passwd,
    "python3 - <<'PY'\nimport os\nos.system('rm /etc/passwd')\nPY"
);
control!(
    control_subprocess_run_shell_true_rm_rf_home,
    "python3 - <<'PY'\nimport subprocess\nsubprocess.run('rm -rf ~', shell=True)\nPY"
);
