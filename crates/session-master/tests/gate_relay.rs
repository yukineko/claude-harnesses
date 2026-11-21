#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod support;
use support::*;

const NOW: i64 = T0 + 5;

fn setup() -> Sandbox {
    let sb = Sandbox::new();
    assert_eq!(sb.register("A", T0).code, 0);
    assert_eq!(sb.register("B", T0 + 1).code, 0);
    assert_eq!(sb.register("C", T0 + 2).code, 0);
    assert_eq!(sb.sm(&["name", "C-name"], "C", T0 + 3).code, 0);
    assert_eq!(sb.sm(&["name", "A-name"], "A", T0 + 3).code, 0);
    sb
}

#[test]
fn member_to_member_denied() {
    let sb = setup();
    let o = sb.gate_send("B", &sb.repo, "C-name", NOW);
    assert_eq!(o.code, 0);
    assert!(is_deny(&o), "stdout {}", o.stdout);
}

#[test]
fn member_to_master_allowed() {
    let sb = setup();
    let o = sb.gate_send("B", &sb.repo, "A-name", NOW);
    assert_eq!(o.code, 0);
    assert!(!is_deny(&o), "stdout {}", o.stdout);
}

#[test]
fn master_to_member_allowed() {
    let sb = setup();
    let o = sb.gate_send("A", &sb.repo, "C-name", NOW);
    assert_eq!(o.code, 0);
    assert!(!is_deny(&o), "stdout {}", o.stdout);
}

#[test]
fn member_to_main_allowed() {
    let sb = setup();
    let o = sb.gate_send("B", &sb.repo, "main", NOW);
    assert_eq!(o.code, 0);
    assert!(!is_deny(&o), "stdout {}", o.stdout);
}

#[test]
fn member_to_unknown_denied() {
    let sb = setup();
    let o = sb.gate_send("B", &sb.repo, "nobody-knows", NOW);
    assert!(is_deny(&o), "stdout {}", o.stdout);
}
