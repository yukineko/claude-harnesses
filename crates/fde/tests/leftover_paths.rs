// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Behaviours the implementers added without an independent test, plus one
//! coordinator-decided change (L5).
//!
//! Written by an independent test author (CLAUDE.md §2(a)); the implementer
//! must match the contract, not edit these tests to pass.
//!
//! * L1 — a current-fingerprint ledger record with `head` / `dirty` but no
//!   `ratified_head` makes `fde evidence red|green|pass` undetermined (exit 2)
//!   and nothing is recorded.
//! * L2 — a RATIFIED registered target whose root cannot be stat'ed (error
//!   other than `NotFound`) makes the gate deny even an unrelated Edit.
//! * L3 — a RATIFIED registered target whose root is gone: an Edit under the
//!   former root is denied as gone; an unrelated Edit is allowed.
//! * L4 — a gone (unratified) target: Bash `fde status <root>` is allowed by
//!   the status/report/begin exception; `fde forget <root>` is human-only.
//! * L5 (decided change) — `fde forget <root>` of a registered target whose
//!   root exists but is no longer a git repository removes its state (key:
//!   the canonical path, since git resolution fails); the same for an
//!   unregistered non-git directory is refused (exit 1).
//! * L6 — `fde ratify` / `fde evidence red` when HEAD is readable but the
//!   dirty probe (`git status`) fails: exit 2, nothing written.
//! * L7 — a RED whose CMD moves HEAD backwards off the ratified line.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::{json, Value};

use common::{
    code, git, init_repo_at, parse_resolved, pretooluse_payload, run_fde, run_gate, show,
    snapshot_files, stderr, stdout, write_spec, Engagement, TempPlace,
};

// ------------------------------------------------------------------ helpers

fn assert_code(out: &Output, want: i32, what: &str) {
    assert_eq!(code(out), Some(want), "{what}: {}", show(out));
}

fn assert_undetermined(out: &Output, what: &str) {
    assert_code(out, 2, what);
    assert!(
        !stderr(out).trim().is_empty(),
        "{what}: exit 2 must state a reason on stderr: {}",
        show(out)
    );
}

fn s(p: &Path) -> String {
    p.to_str().expect("utf-8 path").to_string()
}

/// `hookSpecificOutput.permissionDecision` of the gate's stdout, if any.
fn decision(out: &Output) -> Option<String> {
    let v: Value = serde_json::from_str(stdout(out).trim()).ok()?;
    v.get("hookSpecificOutput")?
        .get("permissionDecision")
        .and_then(Value::as_str)
        .map(str::to_string)
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

fn reason(out: &Output) -> String {
    format!("{}\n{}", stdout(out), stderr(out))
}

fn edit_payload(cwd: &Path, file: &Path) -> String {
    pretooluse_payload(
        cwd,
        "Edit",
        json!({"file_path": s(file), "old_string": "a", "new_string": "b"}),
    )
}

fn bash_payload(cwd: &Path, command: &str) -> String {
    pretooluse_payload(cwd, "Bash", json!({"command": command}))
}

fn ev(e: &Engagement, kind: &str, req: &str, cmd: &str) -> Output {
    e.fde(&["evidence", kind, "--req", req, "--cmd", cmd])
}

fn key_dir(e: &Engagement) -> PathBuf {
    let (_root, key) = parse_resolved(&e.fde(&["resolve"]));
    e.state.path.join("targets").join(key)
}

/// Restores a directory's mode on drop (also on a panicking test).
struct ModeGuard(PathBuf, u32);
impl Drop for ModeGuard {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(self.1));
    }
}

