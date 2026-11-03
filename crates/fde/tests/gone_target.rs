// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Contract C3: a deleted ("gone") registered target, and `fde forget`.
//!
//! Written by an independent test author (CLAUDE.md §2(a)); the implementer
//! must match the contract, not edit these tests to pass.
//!
//! Contract under test (coordinator-decided):
//!
//! 1. A registered target whose root is observed not to exist
//!    (`symlink_metadata` fails with `NotFound`) is gone: determined. Any
//!    other error stat'ing the root is undetermined (deny, unchanged).
//! 2. Gate: a gone target is not guarded; it causes no deny for calls on
//!    unrelated paths. Other targets are judged as before. A corrupt
//!    registration still makes the gate deny everything.
//! 3. `fde status <gone root>` (also through a symlinked-parent spelling) ->
//!    exit 1, stderr contains `gone`. A nonexistent path that is not a
//!    registered root stays exit 2.
//! 4. `fde forget [TARGET]` removes `<state>/targets/<key>/` entirely. Key:
//!    the resolved git top-level when TARGET exists, else the
//!    canonical_lenient absolute path (relative against cwd). Exit 0 removed
//!    / 1 not registered (no state dir for the key) / 2 undetermined (cannot
//!    resolve, cannot remove). Works for a gone target and for a corrupt
//!    registration; never touches the target.
//! 5. `fde forget` is human-only: the gate denies any Bash call whose command
//!    invokes `fde forget` (textual: a word `fde` or ending in `/fde`, later a
//!    word `forget`; split on whitespace and `;&|()<>` and newlines, quotes
//!    stripped) whenever at least one target is registered (gone included).
//!    The reason says `fde forget` is human-only. With no registered target
//!    the gate allows everything.
//!
//! Before C3 exists, every `fde forget` call is a clap error (exit 2,
//! "unrecognized subcommand"): that is the expected RED for the forget tests.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::{json, Value};

use common::{
    code, init_repo_at, parse_resolved, pretooluse_payload, run_fde, run_gate, show,
    snapshot_files, stderr, stdout, Engagement, TempPlace, TempRepo,
};

// ------------------------------------------------------------------ helpers

