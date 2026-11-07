// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! False positives of the git exec-capable ban (backlog fb338793).
//!
//! User ruling 2026-11-07, "Allow known-safe values": a command-valued git
//! key (`core.editor`, `credential.helper`, `core.sshCommand`, `gpg.program`,
//! `sequence.editor`, ...) may be set to a fixed list of safe values, the same
//! way the existing pager exception (`less`/`more`/`cat` + flags) works;
//! anything else stays Deny under the ban. Bare `git grep -O` /
//! `--open-files-in-pager` with no argument runs the configured pager, not a
//! caller-chosen command, and is Allow.
//!
//! `target_*` tests: ordinary commands that must be Allow (RED on the branch
//! at 762ac603/c547b61c: Deny). `control_*` tests: the same keys with a
//! non-listed value, which must stay Deny (pass now). Every command is driven
//! as the main agent AND as a subagent (`agent_id` set).
//!
//! The hook runs with `HOME=<base>/home`, payload cwd = `CLAUDE_PROJECT_DIR` =
//! a real one-commit repo under [`neutral_base::neutral_base`], so Allow is
//! exactly empty stdout.

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
            .join("git_exec_ban_safe_values_fp_20261107")
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
            session: format!("git-safe-values-{name}-{}", std::process::id()),
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
            payload["agent_id"] = serde_json::json!("agent-safe-values");
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
                "`{command}` (subagent={sub}) is an ordinary command with a known-safe value \
                 and must be Allow (user ruling 2026-11-07), got {d:?}"
            );
        }
    }

    fn assert_deny_both(&self, command: &str) {
        for sub in [false, true] {
            let d = self.decision(command, sub);
            assert_eq!(
                d, "deny",
                "`{command}` (subagent={sub}) runs a non-listed command and must stay Deny, got {d:?}"
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

// ---- targets: known-safe values are Allow ----

target!(
    target_config_global_core_editor_vim,
    "git config --global core.editor vim"
);
target!(
    target_config_global_core_editor_nano,
    "git config --global core.editor nano"
);
target!(
    target_config_global_core_editor_code_wait,
    "git config --global core.editor 'code --wait'"
);
target!(
    target_dash_c_core_editor_vim_commit,
    "git -c core.editor=vim commit"
);
target!(
    target_dash_c_core_editor_code_wait_commit,
    "git -c core.editor='code --wait' commit"
);
target!(
    target_config_global_credential_helper_osxkeychain,
    "git config --global credential.helper osxkeychain"
);
target!(
    target_config_global_credential_helper_store,
    "git config --global credential.helper store"
);
target!(
    target_config_global_credential_helper_cache,
    "git config --global credential.helper cache"
);
target!(
    target_dash_c_core_sshcommand_ssh_fetch,
    "git -c core.sshCommand=ssh fetch"
);
target!(
    target_dash_c_core_sshcommand_ssh_i_key_fetch,
    "git -c core.sshCommand='ssh -i key' fetch"
);
target!(
    target_dash_c_gpg_program_gpg_commit,
    "git -c gpg.program=gpg commit"
);
target!(
    target_dash_c_sequence_editor_colon_rebase_i,
    "git -c sequence.editor=: rebase -i HEAD~1"
);
target!(
    target_dash_c_core_editor_true_rebase_continue,
    "git -c core.editor=true rebase --continue"
);
target!(target_grep_n_capital_o_bare, "git grep -nO foo");
target!(target_grep_f_capital_o_bare, "git grep -F -O foo");
target!(
    target_grep_open_files_in_pager_bare,
    "git grep --open-files-in-pager foo"
);

// ---- controls: non-listed values stay Deny (must pass now and after) ----

control!(
    control_core_editor_rm_rf_home,
    "git config --global core.editor 'rm -rf ~'"
);
control!(
    control_dash_c_core_editor_rm_rf_home,
    "git -c core.editor='rm -rf ~' commit"
);
control!(
    control_core_editor_vim_semicolon_rm,
    "git -c core.editor='vim; rm -rf ~' commit"
);
control!(
    control_core_editor_vim_command_subst,
    "git -c core.editor='vim $(rm -rf ~)' commit"
);
control!(
    control_config_core_editor_vim_command_subst,
    "git config --global core.editor 'vim $(rm -rf ~)'"
);
control!(
    control_credential_helper_bang_rm,
    "git config --global credential.helper '!rm -rf ~'"
);
control!(
    control_dash_c_credential_helper_bang_rm,
    "git -c credential.helper='!rm -rf ~' fetch"
);
control!(
    control_core_sshcommand_proxycommand,
    "git -c core.sshCommand='ssh -o ProxyCommand=sh' fetch"
);
control!(
    control_core_sshcommand_rm_rf_home,
    "git -c core.sshCommand='rm -rf ~' fetch"
);
control!(
    control_gpg_program_sh_c_x,
    "git -c gpg.program='sh -c x' commit"
);
control!(
    control_sequence_editor_rm_rf_home,
    "git -c sequence.editor='rm -rf ~' rebase -i HEAD~1"
);
control!(control_grep_glued_o_rm, "git grep -Orm foo");
control!(control_grep_glued_o_quoted_rm, "git grep -O'rm -rf ~' foo");
control!(
    control_grep_open_files_in_pager_equals_rm,
    "git grep --open-files-in-pager='rm -rf ~' foo"
);
