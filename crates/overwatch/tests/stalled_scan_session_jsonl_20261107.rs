// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (2026-11-07): `stalled::scan` treats every entry of the project transcript
//! dir as a session dir, including the `<session>.jsonl` FILES Claude Code writes
//! beside the `<session>/` dirs, and reports an Unknown row
//! "cannot list .../<id>.jsonl/subagents: Not a directory" for each
//! (281 such rows seen in `overwatch status --json` for this repo).

use overwatch::stalled::{scan, StallState};
use std::time::SystemTime;

#[test]
fn session_jsonl_file_beside_session_dir_yields_no_unknown_row() {
    let home = std::env::temp_dir().join(format!("ow-stalled-jsonl-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let project = std::path::PathBuf::from("/tmp/ow-stalled-jsonl-project");
    let proj_dir = home
        .join(".claude/projects")
        .join(overwatch::stalled::slug(&project));
    let sid = "0d3d5bb8-0000-4000-8000-000000000001";
    std::fs::create_dir_all(proj_dir.join(sid).join("subagents")).unwrap();
    std::fs::write(proj_dir.join(format!("{sid}.jsonl")), "{}\n").unwrap();

    let rows = scan(&home, &project, SystemTime::now());
    let unknown: Vec<_> = rows
        .iter()
        .filter(|r| matches!(r.state, StallState::Unknown { .. }))
        .collect();
    assert!(
        unknown.is_empty(),
        "a <sid>.jsonl file produced Unknown rows: {unknown:?}"
    );
}

#[cfg(unix)]
fn fresh(tag: &str) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    let home = std::env::temp_dir().join(format!("ow-stalled-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let project = std::path::PathBuf::from(format!("/tmp/ow-stalled-{tag}-project"));
    let proj_dir = home
        .join(".claude/projects")
        .join(overwatch::stalled::slug(&project));
    std::fs::create_dir_all(&proj_dir).unwrap();
    (home, project, proj_dir)
}

#[cfg(unix)]
#[test]
fn unreadable_real_session_subagents_dir_still_yields_unknown() {
    use std::os::unix::fs::PermissionsExt;
    let (home, project, proj_dir) = fresh("perm");
    let sub = proj_dir.join("sid1").join("subagents");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read_dir(&sub).is_ok() {
        // running as root: chmod 000 does not block the read; nothing to test.
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }
    let rows = scan(&home, &project, SystemTime::now());
    std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        rows.iter()
            .any(|r| matches!(r.state, StallState::Unknown { .. })),
        "unreadable subagents dir was dropped: {rows:?}"
    );
}

#[cfg(unix)]
#[test]
fn entry_with_undeterminable_type_is_unknown_not_skipped() {
    // A dangling symlink: stat fails (NotFound), so the type is undeterminable.
    let (home, project, proj_dir) = fresh("dangling");
    std::os::unix::fs::symlink(proj_dir.join("nonexistent-target"), proj_dir.join("sid2")).unwrap();
    let rows = scan(&home, &project, SystemTime::now());
    assert!(
        rows.iter()
            .any(|r| matches!(r.state, StallState::Unknown { .. })),
        "undeterminable entry was silently dropped: {rows:?}"
    );
}