fn assert_code(out: &Output, want: i32, what: &str) {
    assert_eq!(code(out), Some(want), "{what}: {}", show(out));
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

/// The deny reason as printed (stdout JSON + stderr), for content checks.
fn reason(out: &Output) -> String {
    format!("{}\n{}", stdout(out), stderr(out))
}

fn s(p: &Path) -> String {
    p.to_str().expect("utf-8 path").to_string()
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

/// Run the gate with a neutral process cwd.
fn gate(state: &Path, neutral: &Path, payload: &str) -> Output {
    run_gate(neutral, state, payload.as_bytes())
}

/// Two distinct unratified targets registered in ONE state dir:
/// `a` (the engagement's repo) and `b` (a second repo). `elsewhere` is a
/// plain dir outside both (also the neutral process cwd).
struct Two {
    a: Engagement,
    b: TempRepo,
    a_root: PathBuf,
    a_key: String,
    b_key: String,
    elsewhere: TempPlace,
}

impl Two {
    fn new() -> Self {
        let a = Engagement::new(&[("R1", "first")]);
        a.begin();
        let b = TempRepo::new();
        let out = run_fde(&b.root, &a.state.path, &["begin", "--spec", a.spec_str()]);
        assert_code(&out, 0, "register B");
        let (a_root, a_key) = parse_resolved(&a.fde(&["resolve"]));
        let (_b_root, b_key) = parse_resolved(&run_fde(&b.root, &a.state.path, &["resolve"]));
        assert_ne!(a_key, b_key, "fixture: distinct keys");
        let elsewhere = TempPlace::new("fde-elsewhere-");
        Two {
            a,
            b,
            a_root,
            a_key,
            b_key,
            elsewhere,
        }
    }

    fn state(&self) -> &Path {
        &self.a.state.path
    }

    fn key_dir(&self, key: &str) -> PathBuf {
        self.state().join("targets").join(key)
    }

    /// Delete A's root directory.
    fn delete_a(&self) {
        fs::remove_dir_all(&self.a_root).unwrap();
        assert!(
            fs::symlink_metadata(&self.a_root).is_err(),
            "fixture: A is gone"
        );
    }

    fn gate(&self, payload: &str) -> Output {
        gate(self.state(), &self.elsewhere.path, payload)
    }

    /// fde with cwd = the neutral dir.
    fn fde(&self, args: &[&str]) -> Output {
        run_fde(&self.elsewhere.path, self.state(), args)
    }
}

// ------------------------------------------------- (2) gate and gone targets

#[test]
fn gone_target_does_not_deny_an_unrelated_edit() {
    let t = Two::new();
    let unrelated = t.elsewhere.path.join("notes.txt");
    // Control: before deletion, an unrelated edit is allowed.
    assert_allowed(
        &t.gate(&edit_payload(&t.elsewhere.path, &unrelated)),
        "control: unrelated edit with both targets present",
    );
    t.delete_a();
    assert_allowed(
        &t.gate(&edit_payload(&t.elsewhere.path, &unrelated)),
        "unrelated edit after A is gone",
    );
}

#[test]
fn gone_target_does_not_deny_unrelated_bash() {
    let t = Two::new();
    t.delete_a();
    assert_allowed(
        &t.gate(&bash_payload(&t.elsewhere.path, "ls")),
        "Bash with cwd outside both after A is gone",
    );
}

#[test]
fn other_target_stays_guarded_when_one_is_gone() {
    let t = Two::new();
    t.delete_a();
    let inside_b = t.b.root.join("README.md");
    let out = t.gate(&edit_payload(&t.elsewhere.path, &inside_b));
    assert_denied(&out, "edit inside B after A is gone");
    let r = reason(&out);
    assert!(
        r.contains(&s(&t.b.root)) && r.contains("not ratified"),
        "the deny must be B's unratified violation, not an A artefact: {}",
        show(&out)
    );
}

#[test]
fn gone_only_target_does_not_deny_an_unrelated_edit() {
    let e = Engagement::new(&[("R1", "first")]);
    e.begin();
    let elsewhere = TempPlace::new("fde-elsewhere-");
    let root = e.root().to_path_buf();
    fs::remove_dir_all(&root).unwrap();
    let out = gate(
        &e.state.path,
        &elsewhere.path,
        &edit_payload(&elsewhere.path, &elsewhere.path.join("x.txt")),
    );
    assert_allowed(&out, "unrelated edit with the only target gone");
}

/// Restores a directory's mode on drop (also on a panicking test).
struct ModeGuard(PathBuf, u32);
impl Drop for ModeGuard {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(self.1));
    }
}

#[test]
fn unstattable_root_is_undetermined_and_denies() {
    use std::os::unix::fs::PermissionsExt;
    let parent = TempPlace::new("fde-locked-parent-");
    let root = init_repo_at(&parent.path.join("repo"));
    let state = TempPlace::new("fde-state-");
    let spec_dir = TempPlace::new("fde-spec-");
    let spec = common::write_spec(&spec_dir.path, "spec.toml", "fixture", &[("R1", "x")]);
    assert_code(
        &run_fde(&root, &state.path, &["begin", "--spec", &s(&spec)]),
        0,
        "register the target",
    );
    let elsewhere = TempPlace::new("fde-elsewhere-");
    let payload = edit_payload(&elsewhere.path, &elsewhere.path.join("x.txt"));
    let _restore = ModeGuard(parent.path.clone(), 0o755);
    fs::set_permissions(&parent.path, fs::Permissions::from_mode(0o000)).unwrap();
    // Control: the root's stat fails with something other than NotFound.
    let err = fs::symlink_metadata(&root).expect_err("fixture: root unstattable");
    assert_ne!(
        err.kind(),
        std::io::ErrorKind::NotFound,
        "fixture: the stat error must not be NotFound ({err}); running as root?"
    );
    let out = gate(&state.path, &elsewhere.path, &payload);
    assert_denied(&out, "unrelated edit while a target root cannot be stat'ed");
}

/// Coordinator ruling (contract gap): a gone target's OWN former path stays
/// denied — a Violation whose reason says the target is gone and names
/// `fde forget`. Only unrelated paths stop being blocked.
fn assert_denied_as_gone(out: &Output, what: &str) {
    assert_denied(out, what);
    let r = reason(out);
    assert!(
        r.contains("gone") && r.contains("fde forget"),
        "{what}: the reason must say the target is gone and name `fde forget`: {}",
        show(out)
    );
}

