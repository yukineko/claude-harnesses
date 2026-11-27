#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Verifier (session-master-verify): false-positive overreach controls.
//! Ordinary, non-master-only commands run by a MEMBER must NOT be Denied.
//! One test per command so a RED case can be ignored without hiding the rest.
//! Second half: bypass probes (master-only effect that a member is Allowed).
mod support;
use support::*;

const NOW: i64 = T0 + 5;

fn allowed_in_main(cmd: &str) {
    let sb = Sandbox::new();
    sb.master_and_member();
    let o = sb.gate_bash("B", &sb.repo, cmd, NOW);
    assert_eq!(o.code, 0, "{cmd:?}: stderr {}", o.stderr);
    assert!(
        !is_deny(&o),
        "member Denied for ordinary {cmd:?}: {:?}",
        o.stdout
    );
}

fn allowed_in_feature(cmd: &str) {
    let sb = Sandbox::new();
    sb.master_and_member();
    let wt = sb.worktree("feat", "feat-b");
    let cmd = cmd.replace("WT", wt.to_str().unwrap());
    let o = sb.gate_bash("B", &wt, &cmd, NOW);
    assert_eq!(o.code, 0, "{cmd:?}: stderr {}", o.stderr);
    assert!(
        !is_deny(&o),
        "member Denied for ordinary {cmd:?}: {:?}",
        o.stdout
    );
}

fn denied_in_main(cmd: &str) {
    let sb = Sandbox::new();
    sb.master_and_member();
    let o = sb.gate_bash("B", &sb.repo, cmd, NOW);
    assert_eq!(o.code, 0, "{cmd:?}: stderr {}", o.stderr);
    assert!(is_deny(&o), "member NOT denied for {cmd:?}: {:?}", o.stdout);
}

macro_rules! allow_main {
    ($($name:ident => $cmd:expr;)*) => { $( #[test] fn $name() { allowed_in_main($cmd); } )* };
}
macro_rules! allow_feat {
    ($($name:ident => $cmd:expr;)*) => { $( #[test] fn $name() { allowed_in_feature($cmd); } )* };
}
/// RED observed 2026-11-27 at d01b4fe1b: member Denied an ordinary command.
macro_rules! red_allow_main {
    ($($name:ident => $cmd:expr;)*) => { $(
        #[test]
        fn $name() { allowed_in_main($cmd); }
    )* };
}

red_allow_main! {
    fp_docker_exec_git_status => "docker exec c git status";
    fp_editor_var => "$EDITOR file.txt";
    fp_man_git => "man git";
    fp_brew_install_gh => "brew install gh";
    fp_fd_in_session_master => "fd -e rs . crates/session-master";
    fp_npm_test_grep_ssh => "npm test -- --grep ssh";
}

allow_main! {
    fp_cargo_test => "cargo test -p session-master";
    fp_cargo_env_preamble => ". \"$HOME/.cargo/env\" && cargo build";
    fp_ls => "ls -la crates";
    fp_ls_crate_named_session_master => "ls crates/session-master";
    fp_grep => "grep -rn 'git push' docs";
    fp_rg => "rg -n session-master crates";
    fp_git_status => "git status";
    fp_git_log => "git log --oneline -5";
    fp_git_diff => "git diff HEAD~1";
    fp_git_add => "git add crates/x.rs";
    fp_git_commit => "git commit -m 'fix git push handling'";
    fp_git_pipe_head => "git log --oneline | head -3";
    fp_make => "make";
    fp_make_test => "make -C sub test";
    fp_npm_test => "npm test";
    fp_python_script => "python3 script.py";
    fp_python_m => "python3 -m pytest tests";
    fp_gh_pr_view => "gh pr view 12";
    fp_gh_pr_list => "gh pr list";
    fp_sm_status_self => "session-master status";
    fp_sm_role_self => "session-master role --session B";
    fp_find_exec_grep => r"find . -name '*.rs' -exec grep -l foo {} \;";
    fp_vim_file => "vim src/main.rs";
    fp_for_loop => "for f in a b; do wc -l $f; done";
    fp_env_cargo => "env RUST_LOG=debug cargo test";
    fp_timeout_cargo => "timeout 600 cargo test -p x";
    fp_condukt_state => "condukt state check-oracle";
    fp_backlog_list => "backlog list";
    fp_git_fetch_origin => "git fetch origin";
    fp_git_worktree_add => "git worktree add ../x -b y";
}

allow_feat! {
    fp_feat_git_merge_main => "git merge main";
    fp_feat_git_rebase_main => "git rebase main";
    fp_feat_git_pull => "git pull";
    fp_feat_git_c_merge => "git -C WT merge main";
    fp_feat_checkout_new_branch => "git checkout -b other";
}

#[test]
fn fp_feat_cd_then_merge() {
    allowed_in_feature("cd WT && git merge main");
}

// ---- bypass probes: master-only effects; a member must be Denied ----

#[test]
fn probe_gh_api_pr_merge_denied() {
    denied_in_main("gh api -X PUT repos/o/r/pulls/1/merge");
}

#[test]
fn probe_gh_api_merges_endpoint_denied() {
    denied_in_main("gh api repos/o/r/merges -f base=main -f head=feat");
}

#[test]
fn probe_curl_github_merge_denied() {
    denied_in_main("curl -X PUT https://api.github.com/repos/o/r/pulls/1/merge");
}

#[test]
fn probe_copied_rollout_script_denied() {
    denied_in_main("cp scripts/rollout-plugins.sh /tmp/r.sh && bash /tmp/r.sh");
}

#[test]
fn probe_copied_git_binary_push_denied() {
    denied_in_main("cp /usr/bin/git /tmp/g && /tmp/g push");
}

#[test]
fn probe_gh_pr_merge_with_global_flag_denied() {
    // `gh` positional parse: does a leading unknown flag hide `pr merge`?
    denied_in_main("gh --repo o/r pr merge 1");
}

#[test]
fn probe_git_push_via_pushurl_alias_free_form_denied() {
    // `git -c remote.origin.mirror=true fetch` is not a push; but
    // `git -c push.default=x push` must still be a push.
    denied_in_main("git -c push.default=current push");
}
