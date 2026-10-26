#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! A Write/Edit inside THIS session's Claude Code scratchpad is not a root and
//! not an error; every other write outside git stays Undetermined.

use harness_core::session_roots::resolve;
use harness_core::verdict::Determination;
use std::path::{Path, PathBuf};
use std::process::Command;

const SID: &str = "97394877-cf10-43ad-a4a8-807e13a118aa";
const OTHER_SID: &str = "11111111-2222-3333-4444-555555555555";

fn git_init(dir: &Path) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let st = Command::new("git")
        .arg("init")
        .arg("-q")
        .arg(dir)
        .status()
        .unwrap();
    assert!(st.success());
    std::fs::canonicalize(dir).unwrap()
}

/// Creates `<base>/<rel_dir>/file.txt` and returns its path.
fn make_file(base: &Path, rel_dir: &str) -> PathBuf {
    let d = base.join(rel_dir);
    std::fs::create_dir_all(&d).unwrap();
    let f = d.join("file.txt");
    std::fs::write(&f, "x\n").unwrap();
    f
}

/// Writes `<tmp>/proj/<SID>.jsonl` recording a Write tool_use per path.
fn transcript(tmp: &Path, writes: &[&Path]) -> String {
    let proj = tmp.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    let t = proj.join(format!("{SID}.jsonl"));
    let mut body = String::from(r#"{"type":"user","message":{"role":"user","content":"go"}}"#);
    body.push('\n');
    for (i, p) in writes.iter().enumerate() {
        body.push_str(
            &serde_json::json!({
                "type": "assistant",
                "isSidechain": false,
                "message": {"role": "assistant", "content": [
                    {"type": "tool_use", "id": format!("t{i}"), "name": "Write",
                     "input": {"file_path": p.to_string_lossy(), "content": "x"}}
                ]}
            })
            .to_string(),
        );
        body.push('\n');
    }
    std::fs::write(&t, body).unwrap();
    t.to_string_lossy().into_owned()
}

#[test]
fn this_session_scratchpad_write_is_known_and_not_a_root() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = git_init(&tmp.path().join("repo"));
    let f = make_file(tmp.path(), &format!("claude-1000/slug/{SID}/scratchpad"));
    let t = transcript(tmp.path(), &[&f]);
    match resolve(&cwd, &t) {
        Determination::Known(r) => {
            assert_eq!(r.roots, vec![cwd]);
            assert!(r.wrote_any);
        }
        Determination::Undetermined(w) => {
            panic!("expected Known, got Undetermined: {}", w.as_str())
        }
    }
}

#[test]
fn nested_scratchpad_write_is_known() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = git_init(&tmp.path().join("repo"));
    let f = make_file(
        tmp.path(),
        &format!("claude-1000/slug/{SID}/scratchpad/sub/dir"),
    );
    let t = transcript(tmp.path(), &[&f]);
    match resolve(&cwd, &t) {
        Determination::Known(r) => assert_eq!(r.roots, vec![cwd]),
        Determination::Undetermined(w) => {
            panic!("expected Known, got Undetermined: {}", w.as_str())
        }
    }
}

#[test]
fn other_sessions_scratchpad_is_undetermined() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = git_init(&tmp.path().join("repo"));
    let f = make_file(
        tmp.path(),
        &format!("claude-1000/slug/{OTHER_SID}/scratchpad"),
    );
    let t = transcript(tmp.path(), &[&f]);
    assert!(
        matches!(resolve(&cwd, &t), Determination::Undetermined(_)),
        "a different session's scratchpad must stay Undetermined"
    );
}

#[test]
fn non_git_non_scratchpad_dir_is_undetermined_naming_path() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = git_init(&tmp.path().join("repo"));
    let f = make_file(tmp.path(), "elsewhere");
    let t = transcript(tmp.path(), &[&f]);
    match resolve(&cwd, &t) {
        Determination::Undetermined(w) => {
            assert!(
                w.as_str().contains(&*f.to_string_lossy()),
                "reason: {}",
                w.as_str()
            )
        }
        Determination::Known(r) => panic!("expected Undetermined, got Known: {r:?}"),
    }
}

#[test]
fn non_digit_uid_component_is_undetermined() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = git_init(&tmp.path().join("repo"));
    let f = make_file(tmp.path(), &format!("claude-abc/slug/{SID}/scratchpad"));
    let t = transcript(tmp.path(), &[&f]);
    assert!(
        matches!(resolve(&cwd, &t), Determination::Undetermined(_)),
        "claude-<non-digit> is not Claude Code's scratchpad layout"
    );
}

#[test]
fn mixed_scratchpad_and_second_repo_includes_repo_toplevel() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = git_init(&tmp.path().join("repo"));
    let other = git_init(&tmp.path().join("other"));
    let sp = make_file(tmp.path(), &format!("claude-1000/slug/{SID}/scratchpad"));
    let inrepo = make_file(&other, "src");
    let t = transcript(tmp.path(), &[&sp, &inrepo]);
    match resolve(&cwd, &t) {
        Determination::Known(r) => {
            assert_eq!(r.roots, vec![cwd, other]);
            assert!(r.wrote_any);
        }
        Determination::Undetermined(w) => {
            panic!("expected Known, got Undetermined: {}", w.as_str())
        }
    }
}