#[test]
fn edit_under_a_gone_root_is_denied_as_gone() {
    let t = Two::new();
    t.delete_a();
    let out = t.gate(&edit_payload(&t.elsewhere.path, &t.a_root.join("x")));
    assert_denied_as_gone(&out, "Edit of <gone root>/x");
}

#[test]
fn write_under_a_gone_root_is_denied_as_gone() {
    let t = Two::new();
    t.delete_a();
    let payload = pretooluse_payload(
        &t.elsewhere.path,
        "Write",
        json!({"file_path": s(&t.a_root.join("sub/new.txt")), "content": "x"}),
    );
    assert_denied_as_gone(&t.gate(&payload), "Write of <gone root>/sub/new.txt");
}

#[test]
fn bash_naming_a_gone_root_is_denied_as_gone() {
    let t = Two::new();
    t.delete_a();
    let cmd = format!("mkdir -p {}", s(&t.a_root.join("x")));
    let out = t.gate(&bash_payload(&t.elsewhere.path, &cmd));
    assert_denied_as_gone(&out, "Bash mkdir -p <gone root>/x from an unrelated cwd");
}

#[test]
fn bash_with_cwd_under_a_gone_root_is_denied_as_gone() {
    let t = Two::new();
    t.delete_a();
    let out = t.gate(&bash_payload(&t.a_root.join("sub"), "ls"));
    assert_denied_as_gone(&out, "Bash with cwd <gone root>/sub");
}

// --------------------------------------------- (3) status of a gone target

#[test]
fn status_of_a_gone_root_is_unratified_and_says_gone() {
    let t = Two::new();
    t.delete_a();
    let out = t.fde(&["status", &s(&t.a_root)]);
    assert_code(&out, 1, "status of gone A");
    assert!(
        stderr(&out).contains("gone"),
        "stderr must say the target is gone: {}",
        show(&out)
    );
}

#[test]
fn status_of_a_gone_root_through_a_symlinked_parent_says_gone() {
    let t = Two::new();
    let parent = t.a_root.parent().unwrap().to_path_buf();
    let name = t.a_root.file_name().unwrap().to_owned();
    let links = TempPlace::new("fde-links-");
    let link = links.path.join("tmp-link");
    std::os::unix::fs::symlink(&parent, &link).unwrap();
    let spelled = link.join(&name);
    assert_ne!(spelled, t.a_root, "fixture: a different spelling");
    t.delete_a();
    let out = t.fde(&["status", &s(&spelled)]);
    assert_code(&out, 1, "status of gone A via a symlinked parent");
    assert!(stderr(&out).contains("gone"), "{}", show(&out));
}

#[test]
fn status_of_a_nonexistent_unregistered_path_stays_undetermined() {
    let t = Two::new();
    t.delete_a();
    let nowhere = t.elsewhere.path.join("never-existed");
    let out = t.fde(&["status", &s(&nowhere)]);
    assert_code(&out, 2, "status of a nonexistent unregistered path");
}

// ------------------------------------------------------- (4) fde forget

#[test]
fn forget_a_gone_target_removes_its_state_dir_once() {
    let t = Two::new();
    let a_dir = t.key_dir(&t.a_key);
    assert!(a_dir.is_dir(), "fixture: A's state dir exists");
    t.delete_a();
    let out = t.fde(&["forget", &s(&t.a_root)]);
    assert_code(&out, 0, "forget gone A");
    assert!(!a_dir.exists(), "A's state dir must be removed");
    assert!(t.key_dir(&t.b_key).is_dir(), "B's state dir is untouched");
    let again = t.fde(&["forget", &s(&t.a_root)]);
    assert_code(&again, 1, "forget gone A again (not registered)");
    // After forget, A no longer exists as a registration; status of its
    // former path is an ordinary nonexistent path again.
    assert_code(
        &t.fde(&["status", &s(&t.a_root)]),
        2,
        "status of A's former path after forget",
    );
}

