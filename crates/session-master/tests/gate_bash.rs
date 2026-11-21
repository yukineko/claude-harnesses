#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod support;
use support::*;

const NOW: i64 = T0 + 5;

#[test]
fn member_push_denied_and_queued_once() {
    let sb = Sandbox::new();
    sb.master_and_member();
    let o = sb.gate_bash("B", &sb.repo, "git push origin HEAD", NOW);
    assert_eq!(o.code, 0);
    assert!(is_deny(&o), "stdout {}", o.stdout);
    assert!(deny_reason(&o).contains("queue"));
    let st = sb.status("B", NOW);
    let q = st["queue"].as_array().unwrap();
    assert_eq!(q.len(), 1);
    assert_eq!(q[0]["kind"], "push");
    assert_eq!(q[0]["status"], "pending");
    assert_eq!(q[0]["requester"], "B");
    // identical request again: deduped
    let o2 = sb.gate_bash("B", &sb.repo, "git push origin HEAD", NOW + 1);
    assert!(is_deny(&o2));
    assert_eq!(sb.status("B", NOW + 1)["queue"].as_array().unwrap().len(), 1);
}

#[test]
fn master_push_not_denied() {
    let sb = Sandbox::new();
    sb.master_and_member();
    let o = sb.gate_bash("A", &sb.repo, "git push origin HEAD", NOW);
    assert_eq!(o.code, 0);
    assert!(!o.stdout.contains("deny"), "stdout {}", o.stdout);
}

#[test]
fn member_non_push_commands_allowed() {
    let sb = Sandbox::new();
    sb.master_and_member();
    for cmd in [
        "echo git push",
        "git commit -m \"git push する\"",
        "git status",
        "cat <<'EOF'\ngit push\nEOF",
    ] {
        let o = sb.gate_bash("B", &sb.repo, cmd, NOW);
        assert_eq!(o.code, 0, "{cmd}");
        assert!(!is_deny(&o), "must allow {cmd:?}: {}", o.stdout);
    }
}

#[test]
fn member_hidden_push_and_rollout_denied() {
    let sb = Sandbox::new();
    sb.master_and_member();
    for cmd in [
        "cd x && git push",
        "FOO=1 git -C . push",
        "bash -c 'git push'",
        "echo $(git push)",
        "scripts/rollout-plugins.sh",
        "bash scripts/rollout-plugins.sh",
    ] {
        let o = sb.gate_bash("B", &sb.repo, cmd, NOW);
        assert_eq!(o.code, 0, "{cmd}");
        assert!(is_deny(&o), "must deny {cmd:?}: stdout {}", o.stdout);
    }
}

#[test]
fn member_merge_depends_on_checked_out_branch() {
    let sb = Sandbox::new();
    let wt = sb.worktree("wt-feature", "feature");
    sb.master_and_member();
    let o = sb.gate_bash("B", &sb.repo, "git merge feature", NOW);
    assert!(is_deny(&o), "merge on main must deny: {}", o.stdout);
    let o = sb.gate_bash("B", &wt, "git merge main", NOW);
    assert_eq!(o.code, 0);
    assert!(!is_deny(&o), "merge on feature must allow: {}", o.stdout);
}

#[test]
fn undeterminable_state_fails_closed() {
    let sb = Sandbox::new();
    // state root under a regular file -> cannot read/write state
    let file = sb.home.join("blocker");
    std::fs::write(&file, "x").unwrap();
    let bad = file.join("sub");
    let p = bash_payload("B", &sb.repo, "git push origin HEAD");
    let o = sb.run_with(
        &["gate"],
        &NOW.to_string(),
        &[("SESSION_MASTER_HOME", bad.to_str().unwrap())],
        Some(&p.to_string()),
    );
    assert!(is_deny(&o), "stdout {} stderr {}", o.stdout, o.stderr);
}

#[test]
fn push_outside_any_repo_denied() {
    let sb = Sandbox::new();
    let outside = tempfile::TempDir::new().unwrap();
    let o = sb.gate_bash("B", outside.path(), "git push origin HEAD", NOW);
    assert!(is_deny(&o), "stdout {} stderr {}", o.stdout, o.stderr);
}

#[test]
fn invalid_json_payload_denied() {
    let sb = Sandbox::new();
    let o = sb.run_with(&["gate"], &NOW.to_string(), &[], Some("not json {"));
    assert!(is_deny(&o), "stdout {} stderr {}", o.stdout, o.stderr);
}
