// Reproduction test for backlog 09346035 (runbook half; audit shard s10-small-b).
// RED while the item is an open defect.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn scratch() -> PathBuf {
    let d = std::env::temp_dir().join(format!("runbook-s10-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let o = Command::new("git")
        .args(["init", "-q"])
        .current_dir(&d)
        .output()
        .unwrap();
    assert!(o.status.success());
    d
}

fn run(home: &Path, args: &[&str], stdin: &str) -> (i32, String, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_runbook"))
        .args(args)
        .current_dir(home)
        .env("HOME", home)
        .env_remove("RUNBOOK_DISABLE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let o = child.wait_with_output().unwrap();
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

/// 09346035: an UNREADABLE runbook dir must not look exactly like "no
/// runbook matched" to the hook's consumer. Control: readable -> injects.
#[cfg(unix)]
#[test]
#[ignore = "backlog 09346035: open defect, remove ignore when fixed"]
fn backlog_09346035_runbook_inject_says_something_when_store_unreadable() {
    use std::os::unix::fs::PermissionsExt;
    let h = scratch();
    let (rc, out, err) = run(&h, &["new", "zebra", "--description", "zebra proc"], "");
    assert_eq!(rc, 0, "{out} {err}");
    let payload = format!(
        "{{\"hook_event_name\":\"UserPromptSubmit\",\"session_id\":\"s\",\"cwd\":\"{}\",\"prompt\":\"please run !zebra now\"}}",
        h.display()
    );
    let (_, ctl, _) = run(&h, &["inject"], &payload);
    assert!(
        !ctl.trim().is_empty(),
        "apparatus control (readable store must inject): {ctl:?}"
    );

    let dir = h.join(".runbook");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000)).unwrap();
    let denied = std::fs::read_dir(&dir).is_err();
    let (rc, out, err) = run(&h, &["inject"], &payload);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(denied, "apparatus: mode 000 did not deny this uid (root?)");
    assert_eq!(rc, 0);
    eprintln!("stdout={out:?} stderr={err:?}");
    assert!(
        !(out.trim().is_empty() && err.trim().is_empty()),
        "unreadable runbook dir: hook produced no stdout and no stderr"
    );
}
