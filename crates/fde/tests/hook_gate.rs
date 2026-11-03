// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `fde gate` (the PreToolUse hook) contract tests.
//!
//! Written by an independent test author (CLAUDE.md §2(a)); the implementer
//! must match the contract, not edit these tests to pass.
//!
//! Every test feeds a real-shaped PreToolUse JSON payload (`cwd`,
//! `tool_name`, `tool_input`, ...) on stdin to the built binary, with
//! `FDE_STATE_DIR` pointing at a temp state dir outside the target. Targets are
//! registered through `fde begin` and ratified through `fde ratify`.
//!
//! The process cwd of `fde gate` is deliberately a neutral directory (the
//! spec dir: outside the target, not a git repo) so that the gate must judge
//! from the payload's `cwd` and paths, not from where it happens to run.
//!
//! Decision contract (shared additions): stdout carries
//! `{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":
//! "allow|deny|ask",...}}`; allow exits 0, deny/ask exits 2. Every gate call
//! here also checks that exit 0 never comes with a non-allow decision.
//!
//! Anti-vacuity: an always-deny gate fails every `*_allowed` test, an
//! always-allow gate fails every `*_denied` test; each deny case has an allow
//! partner (same path after ratify, or a path outside every target).

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::{json, Value};

use common::{code, pretooluse_payload, run_gate, show, Engagement, TempPlace, TempRepo};

// ------------------------------------------------------------------ helpers

/// Run the gate on `payload` with process cwd `proc_cwd` and the state dir of `e`.
fn gate_from(e: &Engagement, proc_cwd: &Path, payload: &str) -> Output {
    let out = run_gate(proc_cwd, &e.state.path, payload.as_bytes());
    check_exit_matches_decision(&out);
    out
}

/// Run the gate with the neutral process cwd (the spec dir).
fn gate(e: &Engagement, payload: &str) -> Output {
    gate_from(e, &e.spec_dir.path, payload)
}

