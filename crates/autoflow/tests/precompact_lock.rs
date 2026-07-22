//! End-to-end regression: PreCompact only resumes `/flow` when THIS session's
//! *own project* holds a live backlog lock. Drives the real `autoflow` and
//! `backlog` binaries together (rather than unit-testing in isolation) because
//! `autoflow::lock` now shells out to `backlog lock status --project <p>`
//! instead of reading a lock file directly — the actual per-project CLI
//! contract can only be exercised end-to-end.
//!
//! The core case (`precompact_does_not_resume_for_an_unrelated_projects_lock`)
//! is the regression this file exists for: before the backlog lock became
//! per-project, ANY live lock (regardless of which project acquired it) made
//! `autoflow` believe "this session holds the lock" as long as the session id
//! matched, and a live lock on an unrelated project must never resume `/flow`
//! for a different project's session.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Locate `backlog`'s built binary. `backlog` has no lib target, so Cargo
/// won't wire up `CARGO_BIN_EXE_backlog` for a cross-package dependency on
/// stable (that env var is same-package-only; artifact-dependencies that
/// would fix this require nightly `-Z bindeps`). Instead, resolve it the way
/// `assert_cmd::cargo_bin` does: this test binary lives at
/// `target/<profile>/deps/<name>-<hash>`, and workspace binaries built via
/// `cargo build -p backlog` (or `cargo test --workspace`) land as a sibling at
/// `target/<profile>/backlog`.
fn backlog_bin() -> PathBuf {
    let mut dir = std::env::current_exe().expect("current_exe");
    dir.pop(); // deps/
    if dir.ends_with("deps") {
        dir.pop(); // <profile>/
    }
    let candidate = dir.join(format!("backlog{}", std::env::consts::EXE_SUFFIX));
    assert!(
        candidate.exists(),
        "expected a built `backlog` binary at {candidate:?} — run `cargo build -p backlog` (or `cargo test --workspace`) first"
    );
    candidate
}

fn temp_home(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "autoflow-precompact-lock-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The built `backlog` binary's containing directory, prepended to PATH so
/// the `autoflow` subprocess's `find_backlog_binary()` (PATH-first lookup)
/// resolves it deterministically regardless of the host's own PATH.
fn path_with_backlog_bin() -> std::ffi::OsString {
    let dir = backlog_bin()
        .parent()
        .expect("backlog bin has a parent dir")
        .to_path_buf();
    match std::env::var_os("PATH") {
        Some(p) => {
            let mut dirs = vec![dir];
            dirs.extend(std::env::split_paths(&p));
            std::env::join_paths(dirs).expect("join PATH")
        }
        None => dir.into_os_string(),
    }
}

fn run_backlog(args: &[&str], home: &Path) -> (i32, String) {
    let out = Command::new(backlog_bin())
        .args(args)
        .env("HOME", home)
        .output()
        .expect("backlog runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

fn run_autoflow(args: &[&str], payload: &str, home: &Path) -> (i32, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_autoflow"))
        .args(args)
        .env("HOME", home)
        .env("PATH", path_with_backlog_bin())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("autoflow spawns");
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(payload.as_bytes());
    }
    let out = child.wait_with_output().expect("autoflow runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

fn precompact_payload(session_id: &str, cwd: &Path) -> String {
    format!(
        r#"{{"hook_event_name":"PreCompact","session_id":"{}","cwd":"{}"}}"#,
        session_id,
        cwd.to_string_lossy()
    )
}

fn prompt_submit_payload(session_id: &str) -> String {
    format!(r#"{{"hook_event_name":"UserPromptSubmit","session_id":"{session_id}"}}"#)
}

#[test]
fn precompact_resumes_flow_only_when_this_session_holds_its_own_project_lock() {
    let home = temp_home("own-project");
    let project = home.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let project_str = project.to_string_lossy().into_owned();
    let sess = "sess-own";

    // (a) No lock at all -> PreCompact drops no marker -> prompt-submit silent.
    let (code, _) = run_autoflow(&["pre-compact"], &precompact_payload(sess, &project), &home);
    assert_eq!(code, 0, "pre-compact must always exit 0");
    let (code, out) = run_autoflow(&["prompt-submit"], &prompt_submit_payload(sess), &home);
    assert_eq!(code, 0);
    assert!(
        out.trim().is_empty(),
        "no lock -> no resume injected, got: {out}"
    );

    // (b) Lock held by a DIFFERENT session (same project) -> still no marker.
    let (code, _) = run_backlog(
        &[
            "lock",
            "acquire",
            "--session-id",
            "sess-other",
            "--project",
            &project_str,
        ],
        &home,
    );
    assert_eq!(code, 0, "acquire for sess-other must succeed");
    let (code, _) = run_autoflow(&["pre-compact"], &precompact_payload(sess, &project), &home);
    assert_eq!(code, 0);
    let (code, out) = run_autoflow(&["prompt-submit"], &prompt_submit_payload(sess), &home);
    assert_eq!(code, 0);
    assert!(
        out.trim().is_empty(),
        "other session's lock -> no resume, got: {out}"
    );

    // (c) Lock re-acquired (forced) by THIS session, same project -> marker
    // written, and prompt-submit injects the resume instruction exactly once.
    let (code, _) = run_backlog(
        &[
            "lock",
            "acquire",
            "--session-id",
            sess,
            "--project",
            &project_str,
            "--force",
        ],
        &home,
    );
    assert_eq!(
        code, 0,
        "forced acquire for the owning session must succeed"
    );
    let (code, _) = run_autoflow(&["pre-compact"], &precompact_payload(sess, &project), &home);
    assert_eq!(code, 0);
    let (code, out) = run_autoflow(&["prompt-submit"], &prompt_submit_payload(sess), &home);
    assert_eq!(code, 0);
    assert!(
        out.contains("/flow"),
        "own project's live lock -> resume injected, got: {out}"
    );
    let (code, out) = run_autoflow(&["prompt-submit"], &prompt_submit_payload(sess), &home);
    assert_eq!(code, 0);
    assert!(
        out.trim().is_empty(),
        "marker consumed once -> second prompt-submit is silent, got: {out}"
    );
}

// Core regression: the backlog lock is scoped per-project, so a live lock for
// an UNRELATED project (even held by the same session id) must never make
// autoflow think THIS project's session holds a lock and resume /flow for it.
#[test]
fn precompact_does_not_resume_for_an_unrelated_projects_lock() {
    let home = temp_home("cross-project");
    let project_a = home.join("project-a");
    let project_b = home.join("project-b");
    std::fs::create_dir_all(&project_a).unwrap();
    std::fs::create_dir_all(&project_b).unwrap();
    let sess = "sess-a";

    // sess-a holds the lock for project-b...
    let (code, _) = run_backlog(
        &[
            "lock",
            "acquire",
            "--session-id",
            sess,
            "--project",
            &project_b.to_string_lossy(),
        ],
        &home,
    );
    assert_eq!(code, 0);

    // ...but PreCompact fires for project-a. It must not resume.
    let (code, _) = run_autoflow(
        &["pre-compact"],
        &precompact_payload(sess, &project_a),
        &home,
    );
    assert_eq!(code, 0);
    let (code, out) = run_autoflow(&["prompt-submit"], &prompt_submit_payload(sess), &home);
    assert_eq!(code, 0);
    assert!(
        out.trim().is_empty(),
        "project-b's lock must not resume project-a's session, got: {out}"
    );
}
