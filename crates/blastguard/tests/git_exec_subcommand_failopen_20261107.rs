// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED reproduction: system-harming commands and git exec-capable shapes
//! that are not Deny.
//!
//! User ruling 2026-11-07 (verbatim, relayed): "any command harmful to the
//! system must be DENY, for whatever reason; this is a critical bug".
//!
//! User design ruling 2026-11-07 (relayed): instead of parsing what a git
//! exec-wrapper runs, BAN it outright: every git subcommand / option that
//! executes an arbitrary command is Deny regardless of the embedded command
//! (so `git bisect run true` is Deny). The `ban_*` targets pin that with a
//! HARMLESS embedded command.
//!
//! Three shapes measured 2026-11-07 on main fa2e6fda (blastguard 0.2.287):
//!
//!   1. git subcommands that EXECUTE an argument as a command
//!      (`git bisect run CMD…`, `git rebase --exec/-x CMD`,
//!      `git submodule foreach CMD`) are Allow even when CMD is
//!      `rm -rf /usr`, which is Deny on its own. Cause read in
//!      `src/detect/git.rs`: `analyze_git_words` ends in
//!      `_ => Decision::Allow`, so an unmodelled subcommand never has its
//!      embedded command analysed.
//!   2. non-recursive `rm` of a system file (`rm /etc/passwd`,
//!      `rm -f /bin/ls`, …) is Allow.
//!   3. the SECOND identical request for `rm -rf /usr/lib` in one session is
//!      Ask, not Deny (`src/main.rs` `downgrade_on_repeat`, operator ruling
//!      2026-09-18 「2度目はaskせよ」). The 2026-11-07 ruling ("for whatever
//!      reason") conflicts with that ruling for system-harming commands; the
//!      target pins the 2026-11-07 reading.
//!   4. (ban ruling) with a HARMLESS embedded command every exec-capable git
//!      shape except `filter-branch` is Allow: `bisect run`, `rebase`/`pull
//!      --rebase` `-x`/`--exec`, `submodule foreach`, `-c alias.x=!…` and
//!      `config alias.x '!…'`, `difftool -x/--extcmd`, `-c core.pager` /
//!      `core.sshCommand` / `core.editor` / `sequence.editor` /
//!      `diff.external`, `grep -O/--open-files-in-pager`,
//!      `--upload-pack`/`-u`/`--receive-pack`, `archive --exec`,
//!      `send-email --to-cmd`.
//!
//! Controls (must pass now and after the fix): bare system deletions are Deny
//! on the first request; non-executing `git bisect start` / `good` / `bad`,
//! `git rebase HEAD~1` and `git log` stay Allow; `git -c 'alias.x=!rm -rf
//! /usr' x` and every `git filter-branch --*-filter` are already Deny (the
//! `ban_already_deny_*` tests pin those ban shapes against regression);
//! deletion inside a `/private/tmp` scratch dir and inside a managed linked
//! worktree (`<dirname(project)>/.harness-worktrees/<wt>`) stays Allow.
//!
//! The hook runs with `HOME=<base>/home` (also isolates the repeat ledger),
//! payload cwd = `CLAUDE_PROJECT_DIR` = a real one-commit repo under
//! [`neutral_base::neutral_base`], `BLASTGUARD_ASK` / `TMPDIR` removed, so
//! Allow is exactly empty stdout.

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
    /// Primary working tree (NOT managed).
    repo: PathBuf,
    /// `<base>/src/.harness-worktrees/wt` — managed linked worktree.
    hwt: PathBuf,
    session: String,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let base = neutral_base::neutral_base()
            .join("git_exec_subcommand_failopen_20261107")
            .join(name);
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        let repo = base.join("src/repo");
        let hw = base.join("src/.harness-worktrees");
        let hwt = hw.join("wt");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&hw).unwrap();
        git(&repo, &["init", "-q"]);
        std::fs::write(repo.join("f.txt"), "x\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "init"]);
        git(
            &repo,
            &["worktree", "add", "-q", "-b", "wt", hwt.to_str().unwrap()],
        );
        std::fs::create_dir_all(hwt.join("subdir/nested")).unwrap();
        std::fs::write(hwt.join("subdir/nested/b.txt"), "b\n").unwrap();
        Fixture {
            base,
            home,
            repo,
            hwt,
            session: format!("git-exec-failopen-{name}-{}", std::process::id()),
        }
    }

    /// Hook stdout for a Bash `command` (cwd = CLAUDE_PROJECT_DIR = repo).
    fn bash(&self, command: &str) -> String {
        let payload = serde_json::json!({
            "session_id": self.session,
            "hook_event_name": "PreToolUse",
            "cwd": self.repo.display().to_string(),
            "tool_name": "Bash",
            "tool_input": { "command": command },
        })
        .to_string();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
        git_env::scrub(&mut cmd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("CLAUDE_PROJECT_DIR", &self.repo)
            .env("BLASTGUARD_APPROVALS_DIR", self.base.join("store"))
            .env("HOME", &self.home)
            .env("CLAUDE_CODE_ENTRYPOINT", "cli")
            .env("CLAUDECODE", "1")
            .env("CLAUDE_CODE_SESSION_ID", &self.session)
            .env_remove("BLASTGUARD_ASK")
            .env_remove("TMPDIR")
            .current_dir(&self.base);
        let out = bounded::run_bounded(&mut cmd, payload.as_bytes(), bounded::HOOK_DEADLINE);
        assert_eq!(
            out.status.code(),
            Some(0),
            "hook must exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// `"allow"` for empty stdout, else the emitted `permissionDecision`.
    fn decision(&self, command: &str) -> String {
        let out = self.bash(command);
        if out.trim().is_empty() {
            return "allow".to_string();
        }
        serde_json::from_str::<serde_json::Value>(out.trim())
            .unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {out}"))
            .pointer("/hookSpecificOutput/permissionDecision")
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_else(|| panic!("no permissionDecision in {out}"))
    }

    fn assert_deny(&self, command: &str) {
        let d = self.decision(command);
        assert_eq!(
            d, "deny",
            "`{command}` harms the system and must be Deny (user ruling 2026-11-07), got {d:?}"
        );
    }

    fn assert_allow(&self, command: &str) {
        let d = self.decision(command);
        assert_eq!(d, "allow", "`{command}` must stay Allow, got {d:?}");
    }
}

// ---- controls (must pass) ----

#[test]
fn control_bare_rm_rf_usr_is_deny() {
    Fixture::new("c_usr").assert_deny("rm -rf /usr");
}

#[test]
fn control_bare_rm_rf_usr_lib_first_request_is_deny() {
    Fixture::new("c_usr_lib").assert_deny("rm -rf /usr/lib");
}

#[test]
fn control_bare_rm_rf_etc_is_deny() {
    Fixture::new("c_etc").assert_deny("rm -rf /etc");
}

#[test]
fn control_bare_rm_rf_system_is_deny() {
    Fixture::new("c_system").assert_deny("rm -rf /System");
}

#[test]
fn control_bare_rm_rf_bin_is_deny() {
    Fixture::new("c_bin").assert_deny("rm -rf /bin");
}

#[test]
fn control_bare_rm_rf_home_ssh_is_deny() {
    Fixture::new("c_ssh").assert_deny("rm -rf ~/.ssh");
}

#[test]
fn control_bare_rm_rf_dollar_home_ssh_is_deny() {
    Fixture::new("c_ssh_var").assert_deny("rm -rf $HOME/.ssh");
}

#[test]
fn control_git_alias_bang_destructive_is_deny() {
    Fixture::new("c_alias").assert_deny("git -c 'alias.x=!rm -rf /usr' x");
}

#[test]
fn control_git_filter_branch_tree_filter_destructive_is_deny() {
    Fixture::new("c_fb_rm").assert_deny("git filter-branch --tree-filter 'rm -rf /usr' HEAD");
}

#[test]
fn ban_already_deny_filter_branch_tree_filter() {
    Fixture::new("c_fb_tree").assert_deny("git filter-branch --tree-filter true HEAD");
}

#[test]
fn ban_already_deny_filter_branch_index_filter() {
    Fixture::new("c_fb_index").assert_deny("git filter-branch --index-filter true HEAD");
}

#[test]
fn ban_already_deny_filter_branch_msg_filter() {
    Fixture::new("c_fb_msg").assert_deny("git filter-branch --msg-filter cat HEAD");
}

#[test]
fn ban_already_deny_filter_branch_env_filter() {
    Fixture::new("c_fb_env").assert_deny("git filter-branch --env-filter true HEAD");
}

#[test]
fn ban_already_deny_filter_branch_commit_filter() {
    Fixture::new("c_fb_commit")
        .assert_deny("git filter-branch --commit-filter 'git commit-tree \"$@\"' HEAD");
}

#[test]
fn ban_already_deny_filter_branch_parent_filter() {
    Fixture::new("c_fb_parent").assert_deny("git filter-branch --parent-filter cat HEAD");
}

#[test]
fn ban_already_deny_filter_branch_tag_name_filter() {
    Fixture::new("c_fb_tag").assert_deny("git filter-branch --tag-name-filter cat HEAD");
}

#[test]
fn control_git_bisect_start_stays_allow() {
    Fixture::new("c_bisect_start").assert_allow("git bisect start");
}

#[test]
fn control_git_bisect_good_stays_allow() {
    Fixture::new("c_bisect_good").assert_allow("git bisect good");
}

#[test]
fn control_git_bisect_bad_stays_allow() {
    Fixture::new("c_bisect_bad").assert_allow("git bisect bad");
}

#[test]
fn control_git_rebase_without_exec_stays_allow() {
    Fixture::new("c_rebase").assert_allow("git rebase HEAD~1");
}

#[test]
fn control_git_log_stays_allow() {
    Fixture::new("c_log").assert_allow("git log");
}

#[test]
fn control_rm_rf_inside_private_tmp_scratch_stays_allow() {
    let f = Fixture::new("c_tmp");
    let scratch = PathBuf::from(format!(
        "/private/tmp/blastguard-git-exec-failopen-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(scratch.join("work/nested")).unwrap();
    std::fs::write(scratch.join("work/nested/a.txt"), "a\n").unwrap();
    let cmd = format!("rm -rf {}", scratch.join("work").display());
    let d = f.decision(&cmd);
    let _ = std::fs::remove_dir_all(&scratch);
    assert_eq!(d, "allow", "`{cmd}` must stay Allow, got {d:?}");
}

#[test]
fn control_rm_rf_inside_managed_worktree_stays_allow() {
    let f = Fixture::new("c_wt");
    f.assert_allow(&format!("rm -rf {}", f.hwt.join("subdir").display()));
}

// ---- targets: git exec-capable shapes are banned (RED on 0.2.287: Allow) ----

#[test]
#[ignore = "backlog fb338793: git exec-capable shape must be Deny (ban ruling)"]
fn ban_git_bisect_run_true() {
    Fixture::new("t_ban_git_bisect_run_true").assert_deny("git bisect run true");
}

#[test]
#[ignore = "backlog fb338793: git exec-capable shape must be Deny (ban ruling)"]
fn ban_git_bisect_run_make_test() {
    Fixture::new("t_ban_git_bisect_run_make_test").assert_deny("git bisect run make test");
}

#[test]
#[ignore = "backlog fb338793: git exec-capable shape must be Deny (ban ruling)"]
fn ban_git_rebase_x() {
    Fixture::new("t_ban_git_rebase_x").assert_deny("git rebase -x 'echo ok' HEAD~1");
}

#[test]
#[ignore = "backlog fb338793: git exec-capable shape must be Deny (ban ruling)"]
fn ban_git_rebase_exec() {
    Fixture::new("t_ban_git_rebase_exec").assert_deny("git rebase --exec 'echo ok' HEAD~1");
}

#[test]
#[ignore = "backlog fb338793: git exec-capable shape must be Deny (ban ruling)"]
fn ban_git_rebase_exec_equals() {
    Fixture::new("t_ban_git_rebase_exec_equals").assert_deny("git rebase --exec='echo ok' HEAD~1");
}

#[test]
#[ignore = "backlog fb338793: git exec-capable shape must be Deny (ban ruling)"]
fn ban_git_rebase_i_exec() {
    Fixture::new("t_ban_git_rebase_i_exec").assert_deny("git rebase -i --exec 'echo ok' HEAD~1");
}

#[test]
#[ignore = "backlog fb338793: git exec-capable shape must be Deny (ban ruling)"]
fn ban_git_pull_rebase_x() {
    Fixture::new("t_ban_git_pull_rebase_x").assert_deny("git pull --rebase -x 'echo ok'");
}

#[test]
#[ignore = "backlog fb338793: git exec-capable shape must be Deny (ban ruling)"]
fn ban_git_submodule_foreach() {
    Fixture::new("t_ban_git_submodule_foreach").assert_deny("git submodule foreach 'echo ok'");
}

#[test]
#[ignore = "backlog fb338793: git exec-capable shape must be Deny (ban ruling)"]
fn ban_git_submodule_foreach_recursive() {
    Fixture::new("t_ban_git_submodule_foreach_recursive")
        .assert_deny("git submodule foreach --recursive true");
}

#[test]
#[ignore = "backlog fb338793: git exec-capable shape must be Deny (ban ruling)"]
fn ban_git_c_shell_alias_invoked() {
    Fixture::new("t_ban_git_c_shell_alias_invoked").assert_deny("git -c 'alias.x=!echo ok' x");
}

#[test]
#[ignore = "backlog fb338793: git exec-capable shape must be Deny (ban ruling)"]
fn ban_git_c_shell_alias_invoked_quoted_value() {
    Fixture::new("t_ban_git_c_shell_alias_invoked_quoted_value")
        .assert_deny("git -c alias.x='!echo ok' x");
}

#[test]
#[ignore = "backlog fb338793: git exec-capable shape must be Deny (ban ruling)"]
fn ban_git_config_defines_shell_alias() {
    Fixture::new("t_ban_git_config_defines_shell_alias")
        .assert_deny("git config alias.x '!echo ok'");
}

#[test]
#[ignore = "backlog fb338793: git exec-capable shape must be Deny (ban ruling)"]
fn ban_git_config_global_defines_shell_alias() {
    Fixture::new("t_ban_git_config_global_defines_shell_alias")
        .assert_deny("git config --global alias.x '!echo ok'");
}

#[test]
#[ignore = "backlog fb338793: git exec-capable shape must be Deny (ban ruling)"]
fn ban_git_difftool_extcmd_equals() {
    Fixture::new("t_ban_git_difftool_extcmd_equals").assert_deny("git difftool --extcmd=echo HEAD");
}

#[test]
#[ignore = "backlog fb338793: git exec-capable shape must be Deny (ban ruling)"]
fn ban_git_difftool_extcmd() {
    Fixture::new("t_ban_git_difftool_extcmd").assert_deny("git difftool --extcmd echo HEAD");
}

#[test]
#[ignore = "backlog fb338793: git exec-capable shape must be Deny (ban ruling)"]
fn ban_git_difftool_x() {
    Fixture::new("t_ban_git_difftool_x").assert_deny("git difftool -x echo HEAD");
}

#[test]
#[ignore = "backlog fb338793: git exec-capable shape must be Deny (ban ruling)"]
fn ban_git_c_core_pager() {
    Fixture::new("t_ban_git_c_core_pager").assert_deny("git -c core.pager='echo ok' log");
}

#[test]
#[ignore = "backlog fb338793: git exec-capable shape must be Deny (ban ruling)"]
fn ban_git_c_core_sshcommand() {
    Fixture::new("t_ban_git_c_core_sshcommand")
        .assert_deny("git -c core.sshCommand='echo ok' fetch");
}

#[test]
#[ignore = "backlog fb338793: git exec-capable shape must be Deny (ban ruling)"]
fn ban_git_c_core_editor() {
    Fixture::new("t_ban_git_c_core_editor").assert_deny("git -c core.editor='echo ok' commit");
}

#[test]
#[ignore = "backlog fb338793: git exec-capable shape must be Deny (ban ruling)"]
fn ban_git_c_sequence_editor() {
    Fixture::new("t_ban_git_c_sequence_editor")
        .assert_deny("git -c sequence.editor='echo ok' rebase -i HEAD~1");
}

#[test]
#[ignore = "backlog fb338793: git exec-capable shape must be Deny (ban ruling)"]
fn ban_git_c_diff_external() {
    Fixture::new("t_ban_git_c_diff_external").assert_deny("git -c diff.external='echo ok' diff");
}

#[test]
#[ignore = "backlog fb338793: git exec-capable shape must be Deny (ban ruling)"]
fn ban_git_grep_open_files_in_pager_short() {
    Fixture::new("t_ban_git_grep_open_files_in_pager_short")
        .assert_deny("git grep -O'echo ok' foo");
}

#[test]
#[ignore = "backlog fb338793: git exec-capable shape must be Deny (ban ruling)"]
fn ban_git_grep_open_files_in_pager() {
    Fixture::new("t_ban_git_grep_open_files_in_pager")
        .assert_deny("git grep --open-files-in-pager='echo ok' foo");
}

#[test]
#[ignore = "backlog fb338793: git exec-capable shape must be Deny (ban ruling)"]
fn ban_git_fetch_upload_pack() {
    Fixture::new("t_ban_git_fetch_upload_pack")
        .assert_deny("git fetch --upload-pack='echo ok' origin");
}

#[test]
#[ignore = "backlog fb338793: git exec-capable shape must be Deny (ban ruling)"]
fn ban_git_clone_u() {
    Fixture::new("t_ban_git_clone_u").assert_deny("git clone -u 'echo ok' /x /y");
}

#[test]
#[ignore = "backlog fb338793: git exec-capable shape must be Deny (ban ruling)"]
fn ban_git_clone_upload_pack() {
    Fixture::new("t_ban_git_clone_upload_pack")
        .assert_deny("git clone --upload-pack='echo ok' /x /y");
}

#[test]
#[ignore = "backlog fb338793: git exec-capable shape must be Deny (ban ruling)"]
fn ban_git_push_receive_pack() {
    Fixture::new("t_ban_git_push_receive_pack")
        .assert_deny("git push --receive-pack='echo ok' origin");
}

#[test]
#[ignore = "backlog fb338793: git exec-capable shape must be Deny (ban ruling)"]
fn ban_git_ls_remote_upload_pack() {
    Fixture::new("t_ban_git_ls_remote_upload_pack")
        .assert_deny("git ls-remote --upload-pack='echo ok' origin");
}

#[test]
#[ignore = "backlog fb338793: git exec-capable shape must be Deny (ban ruling)"]
fn ban_git_archive_remote_exec() {
    Fixture::new("t_ban_git_archive_remote_exec")
        .assert_deny("git archive --remote=origin --exec='echo ok' HEAD");
}

#[test]
#[ignore = "backlog fb338793: git exec-capable shape must be Deny (ban ruling)"]
fn ban_git_send_email_to_cmd() {
    Fixture::new("t_ban_git_send_email_to_cmd")
        .assert_deny("git send-email --to-cmd='echo ok' x.patch");
}

// ---- targets: system-harming commands (RED on 0.2.287: Allow) ----

#[test]
#[ignore = "backlog fb338793: system-harming command must be Deny"]
fn git_bisect_run_rm_rf_usr_is_deny() {
    Fixture::new("t_git_bisect_run_rm_rf_usr_is_deny").assert_deny("git bisect run rm -rf /usr");
}

#[test]
#[ignore = "backlog fb338793: system-harming command must be Deny"]
fn git_bisect_run_rm_rf_usr_lib_is_deny() {
    Fixture::new("t_git_bisect_run_rm_rf_usr_lib_is_deny")
        .assert_deny("git bisect run rm -rf /usr/lib");
}

#[test]
#[ignore = "backlog fb338793: system-harming command must be Deny"]
fn git_bisect_run_sh_c_rm_rf_usr_is_deny() {
    Fixture::new("t_git_bisect_run_sh_c_rm_rf_usr_is_deny")
        .assert_deny("git bisect run sh -c 'rm -rf /usr'");
}

#[test]
#[ignore = "backlog fb338793: system-harming command must be Deny"]
fn git_rebase_exec_rm_rf_usr_is_deny() {
    Fixture::new("t_git_rebase_exec_rm_rf_usr_is_deny")
        .assert_deny("git rebase --exec 'rm -rf /usr' HEAD~1");
}

#[test]
#[ignore = "backlog fb338793: system-harming command must be Deny"]
fn git_rebase_exec_equals_rm_rf_usr_is_deny() {
    Fixture::new("t_git_rebase_exec_equals_rm_rf_usr_is_deny")
        .assert_deny("git rebase -i --exec='rm -rf /usr' HEAD~1");
}

#[test]
#[ignore = "backlog fb338793: system-harming command must be Deny"]
fn git_rebase_x_rm_rf_usr_is_deny() {
    Fixture::new("t_git_rebase_x_rm_rf_usr_is_deny")
        .assert_deny("git rebase -x 'rm -rf /usr' HEAD~1");
}

#[test]
#[ignore = "backlog fb338793: system-harming command must be Deny"]
fn git_submodule_foreach_rm_rf_usr_is_deny() {
    Fixture::new("t_git_submodule_foreach_rm_rf_usr_is_deny")
        .assert_deny("git submodule foreach 'rm -rf /usr'");
}

#[test]
#[ignore = "backlog fb338793: system-harming command must be Deny"]
fn git_submodule_foreach_recursive_rm_rf_usr_is_deny() {
    Fixture::new("t_git_submodule_foreach_recursive_rm_rf_usr_is_deny")
        .assert_deny("git submodule foreach --recursive rm -rf /usr");
}

#[test]
#[ignore = "backlog fb338793: system-harming command must be Deny"]
fn rm_etc_passwd_is_deny() {
    Fixture::new("t_rm_etc_passwd_is_deny").assert_deny("rm /etc/passwd");
}

#[test]
#[ignore = "backlog fb338793: system-harming command must be Deny"]
fn rm_f_etc_passwd_is_deny() {
    Fixture::new("t_rm_f_etc_passwd_is_deny").assert_deny("rm -f /etc/passwd");
}

#[test]
#[ignore = "backlog fb338793: system-harming command must be Deny"]
fn rm_etc_hosts_is_deny() {
    Fixture::new("t_rm_etc_hosts_is_deny").assert_deny("rm /etc/hosts");
}

#[test]
#[ignore = "backlog fb338793: system-harming command must be Deny"]
fn rm_f_bin_ls_is_deny() {
    Fixture::new("t_rm_f_bin_ls_is_deny").assert_deny("rm -f /bin/ls");
}

#[test]
#[ignore = "backlog fb338793: system-harming command must be Deny"]
fn rm_f_usr_lib_libsystem_is_deny() {
    Fixture::new("t_rm_f_usr_lib_libsystem_is_deny")
        .assert_deny("rm -f /usr/lib/libSystem.B.dylib");
}

// ---- target: repeat downgrade of a system-harming deny (RED on 0.2.287: Ask) ----

#[test]
#[ignore = "backlog fb338793: repeated system-harming deny must stay Deny"]
fn second_identical_rm_rf_usr_lib_stays_deny() {
    let f = Fixture::new("t_repeat_usr_lib");
    f.assert_deny("rm -rf /usr/lib");
    f.assert_deny("rm -rf /usr/lib");
}
