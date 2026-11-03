// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Bash word resolution against a GONE registered target.
//!
//! Written by an independent test author (CLAUDE.md §2(a)); the implementer
//! must match the contract, not edit these tests to pass.
//!
//! Contract under test (coordinator-decided):
//!
//! * A Bash call is denied as gone when a resolved path word lies under the
//!   recorded root of a gone target (root directory deleted), even when the
//!   payload cwd is outside it and the command text does not spell the root.
//! * Fail-closed: for a gone target, a Bash path word whose canonicalization
//!   fails with an error other than NotFound (e.g. ENOTDIR for
//!   `notes.txt/x` where `notes.txt` is a regular file) means the gate cannot
//!   show the call avoids the gone root: deny (exit 2, undetermined) with a
//!   reason naming the word.
//! * Unresolvable words (`$`, globs, ...) are outside this decision; `echo
//!   $HOME` is pinned to the current behaviour (allow).

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::{json, Value};

use common::{code, init_repo_at, pretooluse_payload, run_fde, run_gate, show, TempPlace};

// ------------------------------------------------------------------ helpers

fn decision(out: &Output) -> Option<String> {
    let v: Value = serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).ok()?;
    v.get("hookSpecificOutput")?
        .get("permissionDecision")?
        .as_str()
        .map(str::to_string)
}

