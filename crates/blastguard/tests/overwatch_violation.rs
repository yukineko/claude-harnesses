//! End-to-end tests for the fail-soft overwatch violation emission on deny:
//! a deny appends a violation line, the deny decision/stdout are byte-
//! identical with vs without the emit path being writable, and an
//! unwritable store never panics or changes the exit code.

use std::io::Write;
use std::process::{Command, Stdio};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_blastguard")
}

/// Run the binary with `payload` on stdin under the given `HOME`. Returns
/// (exit_code, stdout).
fn run_with_home(payload: &str, home: &std::path::Path) -> (i32, String) {
    let mut cmd = Command::new(bin());
    cmd.env("HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("binary spawns");
    if let Some(mut child_stdin) = child.stdin.take() {
        let _ = child_stdin.write_all(payload.as_bytes());
    }
    let out = child.wait_with_output().expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

fn deny_payload() -> String {
    r#"{"session_id":"sess-1","cwd":"/tmp/blastguard-violation-test-project","hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"rm -rf build"}}"#.to_string()
}

/// Locate the violations.jsonl file overwatch would write for a given HOME +
/// cwd, mirroring `overwatch::store::violations_path`'s layout
/// (`~/.overwatch/<project-key>/overwatch/violations.jsonl`) without linking
/// against overwatch's private path-resolution — we just glob for the file
/// under the fake HOME since only this test process's binary invocation can
/// have written anything there.
fn find_violations_file(home: &std::path::Path) -> Option<std::path::PathBuf> {
    let root = home.join(".overwatch");
    if !root.exists() {
        return None;
    }
    walk(&root)
        .into_iter()
        .find(|entry| entry.file_name().and_then(|n| n.to_str()) == Some("violations.jsonl"))
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                out.extend(walk(&path));
            } else {
                out.push(path);
            }
        }
    }
    out
}

#[test]
fn deny_appends_one_violation_line() {
    let tmp = tempdir();
    let (code, stdout) = run_with_home(&deny_payload(), tmp.path());
    assert_eq!(code, 0, "hook must always exit 0");
    assert!(
        stdout.contains(r#""permissionDecision":"deny""#),
        "expected deny, got: {stdout}"
    );

    let violations_file =
        find_violations_file(tmp.path()).expect("violations.jsonl must have been created");
    let contents = std::fs::read_to_string(&violations_file).expect("violations file readable");
    let lines: Vec<&str> = contents.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(
        lines.len(),
        1,
        "exactly one violation line expected, got: {contents}"
    );

    let ev: serde_json::Value = serde_json::from_str(lines[0]).expect("valid JSON line");
    assert_eq!(ev["source"], "blastguard");
    assert_eq!(ev["signature"], "blastguard:rm-recursive");
    assert_eq!(ev["session_id"], "sess-1");
    assert!(ev["task_key"].as_str().unwrap().contains("Bash"));
}

#[test]
fn allow_emits_no_violation() {
    let tmp = tempdir();
    let payload = r#"{"session_id":"sess-2","cwd":"/tmp/blastguard-violation-test-project","hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"cargo test"}}"#;
    let (code, stdout) = run_with_home(payload, tmp.path());
    assert_eq!(code, 0);
    assert!(stdout.trim().is_empty(), "allow must be silent");
    assert!(
        find_violations_file(tmp.path()).is_none(),
        "allow must not create a violations file"
    );
}

#[test]
fn deny_decision_is_byte_identical_regardless_of_store_writability() {
    // Writable HOME (emit succeeds).
    let writable = tempdir();
    let (code_w, stdout_w) = run_with_home(&deny_payload(), writable.path());

    // Unwritable HOME (emit must fail-soft: same decision, same bytes).
    let unwritable = tempdir();
    let overwatch_dir = unwritable.path().join(".overwatch");
    std::fs::create_dir_all(&overwatch_dir).unwrap();
    let mut perms = std::fs::metadata(&overwatch_dir).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o400); // read-only, no write/exec
    std::fs::set_permissions(&overwatch_dir, perms.clone()).unwrap();

    let (code_u, stdout_u) = run_with_home(&deny_payload(), unwritable.path());

    // Restore perms so tempdir cleanup can remove it.
    let mut restore = perms.clone();
    std::os::unix::fs::PermissionsExt::set_mode(&mut restore, 0o700);
    let _ = std::fs::set_permissions(&overwatch_dir, restore);

    assert_eq!(code_w, 0);
    assert_eq!(code_u, 0, "unwritable store must not change the exit code");
    assert_eq!(
        stdout_w, stdout_u,
        "deny decision/stdout must be byte-identical regardless of store writability"
    );
    assert!(stdout_u.contains(r#""permissionDecision":"deny""#));
}

#[test]
fn unwritable_store_does_not_panic_and_stays_exit_zero() {
    let home = tempdir();
    let overwatch_dir = home.path().join(".overwatch");
    std::fs::create_dir_all(&overwatch_dir).unwrap();
    let mut perms = std::fs::metadata(&overwatch_dir).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o400);
    std::fs::set_permissions(&overwatch_dir, perms.clone()).unwrap();

    let mut cmd = Command::new(bin());
    cmd.env("HOME", home.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("binary spawns");
    if let Some(mut child_stdin) = child.stdin.take() {
        let _ = child_stdin.write_all(deny_payload().as_bytes());
    }
    let out = child.wait_with_output().expect("binary runs");

    let mut restore = perms;
    std::os::unix::fs::PermissionsExt::set_mode(&mut restore, 0o700);
    let _ = std::fs::set_permissions(&overwatch_dir, restore);

    assert_eq!(
        out.status.code(),
        Some(0),
        "must exit 0 even when unwritable"
    );
    assert!(
        out.status.success(),
        "process must not abort/crash on unwritable store"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains(r#""permissionDecision":"deny""#));
}

// ---------------------------------------------------------------------------
// Minimal tempdir helper (avoid pulling in a dev-dependency just for this).
// ---------------------------------------------------------------------------

struct TempDir(std::path::PathBuf);

impl TempDir {
    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tempdir() -> TempDir {
    let mut dir = std::env::temp_dir();
    let unique = format!(
        "blastguard-overwatch-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    dir.push(unique);
    std::fs::create_dir_all(&dir).unwrap();
    TempDir(dir)
}
