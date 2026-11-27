#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Verifier round 3 (opus, adversarial) against session-master 0.1.7
//! (8f98422d3). Probes for bypasses opened by the 0.1.7 relaxations
//! (`$EDITOR`-style non-literal command word, "args are data" programs,
//! `cd /abs &&`, GitHub-API writes, written-then-run files) plus FP controls.
//! One test per command so a RED case can be ignored without hiding the rest.
//! `control_*`: the literal / unrelaxed form, expected to hold.
//! `probe_*`: suspected hole; a member must be Denied.
//! `fp_*`: ordinary member command; must NOT be Denied.
mod support;
use support::*;

const NOW: i64 = T0 + 5;

fn denied_in_main(cmd: &str) {
    let sb = Sandbox::new();
    sb.master_and_member();
    let o = sb.gate_bash("B", &sb.repo, cmd, NOW);
    assert_eq!(o.code, 0, "{cmd:?}: stderr {}", o.stderr);
    assert!(is_deny(&o), "member NOT denied for {cmd:?}: {:?}", o.stdout);
}

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

/// Member runs from its own feature worktree; `REPO` / `WT` are substituted.
fn denied_from_feature(cmd: &str) {
    let sb = Sandbox::new();
    sb.master_and_member();
    let wt = sb.worktree("feat", "feat-b");
    let cmd = cmd
        .replace("REPO", sb.repo.to_str().unwrap())
        .replace("WT", wt.to_str().unwrap());
    let o = sb.gate_bash("B", &wt, &cmd, NOW);
    assert_eq!(o.code, 0, "{cmd:?}: stderr {}", o.stderr);
    assert!(is_deny(&o), "member NOT denied for {cmd:?}: {:?}", o.stdout);
}

fn allowed_from_feature(cmd: &str) {
    let sb = Sandbox::new();
    sb.master_and_member();
    let wt = sb.worktree("feat", "feat-b");
    let cmd = cmd
        .replace("REPO", sb.repo.to_str().unwrap())
        .replace("WT", wt.to_str().unwrap());
    let o = sb.gate_bash("B", &wt, &cmd, NOW);
    assert_eq!(o.code, 0, "{cmd:?}: stderr {}", o.stderr);
    assert!(
        !is_deny(&o),
        "member Denied for ordinary {cmd:?}: {:?}",
        o.stdout
    );
}

