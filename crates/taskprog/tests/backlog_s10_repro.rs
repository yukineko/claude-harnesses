// Reproduction tests for backlog items audited in shard s10-small-b.
// Each test is RED while its backlog item is an open defect.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

fn scratch(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let id = N.fetch_add(1, Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!("taskprog-s10-{tag}-{}-{id}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    // canonicalize: macOS /var -> /private/var
    d.canonicalize().unwrap()
}

fn repo(tag: &str) -> PathBuf {
    let d = scratch(tag);
    let o = Command::new("git")
        .args(["init", "-q"])
        .current_dir(&d)
        .output()
        .unwrap();
    assert!(o.status.success());
    d
}

fn run(cwd: &Path, home: &Path, args: &[&str], payload: &str) -> (i32, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_taskprog"))
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env_remove("TASKPROG_DISABLED")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(mut si) = child.stdin.take() {
        let _ = si.write_all(payload.as_bytes());
    }
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

/// b6fdf66b: a `taskprog.toml` at the project root must be honoured by a hook
/// whose cwd is a subdirectory (resolve_progress_path is root-anchored, so
/// Config::load must be too).
#[test]
#[ignore = "backlog b6fdf66b: open defect, remove ignore when fixed"]
fn backlog_b6fdf66b_config_at_project_root_is_read_from_a_subdir_cwd() {
    let root = repo("cfg");
    let home = scratch("cfg-home");
    let custom = root.join("custom-progress.md");
    std::fs::write(&custom, "MARKER_FROM_CUSTOM_PROGRESS_FILE\n").unwrap();
    std::fs::write(
        root.join("taskprog.toml"),
        format!("progress_file = \"{}\"\n", custom.display()),
    )
    .unwrap();
    let sub = root.join("crates").join("x");
    std::fs::create_dir_all(&sub).unwrap();
    let payload = |cwd: &Path| {
        format!(
            "{{\"hook_event_name\":\"SessionStart\",\"session_id\":\"s\",\"cwd\":\"{}\"}}",
            cwd.display()
        )
    };
    // Control: cwd == root reads the configured file.
    let (_, ctl) = run(&root, &home, &["session-start"], &payload(&root));
    assert!(
        ctl.contains("MARKER_FROM_CUSTOM_PROGRESS_FILE"),
        "apparatus control failed: {ctl}"
    );
    // Subject: cwd == subdir must see the same config.
    let (_, out) = run(&sub, &home, &["session-start"], &payload(&sub));
    assert!(
        out.contains("MARKER_FROM_CUSTOM_PROGRESS_FILE"),
        "subdir cwd ignored the root taskprog.toml: {out}"
    );
}

/// 977e7b2a: config.rs documents an empty cwd as "no information" (Undetermined)
/// but every hook path rewrites it to "." and anchors at the process cwd, so a
/// Stop whose stdin carried no cwd seeds a progress file. This test pins the
/// DOCUMENTED reading; which of doc/behaviour is right is a human ruling.
#[test]
#[ignore = "backlog 977e7b2a: open defect, remove ignore when fixed"]
fn backlog_977e7b2a_stop_with_empty_cwd_writes_nothing() {
    let root = repo("emptycwd");
    let home = scratch("emptycwd-home");
    let (code, out) = run(
        &root,
        &home,
        &["stop"],
        r#"{"hook_event_name":"Stop","session_id":"s10-empty"}"#,
    );
    eprintln!("code={code} out={out:?}");
    assert!(
        !root.join(".claude").join("progress.md").exists(),
        "empty-cwd Stop seeded {} despite the doc calling an empty cwd 'no information'",
        root.join(".claude/progress.md").display()
    );
}