/// The deny reason as printed (stdout JSON + stderr).
fn reason(out: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn assert_allowed(out: &Output, what: &str) {
    assert_eq!(
        decision(out).as_deref(),
        Some("allow"),
        "{what}: must be allowed: {}",
        show(out)
    );
    assert_eq!(code(out), Some(0), "{what}: allow exits 0: {}", show(out));
}

fn assert_denied(out: &Output, what: &str) {
    assert_eq!(
        decision(out).as_deref(),
        Some("deny"),
        "{what}: must be denied: {}",
        show(out)
    );
    assert_eq!(code(out), Some(2), "{what}: deny exits 2: {}", show(out));
}

fn assert_denied_as_gone(out: &Output, what: &str) {
    assert_denied(out, what);
    assert!(
        reason(out).contains("gone"),
        "{what}: the reason must say the target is gone: {}",
        show(out)
    );
}

// ------------------------------------------------------------------ fixture

/// `P/` holding the target `P/proj` (registered with `fde begin`, then
/// deleted when `gone`) and a sibling dir `P/sib`; an unrelated dir holding
/// a regular file `notes.txt`. State and spec dirs are separate temp dirs;
/// the gate's process cwd is the spec dir.
struct Layout {
    parent: TempPlace,
    root: PathBuf,
    sib: PathBuf,
    unrelated: TempPlace,
    state: TempPlace,
    spec_dir: TempPlace,
}

impl Layout {
    /// `register`: `fde begin` the target, then delete its root.
    /// Otherwise nothing is registered (and the root is kept).
    fn new(register: bool) -> Self {
        let parent = TempPlace::new("fde-gw-parent-");
        let root = init_repo_at(&parent.path.join("proj"));
        let sib = parent.path.join("sib");
        fs::create_dir_all(&sib).expect("create sib");
        let unrelated = TempPlace::new("fde-gw-unrelated-");
        fs::write(unrelated.path.join("notes.txt"), "n\n").expect("write notes.txt");
        let state = TempPlace::new("fde-gw-state-");
        let spec_dir = TempPlace::new("fde-gw-spec-");
        let spec = common::write_spec(&spec_dir.path, "spec.toml", "gw", &[("R1", "one")]);
        if register {
            let out = run_fde(
                &root,
                &state.path,
                &["begin", "--spec", spec.to_str().expect("utf-8 spec")],
            );
            assert_eq!(code(&out), Some(0), "fixture: fde begin: {}", show(&out));
            fs::remove_dir_all(&root).expect("delete the target root");
            assert!(
                fs::symlink_metadata(&root).is_err(),
                "fixture: the target root is gone"
            );
            // Control: the gate sees the target as gone (status says so).
            let st = run_fde(
                &parent.path,
                &state.path,
                &["status", root.to_str().unwrap()],
            );
            assert!(
                String::from_utf8_lossy(&st.stderr).contains("gone"),
                "fixture: status must report the target gone: {}",
                show(&st)
            );
        }
        Layout {
            parent,
            root,
            sib,
            unrelated,
            state,
            spec_dir,
        }
    }

    fn p(&self) -> &Path {
        &self.parent.path
    }

    /// Run the gate on Bash `cmd` with payload cwd `cwd`. Pre-conditions so
    /// only word resolution can tie the call to the gone root: the cwd is
    /// outside the root and the command does not spell the root path.
    fn gate(&self, cwd: &Path, cmd: &str) -> Output {
        assert!(
            !cwd.starts_with(&self.root),
            "fixture: cwd {} must be outside the root",
            cwd.display()
        );
        assert!(
            !cmd.contains(self.root.to_str().unwrap()),
            "fixture: {cmd:?} must not spell the root path"
        );
        let payload = pretooluse_payload(cwd, "Bash", json!({"command": cmd}));
        run_gate(&self.spec_dir.path, &self.state.path, payload.as_bytes())
    }
}

// ----------------------------------- (1)/(2) undetermined canonicalization

#[test]
fn gone_target_bash_word_with_enotdir_canonicalization_is_denied_naming_the_word() {
    let l = Layout::new(true);
    // Control: the word's canonicalization fails with a non-NotFound error.
    let err = l
        .unrelated
        .path
        .join("notes.txt/x")
        .canonicalize()
        .expect_err("fixture: notes.txt/x cannot canonicalize");
    assert_ne!(
        err.kind(),
        std::io::ErrorKind::NotFound,
        "fixture: the error must not be NotFound ({err})"
    );
    let out = l.gate(&l.unrelated.path, "cat notes.txt/x");
    assert_denied(&out, "Bash `cat notes.txt/x` with a gone target registered");
    assert!(
        reason(&out).contains("notes.txt/x"),
        "the reason must name the word `notes.txt/x`: {}",
        show(&out)
    );
}

#[test]
fn gone_target_bash_word_on_a_regular_file_is_allowed() {
    let l = Layout::new(true);
    let out = l.gate(&l.unrelated.path, "cat notes.txt");
    assert_allowed(&out, "Bash `cat notes.txt` with a gone target registered");
}

#[test]
fn no_target_bash_word_with_enotdir_canonicalization_is_allowed() {
    let l = Layout::new(false);
    let out = l.gate(&l.unrelated.path, "cat notes.txt/x");
    assert_allowed(&out, "Bash `cat notes.txt/x` with nothing registered");
}

// ------------------------------------------- (3) gone-root word resolution

#[test]
fn gone_root_relative_mkdir_from_parent_is_denied_as_gone() {
    let l = Layout::new(true);
    let out = l.gate(l.p(), "mkdir -p proj/x");
    assert_denied_as_gone(&out, "Bash `mkdir -p proj/x` from P");
}

#[test]
fn gone_root_dotdot_touch_from_sibling_is_denied_as_gone() {
    let l = Layout::new(true);
    let out = l.gate(&l.sib, "touch ../proj/x");
    assert_denied_as_gone(&out, "Bash `touch ../proj/x` from P/sib");
}

#[test]
fn gone_root_sib_dotdot_touch_from_parent_is_denied_as_gone() {
    let l = Layout::new(true);
    let out = l.gate(l.p(), "touch sib/../proj/x");
    assert_denied_as_gone(&out, "Bash `touch sib/../proj/x` from P");
}

#[test]
fn gone_root_ls_sibling_from_parent_is_allowed() {
    let l = Layout::new(true);
    let out = l.gate(l.p(), "ls sib");
    assert_allowed(&out, "Bash `ls sib` from P with the target gone");
}

#[test]
fn gone_root_echo_home_from_parent_is_allowed() {
    let l = Layout::new(true);
    let out = l.gate(l.p(), "echo $HOME");
    assert_allowed(&out, "Bash `echo $HOME` from P with only a gone target");
}
