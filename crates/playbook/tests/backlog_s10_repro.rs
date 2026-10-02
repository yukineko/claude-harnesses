// Reproduction test for backlog 09346035 (playbook half; audit shard s10-small-b).
// RED while the item is an open defect.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn scratch() -> PathBuf {
    let d = std::env::temp_dir().join(format!("playbook-s10-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    // macOS: /var -> /private/var; `add` keys the store on current_dir()
    // (canonical), the hook payload must name the same spelling.
    d.canonicalize().unwrap()
}

fn run(home: &Path, args: &[&str], stdin: &str) -> (i32, String, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_playbook"))
        .args(args)
        .current_dir(home)
        .env("HOME", home)
        .env_remove("PLAYBOOK_DISABLE")
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

fn find_md_parent(dir: &Path) -> Option<PathBuf> {
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        if p.is_dir() {
            if let Some(f) = find_md_parent(&p) {
                return Some(f);
            }
        } else if p.extension().is_some_and(|x| x == "md") {
            return p.parent().map(Path::to_path_buf);
        }
    }
    None
}

/// 09346035: an UNREADABLE note store must not look exactly like an EMPTY one
/// to the hook's consumer (the model). Control: a readable store injects.
#[cfg(unix)]
#[test]
#[ignore = "backlog 09346035: open defect, remove ignore when fixed"]
fn backlog_09346035_playbook_inject_says_something_when_store_unreadable() {
    use std::os::unix::fs::PermissionsExt;
    let h = scratch();
    let (rc, out, err) = run(
        &h,
        &[
            "add",
            "--title",
            "Zebra Rule",
            "--trigger",
            "zebra",
            "--always",
            "--body",
            "ZEBRA_NOTE_BODY",
        ],
        "",
    );
    assert_eq!(rc, 0, "{out} {err}");
    let payload = format!(
        "{{\"hook_event_name\":\"UserPromptSubmit\",\"session_id\":\"s\",\"cwd\":\"{}\",\"prompt\":\"tell me about the zebra\"}}",
        h.display()
    );
    let (_, ctl, _) = run(&h, &["inject"], &payload);
    assert!(
        ctl.contains("ZEBRA_NOTE_BODY"),
        "apparatus control (readable store must inject): {ctl:?}"
    );

    let dir = find_md_parent(&h.join(".playbook")).expect("note dir");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000)).unwrap();
    let denied = std::fs::read_dir(&dir).is_err();
    let (rc, out, err) = run(&h, &["inject"], &payload);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(denied, "apparatus: mode 000 did not deny this uid (root?)");
    assert_eq!(rc, 0);
    eprintln!("stdout={out:?} stderr={err:?}");
    assert!(
        !(out.trim().is_empty() && err.trim().is_empty()),
        "unreadable store: hook produced no stdout and no stderr (indistinguishable from an empty store)"
    );
}