/// `hookSpecificOutput.permissionDecision` of the gate's stdout, if any.
fn decision(out: &Output) -> Option<String> {
    let text = String::from_utf8_lossy(&out.stdout);
    let v: Value = serde_json::from_str(text.trim()).ok()?;
    let hso = v.get("hookSpecificOutput")?;
    if hso.get("hookEventName").and_then(Value::as_str) != Some("PreToolUse") {
        return None;
    }
    hso.get("permissionDecision")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Exit 0 must never come with a non-allow decision.
fn check_exit_matches_decision(out: &Output) {
    if code(out) == Some(0) {
        assert_eq!(
            decision(out).as_deref(),
            Some("allow"),
            "exit 0 is an allow; it must carry permissionDecision \"allow\": {}",
            show(out)
        );
    }
}

fn assert_allowed(out: &Output, what: &str) {
    assert_eq!(
        decision(out).as_deref(),
        Some("allow"),
        "{what}: must be allowed (permissionDecision \"allow\"): {}",
        show(out)
    );
    assert_eq!(code(out), Some(0), "{what}: allow exits 0: {}", show(out));
}

fn assert_denied(out: &Output, what: &str) {
    assert_eq!(
        decision(out).as_deref(),
        Some("deny"),
        "{what}: must be denied (permissionDecision \"deny\"): {}",
        show(out)
    );
    assert_eq!(code(out), Some(2), "{what}: deny exits 2: {}", show(out));
}

/// For Bash: deny or ask, never allow.
fn assert_denied_or_asked(out: &Output, what: &str) {
    let d = decision(out);
    assert!(
        matches!(d.as_deref(), Some("deny") | Some("ask")),
        "{what}: must be denied or asked, never allowed (got {d:?}): {}",
        show(out)
    );
    assert_eq!(
        code(out),
        Some(2),
        "{what}: deny/ask exits 2: {}",
        show(out)
    );
}

fn s(p: &Path) -> String {
    p.to_str().expect("utf-8 path").to_string()
}

fn edit(cwd: &Path, file: &str) -> String {
    pretooluse_payload(
        cwd,
        "Edit",
        json!({"file_path": file, "old_string": "fixture", "new_string": "changed"}),
    )
}

fn write(cwd: &Path, file: &str) -> String {
    pretooluse_payload(
        cwd,
        "Write",
        json!({"file_path": file, "content": "new content\n"}),
    )
}

fn multi_edit(cwd: &Path, file: &str) -> String {
    pretooluse_payload(
        cwd,
        "MultiEdit",
        json!({"file_path": file, "edits": [{"old_string": "fixture", "new_string": "changed"}]}),
    )
}

fn notebook_edit(cwd: &Path, file: &str) -> String {
    pretooluse_payload(
        cwd,
        "NotebookEdit",
        json!({"notebook_path": file, "new_source": "print(1)", "edit_mode": "replace"}),
    )
}

fn bash(cwd: &Path, command: &str) -> String {
    pretooluse_payload(
        cwd,
        "Bash",
        json!({"command": command, "description": "fixture command"}),
    )
}

const REQS: &[(&str, &str)] = &[("R1", "the fixture does one thing")];

/// Registered (via `fde begin`) but unratified target; asserts status = 1.
fn unratified() -> Engagement {
    let e = Engagement::new(REQS);
    e.begin();
    let st = e.fde(&["status"]);
    assert_eq!(
        code(&st),
        Some(1),
        "fixture: a begun target is unratified: {}",
        show(&st)
    );
    e
}

/// Every file-editing payload for `file` (absolute), from cwd `cwd`.
fn edit_payloads(cwd: &Path, file: &str) -> Vec<(&'static str, String)> {
    vec![
        ("Edit", edit(cwd, file)),
        ("Write", write(cwd, file)),
        ("MultiEdit", multi_edit(cwd, file)),
    ]
}

// ------------------------------------------------- (1) / (2) ratification

/// (1) Edit / Write / MultiEdit to an existing file inside a registered,
/// unratified target is denied — from inside and from outside the target.
/// Fails an always-allow gate.
#[test]
fn edit_inside_unratified_target_is_denied() {
    let e = unratified();
    let file = s(&e.root().join("README.md"));
    for cwd in [e.root().to_path_buf(), e.spec_dir.path.clone()] {
        for (tool, p) in edit_payloads(&cwd, &file) {
            assert_denied(
                &gate(&e, &p),
                &format!(
                    "{tool} {file} (cwd {}) on an unratified target",
                    cwd.display()
                ),
            );
        }
    }
}

/// (1) A Write that creates a not-yet-existing file in a not-yet-existing
/// subdir of the target is still inside the target (it cannot be
/// canonicalized, but its nearest existing ancestor is the target).
#[test]
fn write_of_new_file_in_new_subdir_inside_unratified_target_is_denied() {
    let e = unratified();
    let file = s(&e.root().join("src/new/module.rs"));
    assert_denied(
        &gate(&e, &write(e.root(), &file)),
        "Write of a new nested file inside an unratified target",
    );
}

/// (1) NotebookEdit carries its path in `notebook_path`, not `file_path`.
#[test]
fn notebook_edit_inside_unratified_target_is_denied() {
    let e = unratified();
    let nb = e.root().join("analysis.ipynb");
    fs::write(&nb, "{\"cells\":[]}").expect("write notebook");
    assert_denied(
        &gate(&e, &notebook_edit(e.root(), &s(&nb))),
        "NotebookEdit inside an unratified target",
    );
}

/// (2) The same edits are allowed once the registered spec is ratified.
/// Partner of the (1) tests; fails an always-deny gate.
#[test]
fn edit_inside_ratified_target_is_allowed() {
    let e = Engagement::ratified(REQS);
    let file = s(&e.root().join("README.md"));
    for cwd in [e.root().to_path_buf(), e.spec_dir.path.clone()] {
        for (tool, p) in edit_payloads(&cwd, &file) {
            assert_allowed(
                &gate(&e, &p),
                &format!("{tool} {file} (cwd {}) on a ratified target", cwd.display()),
            );
        }
    }
    let new_file = s(&e.root().join("src/new/module.rs"));
    assert_allowed(
        &gate(&e, &write(e.root(), &new_file)),
        "Write of a new nested file inside a ratified target",
    );
    let nb = e.root().join("analysis.ipynb");
    fs::write(&nb, "{\"cells\":[]}").expect("write notebook");
    assert_allowed(
        &gate(&e, &notebook_edit(e.root(), &s(&nb))),
        "NotebookEdit inside a ratified target",
    );
}

/// (1)+(2) on ONE engagement: denied before `fde ratify`, allowed after.
/// Neither constant gate passes this.
#[test]
fn ratify_flips_the_same_edit_from_denied_to_allowed() {
    let e = unratified();
    let file = s(&e.root().join("README.md"));
    let p = edit(e.root(), &file);
    assert_denied(&gate(&e, &p), "before ratify");
    e.ratify();
    assert_allowed(&gate(&e, &p), "after ratify");
}

// ------------------------------------------------------ (10) spec drift

/// (10) Editing the spec after ratification makes edits denied again (and
/// `fde status` answers unratified). Allowed before the spec edit, so neither
/// constant gate passes.
#[test]
fn spec_edited_after_ratification_denies_edits_again() {
    let e = Engagement::ratified(REQS);
    let file = s(&e.root().join("README.md"));
    let p = edit(e.root(), &file);
    assert_allowed(&gate(&e, &p), "ratified, spec unchanged");

    let mut spec = fs::read_to_string(&e.spec).expect("read spec");
    spec.push_str("\n[[requirement]]\nid = \"R2\"\nstatement = \"added after ratification\"\n");
    fs::write(&e.spec, spec).expect("edit spec");

    let st = e.fde(&["status"]);
    assert_eq!(
        code(&st),
        Some(1),
        "a spec changed since ratification is unratified: {}",
        show(&st)
    );
    assert_denied(&gate(&e, &p), "edit after the ratified spec changed");
}

// -------------------------------------------- (3) unrelated work is allowed

/// (3) With a registered unratified target, edits to files outside every
/// registered target (another git repo, a plain dir) are allowed. Fails an
/// always-deny gate.
#[test]
fn edit_outside_every_registered_target_is_allowed() {
    let e = unratified();
    let other_repo = TempRepo::new();
    let plain = TempPlace::new("fde-plain-");
    let in_other = s(&other_repo.root.join("README.md"));
    let in_plain = s(&plain.path.join("notes.txt"));
    for (cwd, file) in [
        (other_repo.root.clone(), in_other.clone()),
        (plain.path.clone(), in_plain.clone()),
        // cwd inside the target, file elsewhere: the file decides, not the cwd
        (e.root().to_path_buf(), in_other),
    ] {
        for (tool, p) in edit_payloads(&cwd, &file) {
            assert_allowed(
                &gate(&e, &p),
                &format!("{tool} {file} (cwd {}) outside every target", cwd.display()),
            );
        }
    }
}

/// (3) No target registered at all (empty state dir): fde must not block
/// unrelated work, even inside a git repo.
#[test]
fn edit_with_no_registered_target_is_allowed() {
    let e = Engagement::new(REQS); // never begun
    let file = s(&e.root().join("README.md"));
    for (tool, p) in edit_payloads(e.root(), &file) {
        assert_allowed(&gate(&e, &p), &format!("{tool} with nothing registered"));
    }
    assert_allowed(
        &gate(&e, &bash(e.root(), "ls -la")),
        "Bash with nothing registered",
    );
}

// --------------------------------------------------- (4) git worktrees

/// (4) A git worktree of the target, created OUTSIDE the target dir, counts
/// as inside: denied while unratified, allowed after ratify.
#[test]
fn edit_inside_git_worktree_of_target_is_inside() {
    let e = unratified();
    let place = TempPlace::new("fde-wt-");
    let wt = place.path.join("wt");
    common::git(
        e.root(),
        &["worktree", "add", "-q", "-b", "fde-wt", s(&wt).as_str()],
    );
    let wt = wt.canonicalize().expect("canonicalize worktree");
    assert!(
        !wt.starts_with(e.root()),
        "fixture: the worktree must lie outside the target dir"
    );
    let file = s(&wt.join("README.md"));
    for (tool, p) in edit_payloads(&wt, &file) {
        assert_denied(
            &gate(&e, &p),
            &format!("{tool} in a worktree of an unratified target"),
        );
    }
    assert_denied_or_asked(
        &gate(&e, &bash(&wt, "touch new-file")),
        "Bash with cwd in a worktree of an unratified target",
    );
    e.ratify();
    assert_allowed(
        &gate(&e, &edit(&wt, &file)),
        "Edit in a worktree of a ratified target",
    );
}

// ------------------------------------------- (5) unreadable / corrupt state

/// Every regular file under `dir`, recursively.
fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut v = Vec::new();
    for ent in fs::read_dir(dir).expect("read state dir") {
        let ent = ent.expect("dir entry");
        let ft = ent.file_type().expect("file type");
        if ft.is_dir() {
            v.extend(files_under(&ent.path()));
        } else if ft.is_file() {
            v.push(ent.path());
        }
    }
    v
}

