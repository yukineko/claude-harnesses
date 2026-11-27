#![allow(clippy::expect_used)]
//! RED (backlog, pre-existing on base): a DEBUG trap that changes directory
//! makes a later relative write land in a system dir. Measured in a real
//! shell (bash / zsh, 2026-11-28): `trap 'cd /etc' DEBUG; echo hi > ok.log`
//! changed cwd (bcwd:/usr, zcwd:/usr in the measurement harness), so the
//! relative redirect target `ok.log` is NOT under the confined cwd.
//! Today this is Allow; it must be non-Allow in main and Deny in a subagent.

use blastguard::detect;
use blastguard::model::Decision;
use blastguard::scope::SafeRoots;
use serde_json::json;

fn identity(p: &str) -> Option<String> {
    Some(p.to_string())
}

fn scoped(cmd: &str) -> Decision {
    let roots = SafeRoots::new(
        Some("/home/yuki/proj"),
        Some("/home/yuki/proj"),
        Some("/home/yuki"),
        None,
        Some(identity),
    );
    detect::detect_scoped("Bash", Some(&json!({ "command": cmd })), &roots)
}

#[test]
#[ignore = "RED(21d05411): DEBUG trap that cd's to a system dir lets a later relative write land there"]
fn debug_trap_cd_then_relative_write_is_never_allow() {
    let mut bad = Vec::new();
    for c in [
        "trap 'cd /etc' DEBUG; echo hi > ok.log",
        "trap 'cd /usr' DEBUG; echo hi > ok.log",
    ] {
        let d = scoped(c);
        let h = d.clone().hardened();
        if matches!(d, Decision::Allow) || !h.is_deny() {
            bad.push(format!("main={d:?} sub={h:?} :: {c:?}"));
        }
    }
    assert!(
        bad.is_empty(),
        "relative write after cwd-changing DEBUG trap:\n{}",
        bad.join("\n")
    );
}
