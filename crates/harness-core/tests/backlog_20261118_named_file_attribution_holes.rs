#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `has_unattributable_write`: naming ANY existing regular file must not make
//! a path-less write attributable. Independent verifier observed `Known(false)`
//! (scope narrowed) for commands that write files they never name
//! (2026-11-18). Each RED below asserts `Known(true)`; the controls pin the
//! cases that must stay `Known(false)` so a fix cannot return true for
//! everything.
//!
//! Written by a disinterested author (CLAUDE.md section 2(a)); only the public
//! contract of `has_unattributable_write` and the transcript format were read.

use harness_core::attribution::has_unattributable_write;
use harness_core::verdict::Determination;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static N: AtomicU32 = AtomicU32::new(0);

/// Run `cmd` as a single Bash tool_use in a tmp cwd holding a.rs, existing.rs,
/// gen.py, fmt.sh, x/b.rs and directory other_dir.
fn verdict(cmd: &str) -> Determination<bool> {
    let n = N.fetch_add(1, Ordering::SeqCst);
    let base = std::env::temp_dir().join(format!("attr-holes-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let cwd = base.join("repo");
    std::fs::create_dir_all(cwd.join("x")).unwrap();
    std::fs::create_dir_all(cwd.join("other_dir")).unwrap();
    for f in ["a.rs", "existing.rs", "gen.py", "fmt.sh", "x/b.rs"] {
        std::fs::write(cwd.join(f), "x\n").unwrap();
    }
    let cwd = cwd.canonicalize().unwrap();
    let tdir: PathBuf = base.join("transcripts");
    std::fs::create_dir_all(&tdir).unwrap();
    let transcript = tdir.join("sess.jsonl");
    let mut body = String::from(r#"{"type":"user","message":{"role":"user","content":"go"}}"#);
    body.push('\n');
    body.push_str(
        &serde_json::json!({
            "type": "assistant",
            "cwd": cwd.to_string_lossy(),
            "isSidechain": false,
            "message": {"role": "assistant", "content": [
                {"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": cmd}}
            ]}
        })
        .to_string(),
    );
    body.push('\n');
    std::fs::write(&transcript, body).unwrap();
    has_unattributable_write(transcript.to_str().unwrap())
}

fn expect(cmd: &str, want: bool) {
    match verdict(cmd) {
        Determination::Known(got) => assert_eq!(
            got, want,
            "`{cmd}`: expected Known({want}) (unattributable={want}), got Known({got})"
        ),
        other => panic!("`{cmd}`: expected Known({want}), got a non-Known result: {other:?}"),
    }
}

// ---- REDs: path-less writes hidden behind a named existing file ----

// test-weakening-justified: new RED for an unfixed defect (2026-11-18).
#[ignore = "RED: 2026-11-18 cargo fmt rewrites the whole workspace; `-- a.rs` does not scope it"]
#[test]
fn cargo_fmt_with_named_file_is_unattributable() {
    expect("cargo fmt -- a.rs", true);
}

// test-weakening-justified: new RED for an unfixed defect (2026-11-18).
#[ignore = "RED: 2026-11-18 python script outputs are unnamed (script counted as the named path)"]
#[test]
fn python_script_alone_is_unattributable() {
    expect("python3 gen.py", true);
}

// test-weakening-justified: new RED for an unfixed defect (2026-11-18).
#[ignore = "RED: 2026-11-18 python script plus an existing arg: outputs still unnamed"]
#[test]
fn python_script_with_existing_arg_is_unattributable() {
    expect("python3 gen.py existing.rs", true);
}

// test-weakening-justified: new RED for an unfixed defect (2026-11-18).
#[ignore = "RED: 2026-11-18 shell script writes are unnamed"]
#[test]
fn bash_script_is_unattributable() {
    expect("bash fmt.sh", true);
}

// test-weakening-justified: new RED for an unfixed defect (2026-11-18).
#[ignore = "RED: 2026-11-18 cargo build writes target/ etc; a.rs is not what it writes"]
#[test]
fn cargo_build_with_named_file_is_unattributable() {
    expect("cargo build a.rs", true);
}

// test-weakening-justified: new RED for an unfixed defect (2026-11-18).
#[ignore = "RED: 2026-11-18 rustfmt also formats mod-reached child files"]
#[test]
fn rustfmt_with_named_file_is_unattributable() {
    expect("rustfmt a.rs", true);
}

// test-weakening-justified: new RED for an unfixed defect (2026-11-18).
#[ignore = "RED: 2026-11-18 git apply targets live inside the patch, not the named patch file"]
#[test]
fn git_apply_patch_file_is_unattributable() {
    expect("git apply a.rs", true);
}

// test-weakening-justified: new RED for an unfixed defect (2026-11-18).
#[ignore = "RED: 2026-11-18 patch targets live inside the patch fed by stdin redirect"]
#[test]
fn patch_with_stdin_patch_file_is_unattributable() {
    expect("patch -p1 < a.rs", true);
}

// test-weakening-justified: new RED for an unfixed defect (2026-11-18).
#[ignore = "RED: 2026-11-18 cp into a directory writes other_dir/a.rs which is not named"]
#[test]
fn cp_into_directory_is_unattributable() {
    expect("cp a.rs other_dir/", true);
}

// test-weakening-justified: new RED for an unfixed defect (2026-11-18).
#[ignore = "RED: 2026-11-18 git stash pop rewrites arbitrary tracked files"]
#[test]
fn git_stash_pop_with_named_file_is_unattributable() {
    expect("git stash pop a.rs", true);
}

// ---- Controls: must pass now and after the fix ----

#[test]
fn control_sed_in_place_named_file_is_attributable() {
    expect("sed -i s/a/b/ a.rs", false);
}

#[test]
fn control_tee_named_file_is_attributable() {
    expect("tee a.rs", false);
}

#[test]
fn control_touch_named_file_is_attributable() {
    expect("touch a.rs", false);
}

#[test]
fn control_cp_to_named_existing_dest_is_attributable() {
    expect("cp a.rs existing.rs", false);
}

#[test]
fn control_cat_is_read_only() {
    expect("cat a.rs", false);
}

#[test]
fn control_git_diff_is_read_only() {
    expect("git diff", false);
}