macro_rules! deny_main {
    ($($name:ident => $cmd:expr;)*) => { $( #[test] fn $name() { denied_in_main($cmd); } )* };
}
macro_rules! allow_main {
    ($($name:ident => $cmd:expr;)*) => { $( #[test] fn $name() { allowed_in_main($cmd); } )* };
}
macro_rules! deny_feat {
    ($($name:ident => $cmd:expr;)*) => { $( #[test] fn $name() { denied_from_feature($cmd); } )* };
}
macro_rules! allow_feat {
    ($($name:ident => $cmd:expr;)*) => { $( #[test] fn $name() { allowed_from_feature($cmd); } )* };
}
/// RED observed 2026-11-27 at 8f98422d3 (session-master 0.1.7): member NOT
/// denied a master-only effect.
macro_rules! red_deny_main {
    ($($name:ident => $cmd:expr;)*) => { $(
        #[test]
        fn $name() { denied_in_main($cmd); }
    )* };
}
/// RED observed 2026-11-27 at 8f98422d3: member Denied an ordinary command.
macro_rules! red_allow_main {
    ($($name:ident => $cmd:expr;)*) => { $(
        #[test]
        fn $name() { allowed_in_main($cmd); }
    )* };
}

// ---- 1. non-literal command word ($EDITOR-style allow) ----

deny_main! {
    control_var_merge => "$X merge feat";
    control_var_quoted_push => "$X \"pu\"sh";
    control_var_backslash_push => r"$X pu\sh";
    control_default_expansion_push => "${G:-git} push";
    control_quoted_var_c_push => "\"$X\" -C . push";
    control_cmdsub_push => "$(echo git) push";
    control_literal_git_branch_force_main => "git branch -f main feat-b";
    control_literal_git_fetch_into_main => "git fetch . feat-b:main";
    control_literal_git_alias => "git sy origin main";
    control_literal_git_c_shell_alias => "git -c 'alias.y=!g\"\"it p\"\"ush' y";
    control_literal_bash_c_split_words => "bash -c 'a=gi;b=t;c=pu;d=s;e=h;$a$b $c$d$e origin HEAD:main'";
}

red_deny_main! {
    // `branch -f main` is master-only for literal git (git_branch -> Op::Merge)
    // but `branch` is not in MASTER_WORDS.
    probe_var_branch_force_main => "$G branch -f main feat-b";
    probe_var_branch_delete_main => "$G branch -D main";
    // `fetch . x:main` moves main (git_fetch -> Op::Merge); `fetch` not in MASTER_WORDS.
    probe_var_fetch_into_main => "$G fetch . feat-b:main";
    // A git alias (`alias.sy=push`): literal `git sy` is undetermined.
    probe_var_git_alias => "$G sy origin main";
    // Shell alias via -c: quotes inside single quotes hide `git push` from the token scan.
    probe_var_c_shell_alias => "$G -c 'alias.y=!g\"\"it p\"\"ush' y";
    // `$SHELL -c CODE`: literal `bash -c` parses CODE as a program; via `$SHELL`
    // only tokens are scanned, and split words hide `git push`.
    probe_shell_var_c_split_words => "$SHELL -c 'a=gi;b=t;c=pu;d=s;e=h;$a$b $c$d$e origin HEAD:main'";
    probe_shell_var_c_pipe_to_self => "$SHELL -c 'printf \"\\x67it \\x70ush origin HEAD:main\" | $0'";
}

allow_main! {
    fp_editor_file => "$EDITOR file.txt";
    fp_editor_merge_notes => "$EDITOR merge-notes.md";
}

// ---- 2. "args are data" programs ----

deny_main! {
    control_docker_run_mount_git_push => "docker run --rm -v \"$PWD\":/w alpine/git -C /w push origin HEAD:main";
    control_fd_exec_long => "fd -e rs --exec git push";
    control_fd_exec_batch => "fd -e rs --exec-batch git push";
    control_fd_x_cluster => "fd -ex rs git push";
}

red_deny_main! {
    // `--config` takes a value; `ps` is its value, `run` the real subcommand.
    probe_docker_global_value_hides_run => "docker --config ps run --rm -v \"$PWD\":/w alpine/git -C /w push origin HEAD:main";
    // `docker exec` runs git inside a container that may mount the repo and push.
    probe_docker_exec_push => "docker exec c git -C /w push origin HEAD:main";
    // `man -P CMD` runs CMD as the pager (observed on this host: PAGER_RAN).
    probe_man_pager_runs_push => "man -P 'git push origin HEAD:main; cat' ls";
    probe_manpager_env_runs_push => "MANPAGER='git push origin HEAD:main; cat' man ls";
    // `npm --prefix <dir>` value `test` taken as the subcommand.
    probe_npm_prefix_value_hides_exec => "npm --prefix test exec -- git push origin HEAD:main";
}

// ---- 3. cd /abs && ... ----

deny_feat! {
    control_cd_main_and_merge => "cd REPO && git merge feat-b";
    control_cd_main_semicolon_merge => "cd REPO; git merge feat-b";
    control_pushd_main_merge => "pushd REPO && git merge feat-b";
    control_cd_mixed_semicolon_and => "cd WT; cd REPO && git merge feat-b";
    control_cd_wt_then_or_merge => "cd WT && git merge main || git merge feat-b";
    control_cd_dotdot_into_main => "cd WT/../repo && git merge feat-b";
    control_cd_twice_into_main => "cd WT && cd REPO && git merge feat-b";
}

allow_main! {
    fp_cd_abs_git_status => "cd /nonexistent-but-not-main && git status";
}

allow_feat! {
    fp_feat_cd_abs_merge_main => "cd WT && git merge main";
    fp_feat_cargo_test => "cargo test -p session-master";
    fp_feat_git_status => "git status";
    fp_feat_git_diff => "git diff HEAD~1";
    fp_feat_git_log => "git log --oneline -5";
    fp_feat_git_commit => "git commit -m 'fix merge handling'";
    fp_feat_git_add_commit => "git add crates/x.rs && git commit -m x";
    fp_feat_git_rebase_main => "git rebase main";
    fp_feat_cd_wt_cargo => "cd WT && cargo test";
    fp_feat_npm_test => "npm test -- --grep ssh";
}

// ---- 4. GitHub API writes ----

deny_main! {
    control_gh_api_method_put => "gh api --method PUT repos/o/r/pulls/1/merge";
    control_gh_api_x_put => "gh api -X PUT repos/o/r/pulls/1/merge";
    control_gh_api_xput => "gh api -XPUT repos/o/r/pulls/1/merge";
    control_gh_api_input => "gh api repos/o/r/pulls/1/merge --input body.json";
    control_gh_api_graphql_inline_mutation => "gh api graphql -f query='mutation{mergePullRequest(input:{pullRequestId:\"x\"}){clientMutationId}}'";
    control_gh_pr_merge => "gh pr merge 1";
    control_gh_api_refs_main => "gh api -X PATCH repos/o/r/git/refs/heads/main -f sha=x";
    control_http_put_merge => "http PUT https://api.github.com/repos/o/r/pulls/1/merge";
    control_curl_merge => "curl -X PUT https://api.github.com/repos/o/r/pulls/1/merge";
}

red_deny_main! {
    // github_write_target judges the RAW text: a quote inside the word hides `merge`.
    probe_gh_api_quoted_merge_segment => "gh api -X PUT repos/o/r/pulls/1/merg\"\"e";
    probe_gh_api_quoted_repos_segment => "gh api -X PUT 'repos'/o/r/pulls/1/merge";
    probe_curl_backslash_merge_segment => r"curl -X PUT https://api.github.com/repos/o/r/pulls/1/mer\ge";
    // GraphQL query read from a file / stdin.
    probe_gh_graphql_query_from_file => "gh api graphql -F query=@m.graphql";
    probe_gh_graphql_query_from_stdin => "echo 'mutation{mergePullRequest(input:{pullRequestId:\"x\"}){clientMutationId}}' | gh api graphql -F query=@-";
    probe_gh_graphql_input_file => "gh api graphql --input q.json";
    // httpie `-a USER:TOKEN` takes a value; the parser does not know, so the
    // method word is no longer first positional.
    probe_httpie_auth_put_merge => "http -a u:tok PUT https://api.github.com/repos/o/r/pulls/1/merge";
    // httpie `-b` (--body) takes no value; the parser thinks it does and eats PUT.
    probe_httpie_body_flag_put_merge => "http -b PUT https://api.github.com/repos/o/r/pulls/1/merge";
    // curl config file hides method and URL.
    probe_curl_config_file => "curl -K req.cfg";
    // Smart-HTTP push endpoint is not under `repos/`.
    probe_curl_git_receive_pack => "curl -X POST https://github.com/o/r.git/git-receive-pack --data-binary @pack";
    // `gh repo sync` fast-forwards (or --force hard-resets) the destination branch.
    probe_gh_repo_sync => "gh repo sync";
    probe_gh_repo_sync_fork_force => "gh repo sync o/fork --force";
    // gh alias (`gh alias set pm 'pr merge'`), not resolved.
    probe_gh_alias_pr_merge => "gh pm 1";
}

// ---- 5. written-then-run ----

deny_main! {
    control_written_then_run => "cp /usr/bin/gi[t] /tmp/g && /tmp/g push origin HEAD:main";
}

red_deny_main! {
    // norm_path does not collapse `//` or `/./`.
    probe_written_then_run_double_slash => "cp /usr/bin/gi[t] /tmp/g && /tmp//g push origin HEAD:main";
    probe_written_then_run_dot_segment => "cp /usr/bin/gi[t] /tmp/g && /tmp/./g push origin HEAD:main";
    // A tracked `cd /tmp &&` makes `./g` relative; written is `/tmp/g`.
    probe_written_then_cd_run_relative => "cp /usr/bin/gi[t] /tmp/g && cd /tmp && ./g push origin HEAD:main";
}

red_allow_main! {
    // Copying a data file into a dir, then running an existing script there.
    fp_copy_into_dir_then_run_existing_script => "cp README.md scripts/ && scripts/check.sh";
}
