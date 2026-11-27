#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod support;
use support::*;

#[test]
fn sole_session_is_master() {
    let sb = Sandbox::new();
    assert_eq!(sb.register("A", T0).code, 0);
    let o = sb.role("A", T0);
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    assert_eq!(o.stdout.trim(), "master");
}

#[test]
fn second_session_is_member() {
    let sb = Sandbox::new();
    assert_eq!(sb.register("A", T0).code, 0);
    assert_eq!(sb.register("B", T0 + 1).code, 0);
    let o = sb.role("B", T0 + 2);
    assert_eq!(o.code, 1);
    assert_eq!(o.stdout.trim(), "member");
    let st = sb.status("B", T0 + 2);
    assert_eq!(st["master"]["session"], "A");
}

#[test]
fn oldest_live_member_promoted_after_ttl() {
    let sb = Sandbox::new();
    sb.register("A", T0);
    sb.register("B", T0 + 10);
    sb.register("C", T0 + 20);
    // A stops heartbeating; B and C keep going. TTL=60.
    let now = T0 + 70;
    assert_eq!(sb.sm(&["heartbeat"], "B", now).code, 0);
    assert_eq!(sb.sm(&["heartbeat"], "C", now).code, 0);
    assert_eq!(sb.role("B", now).code, 0);
    assert_eq!(sb.role("C", now).code, 1);
    assert_eq!(sb.role("A", now).code, 1);
}

#[test]
fn worktree_session_joins_same_group() {
    let sb = Sandbox::new();
    let wt = sb.worktree("wt-feature", "feature");
    let a = sb.register("A", T0).json();
    let o = sb.sm_in(&["register"], "B", T0 + 1, &wt);
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    let b = o.json();
    assert_eq!(a["group"], b["group"]);
    assert_eq!(b["role"], "member");
    let st = sb.status("A", T0 + 2);
    assert_eq!(st["members"].as_array().unwrap().len(), 2);
}

#[test]
fn undeterminable_master_liveness_does_not_promote() {
    let sb = Sandbox::new();
    // A's last_heartbeat is in the future relative to the later clock -> undetermined.
    assert_eq!(sb.register("A", T0 + 1000).code, 0);
    assert_eq!(sb.register("B", T0).code, 0);
    let now = T0 + 100;
    let o = sb.role("B", now);
    assert_eq!(o.code, 1, "B must stay member; stdout {}", o.stdout);
    let o = sb.role("A", now);
    assert_eq!(o.code, 0, "A stays master; stdout {}", o.stdout);
}

#[test]
fn invalid_now_is_undetermined() {
    let sb = Sandbox::new();
    sb.register("A", T0);
    let o = sb.run_with(
        &["role", "--session", "A", "--cwd", sb.repo.to_str().unwrap()],
        "not-a-number",
        &[],
        None,
    );
    assert_eq!(o.code, 2, "stdout {} stderr {}", o.stdout, o.stderr);
}