/// (5) A corrupt state/registry while a target is registered is denied, never
/// allowed (a parse failure must not read as "nothing registered").
/// Partner: `edit_inside_ratified_target_is_allowed`.
#[test]
fn corrupt_state_with_a_registered_target_is_denied() {
    let e = unratified();
    let files = files_under(&e.state.path);
    assert!(
        !files.is_empty(),
        "fixture: `fde begin` must have written something under FDE_STATE_DIR"
    );
    for f in &files {
        fs::write(f, b"\x00\xff not json, not toml {{{ [[[ \n").expect("corrupt state file");
    }
    let file = s(&e.root().join("README.md"));
    for (tool, p) in edit_payloads(e.root(), &file) {
        assert_denied(
            &gate(&e, &p),
            &format!("{tool} inside the target with a corrupt registry"),
        );
    }
}

/// (5) Same for a ratified target whose state became corrupt: the earlier
/// ratification cannot be read, so it does not count.
#[test]
fn corrupt_state_of_a_ratified_target_is_denied() {
    let e = Engagement::ratified(REQS);
    let file = s(&e.root().join("README.md"));
    let p = edit(e.root(), &file);
    assert_allowed(&gate(&e, &p), "before corruption");
    for f in files_under(&e.state.path) {
        fs::write(&f, b"\x00\xff garbage").expect("corrupt state file");
    }
    assert_denied(&gate(&e, &p), "after the ratified state was corrupted");
}