/// Overwrite the target's git index with garbage: `git rev-parse --verify
/// HEAD` still exits 0, `git status --porcelain` exits non-zero. Both facts
/// are checked here so the fixture cannot silently stop meaning that.
fn corrupt_index(root: &Path) {
    fs::write(root.join(".git").join("index"), b"garbagegarbagegarbage").unwrap();
    let head = std::process::Command::new("git")
        .current_dir(root)
        .args(["rev-parse", "--verify", "HEAD"])
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .unwrap();
    assert!(
        head.status.success(),
        "fixture: HEAD must stay readable with a corrupt index: {head:?}"
    );
    let st = std::process::Command::new("git")
        .current_dir(root)
        .args(["--no-optional-locks", "status", "--porcelain"])
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .unwrap();
    assert!(
        !st.status.success(),
        "fixture: git status must fail with a corrupt index: {st:?}"
    );
}

// ------------------------------------------------------------------- L1

/// Record a normal RED for R1, then strip `ratified_head` from it (keeping
/// `head` / `dirty`): a record "written by an fde before epochs".
fn engagement_with_unplaced_record() -> (Engagement, PathBuf) {
    let e = Engagement::ratified(&[("R1", "first")]);
    assert_code(&ev(&e, "red", "R1", "false"), 0, "fixture: normal RED");
    let ledger = key_dir(&e).join("evidence.json");
    let mut v: Value = serde_json::from_slice(&fs::read(&ledger).unwrap()).unwrap();
    let rec = v["records"][0].as_object_mut().expect("record #0 object");
    assert!(rec.contains_key("head") && rec.contains_key("dirty"));
    assert!(
        rec.remove("ratified_head").is_some(),
        "fixture: the record had ratified_head"
    );
    fs::write(&ledger, serde_json::to_vec_pretty(&v).unwrap()).unwrap();
    (e, ledger)
}

fn assert_unplaced_record_blocks(kind: &str, cmd: &str) {
    let (e, ledger) = engagement_with_unplaced_record();
    let before = fs::read(&ledger).unwrap();
    let out = ev(&e, kind, "R1", cmd);
    assert_undetermined(&out, &format!("evidence {kind} with an unplaced record"));
    assert_eq!(
        fs::read(&ledger).unwrap(),
        before,
        "evidence {kind}: the ledger must be unchanged"
    );
}

#[test]
fn l1_red_with_a_record_without_ratified_head_is_undetermined() {
    assert_unplaced_record_blocks("red", "false");
}

#[test]
fn l1_green_with_a_record_without_ratified_head_is_undetermined() {
    assert_unplaced_record_blocks("green", "true");
}

#[test]
fn l1_pass_with_a_record_without_ratified_head_is_undetermined() {
    assert_unplaced_record_blocks("pass", "true");
}

// ------------------------------------------------------------------- L2

#[test]
fn l2_ratified_target_with_unstattable_root_denies_an_unrelated_edit() {
    use std::os::unix::fs::PermissionsExt;
    let parent = TempPlace::new("fde-locked-parent-");
    let root = init_repo_at(&parent.path.join("repo"));
    let state = TempPlace::new("fde-state-");
    let spec_dir = TempPlace::new("fde-spec-");
    let spec = write_spec(&spec_dir.path, "spec.toml", "fixture", &[("R1", "x")]);
    assert_code(
        &run_fde(&root, &state.path, &["begin", "--spec", &s(&spec)]),
        0,
        "register",
    );
    assert_code(&run_fde(&root, &state.path, &["ratify"]), 0, "ratify");
    assert_code(&run_fde(&root, &state.path, &["status"]), 0, "ratified");
    let elsewhere = TempPlace::new("fde-elsewhere-");
    let payload = edit_payload(&elsewhere.path, &elsewhere.path.join("x.txt"));
    // Control: a ratified target does not deny an unrelated edit.
    assert_allowed(
        &run_gate(&elsewhere.path, &state.path, payload.as_bytes()),
        "control: unrelated edit with a ratified target",
    );
    let _restore = ModeGuard(parent.path.clone(), 0o755);
    fs::set_permissions(&parent.path, fs::Permissions::from_mode(0o000)).unwrap();
    let err = fs::symlink_metadata(&root).expect_err("fixture: root unstattable");
    assert_ne!(
        err.kind(),
        std::io::ErrorKind::NotFound,
        "fixture: the stat error must not be NotFound ({err}); running as root?"
    );
    let out = run_gate(&elsewhere.path, &state.path, payload.as_bytes());
    assert_denied(
        &out,
        "unrelated edit while a RATIFIED target root cannot be stat'ed",
    );
}