#[test]
fn forget_a_gone_target_by_relative_path_and_by_symlinked_spelling() {
    let t = Two::new();
    let parent = t.a_root.parent().unwrap().to_path_buf();
    let name = t.a_root.file_name().unwrap().to_str().unwrap().to_owned();
    t.delete_a();
    // Relative TARGET resolves against cwd.
    let out = run_fde(&parent, t.state(), &["forget", &name]);
    assert_code(&out, 0, "forget gone A by a relative path");
    assert!(!t.key_dir(&t.a_key).exists());

    // Same for a symlinked-parent spelling of another gone target.
    let u = Two::new();
    let links = TempPlace::new("fde-links-");
    let link = links.path.join("tmp-link");
    std::os::unix::fs::symlink(u.a_root.parent().unwrap(), &link).unwrap();
    let spelled = link.join(u.a_root.file_name().unwrap());
    u.delete_a();
    let out = u.fde(&["forget", &s(&spelled)]);
    assert_code(&out, 0, "forget gone A via a symlinked parent");
    assert!(!u.key_dir(&u.a_key).exists());
}

#[test]
fn forget_an_existing_target_unguards_it_and_never_touches_it() {
    let t = Two::new();
    let inside_b = t.b.root.join("README.md");
    assert_denied(
        &t.gate(&edit_payload(&t.elsewhere.path, &inside_b)),
        "control: B guarded before forget",
    );
    let before = snapshot_files(&t.b.root);
    // TARGET given as a subdirectory: the key is B's git top-level.
    let sub = t.b.subdir("deep/er");
    let before_with_sub = snapshot_files(&t.b.root);
    let out = t.fde(&["forget", &s(&sub)]);
    assert_code(&out, 0, "forget B via a subdirectory");
    assert!(!t.key_dir(&t.b_key).exists(), "B's state dir removed");
    assert!(t.key_dir(&t.a_key).is_dir(), "A's state dir untouched");
    assert_eq!(
        snapshot_files(&t.b.root),
        before_with_sub,
        "forget must not modify the target"
    );
    assert_eq!(before, before_with_sub, "fixture: subdir adds no files");
    assert_allowed(
        &t.gate(&edit_payload(&t.elsewhere.path, &inside_b)),
        "edit inside B after forget",
    );
}

#[test]
fn forget_without_target_uses_the_cwd_top_level() {
    let t = Two::new();
    let out = run_fde(&t.b.root, t.state(), &["forget"]);
    assert_code(&out, 0, "forget with cwd = B");
    assert!(!t.key_dir(&t.b_key).exists());
    assert!(t.key_dir(&t.a_key).is_dir());
}

#[test]
fn forget_an_unregistered_path_is_refused() {
    let t = Two::new();
    let other = TempRepo::new();
    assert_code(
        &t.fde(&["forget", &s(&other.root)]),
        1,
        "forget an existing unregistered repo",
    );
    assert_code(
        &t.fde(&["forget", &s(&t.elsewhere.path.join("never-existed"))]),
        1,
        "forget a nonexistent unregistered path",
    );
    assert!(t.key_dir(&t.a_key).is_dir() && t.key_dir(&t.b_key).is_dir());
}

#[test]
fn corrupt_registration_denies_everything_until_forgotten() {
    let e = Engagement::new(&[("R1", "first")]);
    e.begin();
    let (_root, key) = parse_resolved(&e.fde(&["resolve"]));
    let dir = e.state.path.join("targets").join(&key);
    fs::write(dir.join("registration.json"), "{not json").unwrap();
    let elsewhere = TempPlace::new("fde-elsewhere-");
    let payload = edit_payload(&elsewhere.path, &elsewhere.path.join("x.txt"));
    assert_denied(
        &gate(&e.state.path, &elsewhere.path, &payload),
        "unrelated edit with a corrupt registration",
    );
    let before = snapshot_files(e.root());
    let out = e.fde(&["forget"]);
    assert_code(&out, 0, "forget a target with a corrupt registration");
    assert!(!dir.exists(), "its state dir is removed");
    assert_eq!(snapshot_files(e.root()), before, "target untouched");
    assert_allowed(
        &gate(&e.state.path, &elsewhere.path, &payload),
        "unrelated edit after forgetting the corrupt registration",
    );
}

