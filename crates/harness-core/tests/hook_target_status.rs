#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `hook_target_status` / `hook_target_findings` (GH #1483 / #1484): a hook
//! target that cannot be checked is `Unverifiable` (never `Present`), and a
//! direct-exec target that exists without the exec bit is `NotExecutable`.

use std::os::unix::fs::PermissionsExt;

use harness_core::hook::{hook_target_findings, hook_target_status, HookTargetStatus};
use serde_json::json;

fn file_with_mode(dir: &std::path::Path, name: &str, mode: u32) -> String {
    let p = dir.join(name);
    std::fs::write(&p, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
    p.to_string_lossy().into_owned()
}

#[test]
fn direct_exec_target_without_exec_bit_is_not_executable() {
    let dir = tempfile::tempdir().unwrap();
    let path = file_with_mode(dir.path(), "hook-bin", 0o644);
    assert_eq!(
        hook_target_status(&format!("{path} check"), None),
        HookTargetStatus::NotExecutable {
            binary_path: path.clone()
        }
    );
}

#[test]
fn executable_direct_target_is_present() {
    let dir = tempfile::tempdir().unwrap();
    let path = file_with_mode(dir.path(), "hook-bin", 0o755);
    assert_eq!(
        hook_target_status(&format!("{path} check"), None),
        HookTargetStatus::Present
    );
}

#[test]
fn interpreter_wrapped_script_does_not_need_exec_bit() {
    let dir = tempfile::tempdir().unwrap();
    let path = file_with_mode(dir.path(), "guard.sh", 0o644);
    assert_eq!(
        hook_target_status(&format!("bash {path}"), None),
        HookTargetStatus::Present
    );
}

#[test]
fn absent_target_is_missing() {
    assert_eq!(
        hook_target_status("/no/such/dir/ghost run", None),
        HookTargetStatus::Missing {
            binary_path: "/no/such/dir/ghost".to_string()
        }
    );
}

#[test]
fn every_unresolvable_shape_is_unverifiable_never_present() {
    for cmd in [
        "bash hooks/gaurd.sh",
        "python3 scripts/x.py",
        "bash -c 'exit 0'",
        "python3 -m json.tool",
        "${CLAUDE_PLUGIN_ROOT}/bin/x run",
        "$SOME_UNKNOWN_VAR/bin/x",
        "cd /tmp",
        "./relative/bin",
        "",
        "   ",
    ] {
        match hook_target_status(cmd, None) {
            HookTargetStatus::Unverifiable { reason } => {
                assert!(!reason.is_empty(), "empty reason for {cmd:?}");
            }
            other => panic!("{cmd:?} must be Unverifiable, got {other:?}"),
        }
    }
}

#[test]
fn findings_report_every_non_present_hook_with_its_event() {
    let dir = tempfile::tempdir().unwrap();
    let noexec = file_with_mode(dir.path(), "noexec", 0o644);
    let ok = file_with_mode(dir.path(), "ok", 0o755);
    let settings = json!({"hooks": {
        "Stop": [{"hooks": [
            {"type": "command", "command": format!("{ok} run")},
            {"type": "command", "command": "bash hooks/gaurd.sh"},
            {"type": "command", "command": format!("{noexec} run")},
            {"type": "command", "command": "/no/such/ghost"}
        ]}]
    }});
    let f = hook_target_findings(&settings, None);
    let statuses: Vec<_> = f.iter().map(|x| (&x.event[..], &x.status)).collect();
    assert_eq!(f.len(), 3, "{f:?}");
    assert!(statuses.iter().all(|(e, _)| *e == "Stop"));
    assert!(matches!(f[0].status, HookTargetStatus::Unverifiable { .. }));
    assert!(matches!(
        f[1].status,
        HookTargetStatus::NotExecutable { .. }
    ));
    assert!(matches!(f[2].status, HookTargetStatus::Missing { .. }));
}

#[test]
fn malformed_hook_structure_is_unverifiable_not_clean() {
    let unverifiable = |s: serde_json::Value| {
        let f = hook_target_findings(&s, None);
        f.len() == 1 && matches!(f[0].status, HookTargetStatus::Unverifiable { .. })
    };
    assert!(unverifiable(json!("not an object")), "non-object settings");
    assert!(unverifiable(json!({"hooks": "x"})), "non-object hooks");
    assert!(
        unverifiable(json!({"hooks": {"Stop": "x"}})),
        "non-array event"
    );
    assert!(
        unverifiable(json!({"hooks": {"Stop": [{"hooks": "x"}]}})),
        "group without hooks array"
    );
    assert!(
        unverifiable(json!({"hooks": {"Stop": [{"hooks": [{"type": "command"}]}]}})),
        "command hook without command"
    );
    // Genuinely empty / non-command hooks: nothing to check, no finding.
    assert!(hook_target_findings(&json!({}), None).is_empty());
    assert!(hook_target_findings(
        &json!({"hooks": {"Stop": [{"hooks": [{"type": "prompt", "prompt": "x"}]}]}}),
        None
    )
    .is_empty());
}