// ------------------------------------------------------------------- L3

#[test]
fn l3_ratified_gone_target_denies_its_former_path_only() {
    let e = Engagement::ratified(&[("R1", "first")]);
    let root = e.root().to_path_buf();
    let elsewhere = TempPlace::new("fde-elsewhere-");
    fs::remove_dir_all(&root).unwrap();
    let inside = run_gate(
        &elsewhere.path,
        &e.state.path,
        edit_payload(&elsewhere.path, &root.join("x")).as_bytes(),
    );
    assert_denied(&inside, "Edit of <gone ratified root>/x");
    assert!(
        reason(&inside).contains("gone"),
        "the reason must say the target is gone: {}",
        show(&inside)
    );
    let unrelated = run_gate(
        &elsewhere.path,
        &e.state.path,
        edit_payload(&elsewhere.path, &elsewhere.path.join("x.txt")).as_bytes(),
    );
    assert_allowed(&unrelated, "unrelated Edit with a gone ratified target");
}

// ------------------------------------------------------------------- L4

#[test]
fn l4_gone_target_allows_fde_status_but_not_fde_forget() {
    let e = Engagement::new(&[("R1", "first")]);
    e.begin();
    let root = e.root().to_path_buf();
    fs::remove_dir_all(&root).unwrap();
    let elsewhere = TempPlace::new("fde-elsewhere-");
    let gate = |cmd: &str| {
        run_gate(
            &elsewhere.path,
            &e.state.path,
            bash_payload(&elsewhere.path, cmd).as_bytes(),
        )
    };
    // Control: the gone root's text in another command is denied as gone, so
    // the allow below comes from the status exception.
    let ls = gate(&format!("ls {}", s(&root)));
    assert_denied(&ls, "control: `ls <gone root>`");
    assert!(reason(&ls).contains("gone"), "{}", show(&ls));

    assert_allowed(
        &gate(&format!("fde status {}", s(&root))),
        "`fde status <gone root>` from an unrelated cwd",
    );
    // The gone-root deny reason also mentions `fde forget` / human-only, so
    // the reason is checked for the forget rule's own wording ("invokes"),
    // and a forget naming something else (no gone-root hit) is denied too.
    for cmd in [
        format!("fde forget {}", s(&root)),
        "fde forget x".to_string(),
    ] {
        let forget = gate(&cmd);
        assert_denied(&forget, &format!("{cmd:?} with a gone target registered"));
        let r = reason(&forget);
        assert!(
            r.contains("invokes `fde forget`") && r.contains("human-only"),
            "{cmd:?}: the deny must come from the human-only forget rule: {}",
            show(&forget)
        );
    }
}

// ------------------------------------------------------------- L5 (decided)

#[test]
fn l5_forget_removes_a_registered_target_that_is_no_longer_a_git_repo() {
    let e = Engagement::new(&[("R1", "first")]);
    e.begin();
    let dir = key_dir(&e);
    assert!(dir.is_dir(), "fixture: state dir exists");
    let root = e.root().to_path_buf();
    fs::remove_dir_all(root.join(".git")).unwrap();
    assert!(root.is_dir(), "fixture: the root still exists");
    let elsewhere = TempPlace::new("fde-elsewhere-");
    let before = snapshot_files(&root);
    let out = run_fde(&elsewhere.path, &e.state.path, &["forget", &s(&root)]);
    assert_code(
        &out,
        0,
        "forget a registered root that is no longer a git repo",
    );
    assert!(!dir.exists(), "its state dir must be removed");
    assert_eq!(
        snapshot_files(&root),
        before,
        "forget never touches the target"
    );
}