/// (5) An unreadable state dir while a target is registered is denied.
#[cfg(unix)]
#[test]
fn unreadable_state_with_a_registered_target_is_denied() {
    use std::os::unix::fs::PermissionsExt;
    let e = unratified();
    let state = e.state.path.clone();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o000)).expect("chmod 000");
    // Restore permissions even when an assertion below panics, so the temp
    // dir can be removed.
    struct Restore(PathBuf);
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
        }
    }
    let _restore = Restore(state.clone());
    assert!(
        fs::read_dir(&state).is_err(),
        "this environment ignores directory permissions (root?): the unreadable \
         case cannot be observed here — this is a test environment limit, not a pass"
    );
    let file = s(&e.root().join("README.md"));
    for (tool, p) in edit_payloads(e.root(), &file) {
        assert_denied(
            &gate(&e, &p),
            &format!("{tool} inside the target with an unreadable state dir"),
        );
    }
}

// --------------------------------------------------------------- (6) Bash

/// (6) Bash with cwd inside an unratified target is denied or asked.
#[test]
fn bash_with_cwd_inside_unratified_target_is_not_allowed() {
    let e = unratified();
    let sub = e.repo.subdir("src");
    for cwd in [e.root().to_path_buf(), sub] {
        assert_denied_or_asked(
            &gate(&e, &bash(&cwd, "echo changed > README.md")),
            &format!("Bash in {}", cwd.display()),
        );
    }
}

/// (6) Bash from elsewhere whose command string names the target path is
/// denied or asked.
#[test]
fn bash_mentioning_unratified_target_from_elsewhere_is_not_allowed() {
    let e = unratified();
    let elsewhere = TempPlace::new("fde-elsewhere-");
    let cmd = format!("echo changed > {}/README.md", s(e.root()));
    assert_denied_or_asked(
        &gate(&e, &bash(&elsewhere.path, &cmd)),
        "Bash naming the target path from outside",
    );
}

/// (6)/(3) partners: Bash unrelated to every target is allowed, and Bash in
/// a ratified target is allowed. Fails an always-deny (or always-ask) gate.
#[test]
fn bash_unrelated_or_in_ratified_target_is_allowed() {
    let e = unratified();
    let elsewhere = TempPlace::new("fde-elsewhere-");
    assert_allowed(
        &gate(&e, &bash(&elsewhere.path, "echo hi > notes.txt")),
        "Bash outside every target, not naming one",
    );

    let r = Engagement::ratified(REQS);
    assert_allowed(
        &gate(&r, &bash(r.root(), "echo changed > README.md")),
        "Bash inside a ratified target",
    );
}

// ----------------------------------------------- (7) `..` and symlinks