#[test]
fn forget_that_cannot_remove_is_undetermined() {
    use std::os::unix::fs::PermissionsExt;
    let e = Engagement::new(&[("R1", "first")]);
    e.begin();
    let (_root, key) = parse_resolved(&e.fde(&["resolve"]));
    let targets = e.state.path.join("targets");
    let dir = targets.join(&key);
    // Neither the per-target dir nor its parent is writable: nothing in it
    // can be unlinked, and it cannot be renamed away.
    let restore_targets = ModeGuard(targets.clone(), 0o755);
    let restore_dir = ModeGuard(dir.clone(), 0o755);
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).unwrap();
    fs::set_permissions(&targets, fs::Permissions::from_mode(0o555)).unwrap();
    let out = e.fde(&["forget"]);
    assert_code(&out, 2, "forget with an unremovable state dir");
    assert!(!stderr(&out).trim().is_empty(), "{}", show(&out));
    drop(restore_dir);
    drop(restore_targets);
    assert!(
        dir.join("registration.json").is_file(),
        "a failed forget must not report removal; the registration remains"
    );
    // Partner: with the permissions restored the same forget succeeds, so the
    // exit 2 above came from the failed removal (not from e.g. a usage error).
    assert_code(&e.fde(&["forget"]), 0, "forget after restoring permissions");
    assert!(!dir.exists(), "state dir removed once removable");
}

// ------------------------------------------- (5) gate: forget is human-only

const FORGET_COMMANDS: &[&str] = &[
    "fde forget /some/path",
    "fde forget",
    "/usr/local/bin/fde forget x",
    "cd /tmp && fde forget x",
    "true;fde forget x",
    "(fde forget x)",
    "true\nfde forget x",
    "'fde' forget x",
    "\"fde\" \"forget\" x",
    "fde\tforget x",
];

#[test]
fn bash_fde_forget_is_denied_as_human_only_while_a_target_is_registered() {
    // A RATIFIED target registered elsewhere: the deny must come from the
    // forget rule, not from touching an unratified area.
    let e = Engagement::ratified(&[("R1", "first")]);
    let elsewhere = TempPlace::new("fde-elsewhere-");
    assert_allowed(
        &gate(
            &e.state.path,
            &elsewhere.path,
            &bash_payload(&elsewhere.path, "ls"),
        ),
        "control: unrelated Bash is allowed",
    );
    for cmd in FORGET_COMMANDS {
        let out = gate(
            &e.state.path,
            &elsewhere.path,
            &bash_payload(&elsewhere.path, cmd),
        );
        assert_denied(&out, &format!("Bash {cmd:?}"));
        let r = reason(&out);
        assert!(
            r.contains("fde forget") && r.contains("human-only"),
            "{cmd:?}: the reason must say `fde forget` is human-only: {}",
            show(&out)
        );
    }
}

#[test]
fn bash_fde_forget_is_denied_while_only_a_gone_target_is_registered() {
    let e = Engagement::new(&[("R1", "first")]);
    e.begin();
    let root = e.root().to_path_buf();
    fs::remove_dir_all(&root).unwrap();
    let elsewhere = TempPlace::new("fde-elsewhere-");
    // Both naming the gone root and naming something else are denied by the
    // forget rule (its reason names `fde forget` as human-only).
    for cmd in [
        format!("fde forget {}", s(&root)),
        "fde forget x".to_string(),
    ] {
        let out = gate(
            &e.state.path,
            &elsewhere.path,
            &bash_payload(&elsewhere.path, &cmd),
        );
        assert_denied(
            &out,
            &format!("Bash {cmd:?} with only a gone target registered"),
        );
        let r = reason(&out);
        assert!(
            r.contains("fde forget") && r.contains("human-only"),
            "{cmd:?}: the reason must say `fde forget` is human-only: {}",
            show(&out)
        );
    }
}

#[test]
fn bash_fde_forget_is_allowed_with_no_registered_target() {
    let state = TempPlace::new("fde-state-");
    let elsewhere = TempPlace::new("fde-elsewhere-");
    for cmd in FORGET_COMMANDS {
        let out = gate(
            &state.path,
            &elsewhere.path,
            &bash_payload(&elsewhere.path, cmd),
        );
        assert_allowed(&out, &format!("Bash {cmd:?} with nothing registered"));
    }
}