#[test]
fn l5_forget_of_an_unregistered_non_git_directory_is_refused() {
    let e = Engagement::new(&[("R1", "first")]);
    e.begin();
    let dir = key_dir(&e);
    let plain = TempPlace::new("fde-plain-");
    let out = run_fde(&plain.path, &e.state.path, &["forget", &s(&plain.path)]);
    assert_code(&out, 1, "forget an unregistered non-git directory");
    assert!(dir.is_dir(), "the registered target's state is untouched");
}

// ------------------------------------------------------------------- L6

#[test]
fn l6_ratify_with_unobservable_dirty_flag_is_undetermined() {
    let e = Engagement::new(&[("R1", "first")]);
    e.begin();
    let dir = key_dir(&e);
    corrupt_index(e.root());
    let before = snapshot_files(&e.state.path);
    let out = e.fde(&["ratify"]);
    assert_undetermined(&out, "ratify with a corrupt index");
    assert!(
        !dir.join("ratification.json").exists(),
        "no ratification may be written"
    );
    assert_eq!(snapshot_files(&e.state.path), before, "state unchanged");
}

#[test]
fn l6_evidence_red_with_unobservable_dirty_flag_is_undetermined() {
    let e = Engagement::ratified(&[("R1", "first")]);
    let dir = key_dir(&e);
    corrupt_index(e.root());
    let marker = e.spec_dir.path.join("ran");
    let cmd = format!("touch {} && false", s(&marker));
    let before = snapshot_files(&e.state.path);
    let out = ev(&e, "red", "R1", &cmd);
    assert_undetermined(&out, "evidence red with a corrupt index");
    assert!(!dir.join("evidence.json").exists(), "nothing recorded");
    assert_eq!(snapshot_files(&e.state.path), before, "state unchanged");
    assert!(!marker.exists(), "CMD must not run");
}

// ------------------------------------------------------------------- L7

/// A RED whose CMD resets HEAD one commit back (off the ratified line).
///
/// `evidence.rs` (module docs, "Recording" step 6) observes HEAD / dirty
/// immediately BEFORE CMD ("describes the tree CMD ran on, not what CMD left
/// behind"), and step 7 re-observes only the ratification (fingerprint and
/// the recorded provenance in `ratification.json`), not the current HEAD. So
/// the RED is recorded (exit 0) with `head` = the pre-CMD commit. The
/// ratified-history rule then makes `verify` and `report` undetermined
/// (exit 2), because the current HEAD is no longer a descendant of R.
#[test]
fn l7_red_whose_cmd_resets_head_back_is_recorded_pre_cmd_and_verify_is_undetermined() {
    let e = Engagement::new(&[("R1", "first")]);
    let c1 = git(e.root(), &["rev-parse", "HEAD"]).trim().to_string();
    fs::write(e.root().join("second.txt"), "2\n").unwrap();
    git(e.root(), &["add", "second.txt"]);
    git(e.root(), &["commit", "-q", "-m", "second"]);
    let c2 = git(e.root(), &["rev-parse", "HEAD"]).trim().to_string();
    e.begin();
    e.ratify();
    let ledger = key_dir(&e).join("evidence.json");
    let out = ev(&e, "red", "R1", "git reset -q --hard HEAD~1 && exit 1");
    let now = git(e.root(), &["rev-parse", "HEAD"]).trim().to_string();
    assert_eq!(now, c1, "fixture: CMD moved HEAD back to the first commit");
    assert_code(&out, 0, "RED whose CMD resets HEAD back");
    let v: Value = serde_json::from_slice(&fs::read(&ledger).unwrap()).unwrap();
    let recs = v["records"].as_array().unwrap();
    assert_eq!(recs.len(), 1, "one record: {v}");
    assert_eq!(recs[0]["head"], json!(c2), "head is the pre-CMD HEAD");
    assert_eq!(recs[0]["ratified_head"], json!(c2));
    assert_undetermined(
        &e.fde(&["evidence", "verify"]),
        "verify after HEAD moved off the ratified line",
    );
    assert_undetermined(
        &e.fde(&["report"]),
        "report after HEAD moved off the ratified line",
    );
}
