#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Verifier-found gate bypasses (F1..F3). Black-box RED tests.
mod support;
use support::*;

const NOW: i64 = T0 + 5;

fn assert_member_denied(cmds: &[&str]) {
    for cmd in cmds {
        let sb = Sandbox::new();
        sb.master_and_member();
        let o = sb.gate_bash("B", &sb.repo, cmd, NOW);
        assert_eq!(o.code, 0, "{cmd}: {}", o.stderr);
        assert!(is_deny(&o), "member must be denied for {cmd:?}: stdout {:?}", o.stdout);
    }
}

fn assert_member_allowed(cmds: &[&str]) {
    for cmd in cmds {
        let sb = Sandbox::new();
        sb.master_and_member();
        let o = sb.gate_bash("B", &sb.repo, cmd, NOW);
        assert_eq!(o.code, 0, "{cmd}: {}", o.stderr);
        assert!(!is_deny(&o), "member must be allowed for {cmd:?}: stdout {:?}", o.stdout);
    }
}

// F1: git literal, subcommand non-literal => undetermined => Deny.
#[test]
fn f1_nonliteral_git_subcommand_denied() {
    assert_member_denied(&["X=push; git $X", "git \"$@\"", "git ${SUB}"]);
}

// F2: member must not call master-only session-master subcommands via Bash.
#[test]
fn f2_member_next_as_master_id_denied() {
    assert_member_denied(&["session-master next --session A"]);
}

#[test]
fn f2_member_role_with_spoofed_env_denied() {
    assert_member_denied(&["CLAUDE_CODE_SESSION_ID=A session-master role"]);
}

#[test]
fn f2_member_complete_denied() {
    assert_member_denied(&["session-master complete q1 --outcome ok --session A"]);
}

#[test]
fn f2_member_leave_other_denied() {
    assert_member_denied(&["session-master leave --session A"]);
}

#[test]
fn f2_member_next_via_path_or_wrapper_denied() {
    assert_member_denied(&[
        "/usr/local/bin/session-master next",
        "env session-master next",
        "bash -c 'session-master next --session A'",
    ]);
}

// F3: push-equivalents.
#[test]
fn f3_member_send_pack_denied() {
    assert_member_denied(&["git send-pack x"]);
}

#[test]
fn f3_member_ssh_git_push_denied() {
    assert_member_denied(&["ssh host git push"]);
}

// Controls: must stay allowed.
#[test]
fn control_member_benign_allowed() {
    assert_member_allowed(&[
        "git status",
        "git commit -m \"git push\"",
        "session-master status",
        "session-master request --kind push --command 'git push'",
    ]);
}

#[test]
fn control_master_push_allowed() {
    let sb = Sandbox::new();
    sb.master_and_member();
    let o = sb.gate_bash("A", &sb.repo, "git push", NOW);
    assert_eq!(o.code, 0);
    assert!(!is_deny(&o), "stdout {:?}", o.stdout);
}