/// (7) A path with `..` components that lands inside the target is inside,
/// including one that lexically starts by leaving the target.
#[test]
fn dotdot_path_landing_inside_target_is_inside() {
    let e = unratified();
    e.repo.subdir("src");
    let name = e
        .root()
        .file_name()
        .expect("target basename")
        .to_str()
        .expect("utf-8 basename");
    let via_sub = format!("{}/src/../README.md", s(e.root()));
    let via_parent = format!("{}/../{name}/README.md", s(e.root()));
    let outside = TempPlace::new("fde-dotdot-");
    let o_name = outside
        .path
        .file_name()
        .expect("outside basename")
        .to_str()
        .expect("utf-8 basename")
        .to_string();
    // starts inside the target, `..` leaves it: outside
    let leaving = format!("{}/../{o_name}/x.txt", s(e.root()));
    let leaves_target = Path::new(&leaving)
        .parent()
        .and_then(|p| p.canonicalize().ok())
        .map(|p| p == outside.path)
        .unwrap_or(false);
    assert!(
        leaves_target,
        "fixture: target and outside place must be siblings for {leaving}"
    );

    for f in [&via_sub, &via_parent] {
        assert_denied(&gate(&e, &edit(e.root(), f)), &format!("Edit {f}"));
    }
    assert_allowed(
        &gate(&e, &write(e.root(), &leaving)),
        &format!("Write {leaving} (`..` leaves the target)"),
    );
}

/// (7) A symlink (outside the target) to the target dir, or to a file inside
/// it, lands inside: denied. A symlink to an outside dir: allowed.
#[cfg(unix)]
#[test]
fn symlink_landing_inside_target_is_inside() {
    use std::os::unix::fs::symlink;
    let e = unratified();
    let links = TempPlace::new("fde-links-");
    let other = TempPlace::new("fde-other-");
    symlink(e.root(), links.path.join("to-target")).expect("dir symlink");
    symlink(e.root().join("README.md"), links.path.join("readme-link")).expect("file symlink");
    symlink(&other.path, links.path.join("to-other")).expect("outside symlink");

    let via_dir = s(&links.path.join("to-target/README.md"));
    let via_dir_new = s(&links.path.join("to-target/brand-new.txt"));
    let via_file = s(&links.path.join("readme-link"));
    for f in [&via_dir, &via_file] {
        assert_denied(&gate(&e, &edit(&links.path, f)), &format!("Edit {f}"));
    }
    assert_denied(
        &gate(&e, &write(&links.path, &via_dir_new)),
        "Write of a new file through a dir symlink into the target",
    );
    assert_denied_or_asked(
        &gate(&e, &bash(&links.path.join("to-target"), "touch x")),
        "Bash with cwd = symlink into the target",
    );
    let via_other = s(&links.path.join("to-other/x.txt"));
    assert_allowed(
        &gate(&e, &write(&links.path, &via_other)),
        "Write through a symlink to an outside dir",
    );
}

// --------------------------------------------- (9) relative file_path

/// (9) A relative `file_path` resolves against the payload `cwd`, not the
/// gate's process cwd. Both directions are checked, so neither constant gate
/// nor a gate that resolves against its own process cwd passes.
#[test]
fn relative_file_path_resolves_against_payload_cwd() {
    let e = unratified();
    let elsewhere = TempPlace::new("fde-elsewhere-");

    // payload cwd = target, process cwd = elsewhere -> inside -> denied
    let out = gate_from(&e, &elsewhere.path, &edit(e.root(), "README.md"));
    assert_denied(&out, "relative README.md with payload cwd = target");
    let out = gate_from(&e, &elsewhere.path, &write(e.root(), "./src/new.rs"));
    assert_denied(&out, "relative ./src/new.rs with payload cwd = target");

    // payload cwd = elsewhere, process cwd = target -> outside -> allowed
    let out = gate_from(&e, e.root(), &write(&elsewhere.path, "README.md"));
    assert_allowed(&out, "relative README.md with payload cwd = elsewhere");
}

// ------------------------------------------------- (8) malformed payloads

/// (8) Empty stdin, non-JSON, truncated JSON, and a JSON payload with no
/// `tool_name` are denied — even with nothing registered (so an always-allow
/// gate, or one that short-circuits "nothing registered" before parsing,
/// fails). Partner: `edit_with_no_registered_target_is_allowed`.
#[test]
fn malformed_payload_is_denied() {
    let e = Engagement::new(REQS); // nothing registered
    let truncated = {
        let full = edit(e.root(), &s(&e.root().join("README.md")));
        full[..full.len() / 2].to_string()
    };
    for (what, stdin) in [
        ("empty stdin", String::new()),
        ("non-JSON", "this is not json".to_string()),
        ("truncated JSON", truncated),
        ("JSON without tool_name", "{\"cwd\":\"/\"}".to_string()),
    ] {
        let out = gate(&e, &stdin);
        assert_denied(&out, what);
    }
}
