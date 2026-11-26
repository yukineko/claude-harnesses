#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Second round of verifier-found gate bypasses (P1..P8). Black-box RED tests.
//! Every case: a MEMBER running push / rollout / main-move in a form the
//! detector cannot prove harmless must be Denied (cannot-determine => Deny).
mod support;
use support::*;

const NOW: i64 = T0 + 5;

/// Run each cmd as member B from `cwd_of(sandbox)`; every one must be Deny.
/// All failures are collected so one test reports every bypassing form.
fn member_denied_in(cmds: &[String], cwd_of: &dyn Fn(&Sandbox) -> std::path::PathBuf) {
    let mut bad = Vec::new();
    for cmd in cmds {
        let sb = Sandbox::new();
        sb.master_and_member();
        let cwd = cwd_of(&sb);
        let o = sb.gate_bash("B", &cwd, cmd, NOW);
        if o.code != 0 || !is_deny(&o) {
            bad.push(format!("{cmd:?} -> code {} stdout {:?}", o.code, o.stdout));
        }
    }
    assert!(bad.is_empty(), "member NOT denied for:\n{}", bad.join("\n"));
}

fn member_denied(cmds: &[&str]) {
    let v: Vec<String> = cmds.iter().map(|s| s.to_string()).collect();
    member_denied_in(&v, &|sb| sb.repo.clone());
}

#[test]
fn control_member_git_status_allowed() {
    let sb = Sandbox::new();
    sb.master_and_member();
    let o = sb.gate_bash("B", &sb.repo, "git status", NOW);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(!is_deny(&o), "stdout {:?}", o.stdout);
}

#[test]
fn p1_nonliteral_command_word_denied() {
    member_denied(&["G=git; S=push; $G $S"]);
}

#[test]
fn p1_eval_nonliteral_denied() {
    member_denied(&["cmd='git push'; eval \"$cmd\""]);
}

#[test]
fn p1_bash_c_nonliteral_denied() {
    member_denied(&[
        "CMD='git push'; bash -c \"$CMD\"",
        "bash -c \"$(printf 'git pus%s' h)\"",
    ]);
}

#[test]
fn p2_unknown_wrappers_denied() {
    member_denied(&[
        "setsid git push",
        r"find . -maxdepth 0 -exec git push \;",
        "arch -arm64 git push",
        "xcrun git push",
        "flock /tmp/l git push",
        "stdbuf -o0 git push",
        "ksh -c 'git push'",
        "fish -c 'git push'",
    ]);
}

#[test]
fn p3_git_alias_denied() {
    member_denied(&["git config alias.p push; git p", "git p"]);
}

#[test]
fn p4_script_via_stdin_denied() {
    member_denied(&[
        "sh < scripts/rollout-plugins.sh",
        "bash -s < scripts/rollout-plugins.sh",
    ]);
}

#[test]
fn p5_glob_rollout_denied() {
    member_denied(&["bash scripts/roll*"]);
}

#[test]
fn p6_git_dir_to_main_from_feature_worktree_denied() {
    let mut bad = Vec::new();
    for tmpl in [
        "git --git-dir=MAIN/.git merge x",
        "GIT_DIR=MAIN/.git git merge x",
    ] {
        let sb = Sandbox::new();
        sb.master_and_member();
        let wt = sb.worktree("feat", "feat-b");
        let cmd = tmpl.replace("MAIN", sb.repo.to_str().unwrap());
        let o = sb.gate_bash("B", &wt, &cmd, NOW);
        if o.code != 0 || !is_deny(&o) {
            bad.push(format!("{cmd:?} -> code {} stdout {:?}", o.code, o.stdout));
        }
    }
    assert!(bad.is_empty(), "member NOT denied for:\n{}", bad.join("\n"));
}

#[test]
fn p7_main_moving_forms_denied() {
    // From the main checkout (on main).
    member_denied(&[
        "git fetch . x:main",
        "git update-ref refs/heads/main HEAD",
        "git branch -f main HEAD",
        "git reset --hard x",
        "git cherry-pick x",
    ]);
}

#[test]
fn p8_session_master_via_variable_denied() {
    member_denied(&["sm=session-master; $sm next --session OTHER"]);
}
