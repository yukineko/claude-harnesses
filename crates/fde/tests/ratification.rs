// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Ratification contract tests (`fde begin`, `fde ratify`, `fde status`).
//!
//! Written by an independent test author (CLAUDE.md §2(a)); the implementer
//! must match the contract, not edit these tests to pass.
//!
//! Contract under test (`crates/fde/src/main.rs` + the shared additions):
//!   - `fde begin [TARGET] --spec FILE` registers TARGET with the draft spec
//!     FILE (which must live outside the target): 0 / 1 refused / 2.
//!   - `fde ratify [TARGET] --spec FILE` records ratification of the
//!     registered spec with a content fingerprint (sha256 hex of the spec
//!     bytes) in the state dir: 0 / 1 refused / 2.
//!   - `fde status [TARGET]`: 0 ratified (fingerprint equals the current spec
//!     bytes) / 1 unratified (also: not registered, reason "not registered") /
//!     2 undetermined (state or spec unreadable or corrupt).
//!   - The ratification record lives in `FDE_STATE_DIR`, never inside the
//!     target and never inside the spec file (spike-specforge decision: fde
//!     reuses only `specforge draft`, whose output lands in the state dir, e.g.
//!     `<state>/specs/<id>.toml`; fde keeps the ratification itself).
//!
//! Discrimination: every test pairs a positive and a negative assertion with
//! pinned exit codes, so a binary that always exits 0, always 1 or always 2
//! fails each of them.

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use sha2::{Digest, Sha256};

use common::{code, run_fde, show, snapshot_files, stderr, write_spec, TempPlace, TempRepo};

/// Target repo + state dir, with the draft spec in the state dir as
/// `specforge draft` would write it (`<state>/specs/<id>.toml`).
struct Fx {
    repo: TempRepo,
    state: TempPlace,
    spec: PathBuf,
}

impl Fx {
    fn new() -> Self {
        Self::with_state(TempPlace::new("fde-state-"))
    }

    fn with_state(state: TempPlace) -> Self {
        let repo = TempRepo::new();
        let specs = state.path.join("specs");
        fs::create_dir_all(&specs).unwrap();
        // Unique spec id per target so several fixtures can share one state dir.
        let id = repo.root.file_name().unwrap().to_str().unwrap().to_owned();
        let spec = write_spec(
            &specs,
            &format!("{id}.toml"),
            &id,
            &[("R1", "the first requirement"), ("R2", "the second")],
        );
        Fx { repo, state, spec }
    }

    fn root(&self) -> &Path {
        &self.repo.root
    }

    fn spec_str(&self) -> &str {
        self.spec.to_str().unwrap()
    }

    fn fde_in(&self, cwd: &Path, args: &[&str]) -> Output {
        run_fde(cwd, &self.state.path, args)
    }

    fn fde(&self, args: &[&str]) -> Output {
        self.fde_in(self.root(), args)
    }

    fn begin(&self) -> Output {
        self.fde(&["begin", "--spec", self.spec_str()])
    }

    fn ratify(&self) -> Output {
        self.fde(&["ratify", "--spec", self.spec_str()])
    }

    fn status(&self) -> Output {
        self.fde(&["status"])
    }
}

fn assert_code(out: &Output, want: i32, what: &str) {
    assert_eq!(code(out), Some(want), "{what}: {}", show(out));
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Files under `dir` (absolute) whose content contains `needle`, skipping `skip`.
fn files_containing(dir: &Path, needle: &str, skip: &Path) -> Vec<PathBuf> {
    snapshot_files(dir)
        .into_iter()
        .map(|(rel, bytes)| (dir.join(rel), bytes))
        .filter(|(p, bytes)| p != skip && String::from_utf8_lossy(bytes).contains(needle))
        .map(|(p, _)| p)
        .collect()
}

/// State files (absolute) that are new or changed between two snapshots.
fn changed(
    dir: &Path,
    before: &BTreeMap<PathBuf, Vec<u8>>,
    after: &BTreeMap<PathBuf, Vec<u8>>,
) -> Vec<PathBuf> {
    common::changed_files(dir, before, after)
}

/// Recursive `relative path -> sha256 hex` listing of `dir`, excluding
/// `.git/index` (git's own stat cache, which `git status` itself may rewrite).
fn content_hash_listing(dir: &Path) -> BTreeMap<PathBuf, String> {
    snapshot_files(dir)
        .into_iter()
        .filter(|(rel, _)| rel != Path::new(".git/index"))
        .map(|(rel, bytes)| (rel, sha256_hex(&bytes)))
        .collect()
}

/// chmod-000 tests are vacuous as root (root reads anything): refuse to run.
fn panic_if_root() {
    let out = Command::new("id").arg("-u").output().expect("spawn id -u");
    assert!(out.status.success(), "id -u failed");
    let uid = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if uid == "0" {
        panic!("this test cannot observe an unreadable file when running as root; run as a normal user");
    }
}

fn set_mode(p: &Path, mode: u32) {
    fs::set_permissions(p, fs::Permissions::from_mode(mode)).unwrap();
}

/// Precondition: `dir` is outside any git work tree.
fn assert_not_in_git(dir: &Path) {
    let out = Command::new("git")
        .current_dir(dir)
        .args(["rev-parse", "--is-inside-work-tree"])
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .expect("spawn git");
    assert!(
        !out.status.success(),
        "fixture precondition: {} must not be inside a git work tree",
        dir.display()
    );
}

// (1) registered-but-unratified and unregistered are both unratified (exit 1).
#[test]
fn registered_without_ratification_and_unregistered_are_unratified() {
    let fx = Fx::new();

    // Unregistered: determined "unratified", with a "not registered" reason.
    let out = fx.status();
    assert_code(&out, 1, "status of an unregistered target");
    assert!(
        stderr(&out).to_lowercase().contains("not registered"),
        "unregistered status must say \"not registered\": {}",
        show(&out)
    );

    // Registration is a positive answer (rules out always-1 / always-2).
    assert_code(&fx.begin(), 0, "begin with a draft spec in the state dir");

    // Registered, not ratified: still exit 1 (rules out always-0).
    let out = fx.status();
    assert_code(&out, 1, "status after begin, before ratify");
    assert!(
        !stderr(&out).to_lowercase().contains("not registered"),
        "a registered target must not be reported as not registered: {}",
        show(&out)
    );
}

// (2) ratify -> ratified, and the fingerprint is in the state dir.
#[test]
fn ratify_records_the_content_fingerprint_in_the_state_dir() {
    let fx = Fx::new();
    assert_code(&fx.begin(), 0, "begin");
    assert_code(&fx.status(), 1, "status before ratify");

    let before = snapshot_files(&fx.state.path);
    assert_code(&fx.ratify(), 0, "ratify");
    let after = snapshot_files(&fx.state.path);
    assert_code(&fx.status(), 0, "status after ratify");

    let want = sha256_hex(&fs::read(&fx.spec).unwrap());
    let holders = files_containing(&fx.state.path, &want, &fx.spec);
    assert!(
        !holders.is_empty(),
        "no state file (other than the spec) contains the sha256 {want} of the spec; state files: {:?}",
        after.keys().collect::<Vec<_>>()
    );
    let changed = changed(&fx.state.path, &before, &after);
    assert!(
        holders.iter().any(|h| changed.contains(h)),
        "the fingerprint must be written by ratify; holders {holders:?}, changed by ratify {changed:?}"
    );
}

// (3) editing the spec after ratification un-ratifies; re-ratify restores.
#[test]
fn editing_the_spec_unratifies_and_reratify_restores() {
    let fx = Fx::new();
    assert_code(&fx.begin(), 0, "begin");
    assert_code(&fx.ratify(), 0, "ratify");
    assert_code(&fx.status(), 0, "status after ratify");

    let mut text = fs::read_to_string(&fx.spec).unwrap();
    text.push_str("\n[[requirement]]\nid = \"R3\"\nstatement = \"added after ratification\"\n");
    fs::write(&fx.spec, &text).unwrap();
    assert_code(&fx.status(), 1, "status after editing the ratified spec");

    assert_code(&fx.ratify(), 0, "re-ratify the edited spec");
    assert_code(&fx.status(), 0, "status after re-ratify");
    let want = sha256_hex(text.as_bytes());
    assert!(
        !files_containing(&fx.state.path, &want, &fx.spec).is_empty(),
        "re-ratify must record the fingerprint {want} of the edited spec"
    );
}

// (4a) corrupt ratification state -> undetermined (2), never ratified.
#[test]
fn corrupt_state_is_undetermined_not_ratified() {
    let fx = Fx::new();
    assert_code(&fx.begin(), 0, "begin");
    let before = snapshot_files(&fx.state.path);
    assert_code(&fx.ratify(), 0, "ratify");
    let after = snapshot_files(&fx.state.path);
    // Positive control: the intact state reads as ratified.
    assert_code(&fx.status(), 0, "status with intact state");

    let written = changed(&fx.state.path, &before, &after);
    assert!(
        !written.is_empty(),
        "ratify wrote no state file; nothing to corrupt"
    );
    for p in &written {
        fs::write(p, b"{{{ \0 not json ]]] = = not toml \0\n").unwrap();
    }
    let out = fx.status();
    assert_ne!(
        code(&out),
        Some(0),
        "corrupt state must never read as ratified: {}",
        show(&out)
    );
    assert_code(&out, 2, "status with corrupt state");
}

// (4b) unreadable ratification state -> undetermined (2), never ratified.
#[test]
fn unreadable_state_is_undetermined_not_ratified() {
    panic_if_root();
    let fx = Fx::new();
    assert_code(&fx.begin(), 0, "begin");
    let before = snapshot_files(&fx.state.path);
    assert_code(&fx.ratify(), 0, "ratify");
    let after = snapshot_files(&fx.state.path);
    assert_code(&fx.status(), 0, "status with readable state");

    let written = changed(&fx.state.path, &before, &after);
    assert!(
        !written.is_empty(),
        "ratify wrote no state file; nothing to make unreadable"
    );
    for p in &written {
        set_mode(p, 0o000);
    }
    let out = fx.status();
    for p in &written {
        set_mode(p, 0o644);
    }
    assert_ne!(
        code(&out),
        Some(0),
        "unreadable state must never read as ratified: {}",
        show(&out)
    );
    assert_code(&out, 2, "status with unreadable state");
}

// (4c) unreadable spec after ratification -> undetermined (2).
#[test]
fn unreadable_spec_after_ratification_is_undetermined() {
    panic_if_root();
    let fx = Fx::new();
    assert_code(&fx.begin(), 0, "begin");
    assert_code(&fx.ratify(), 0, "ratify");
    assert_code(&fx.status(), 0, "status with readable spec");

    set_mode(&fx.spec, 0o000);
    let out = fx.status();
    set_mode(&fx.spec, 0o644);
    assert_ne!(
        code(&out),
        Some(0),
        "an unreadable spec must never read as ratified: {}",
        show(&out)
    );
    assert_code(&out, 2, "status with unreadable spec");

    // Readable again: back to ratified (the record itself was not damaged).
    assert_code(&fx.status(), 0, "status after the spec is readable again");
}

// (5) begin + ratify + status write nothing inside the target, nor into the spec.
#[test]
fn ratification_writes_nothing_into_the_target_or_the_spec() {
    let fx = Fx::new();
    // Settle git's index stat cache first so the porcelain probe is stable.
    let porcelain_before = fx.repo.porcelain_ignored();
    let listing_before = content_hash_listing(fx.root());
    let spec_before = fs::read(&fx.spec).unwrap();

    assert_code(&fx.begin(), 0, "begin");
    assert_code(&fx.status(), 1, "status before ratify");
    assert_code(&fx.ratify(), 0, "ratify");
    assert_code(&fx.status(), 0, "status after ratify");

    let listing_after = content_hash_listing(fx.root());
    let porcelain_after = fx.repo.porcelain_ignored();
    assert_eq!(
        listing_before, listing_after,
        "begin/ratify/status must not create or change any file inside the target"
    );
    assert_eq!(
        porcelain_before, porcelain_after,
        "git status --porcelain --ignored of the target must be unchanged"
    );
    assert_eq!(
        spec_before,
        fs::read(&fx.spec).unwrap(),
        "ratify must not write the ratification into the spec file"
    );
}

// (6) the ratification is visible from a nested subdir and via a relative path.
#[test]
fn ratification_is_visible_from_subdir_and_relative_path() {
    let fx = Fx::new();
    let nested = fx.repo.subdir("a/b/c");
    let parent = fx.root().parent().unwrap().to_path_buf();
    let elsewhere = TempPlace::new("fde-elsewhere-");
    assert_eq!(
        elsewhere.path.parent(),
        Some(parent.as_path()),
        "fixture precondition: temp dirs share one parent"
    );
    let rel = format!("../{}", fx.root().file_name().unwrap().to_str().unwrap());

    assert_code(&fx.begin(), 0, "begin");
    // Negative side: registered but unratified from both spellings.
    assert_code(
        &fx.fde_in(&nested, &["status"]),
        1,
        "nested status before ratify",
    );
    assert_code(
        &fx.fde_in(&elsewhere.path, &["status", &rel]),
        1,
        "relative status before ratify",
    );

    assert_code(&fx.ratify(), 0, "ratify");
    assert_code(
        &fx.fde_in(&nested, &["status"]),
        0,
        "nested status after ratify",
    );
    assert_code(
        &fx.fde_in(&elsewhere.path, &["status", &rel]),
        0,
        "relative status after ratify",
    );
}

// (7) begin refuses a spec inside the target and a missing spec.
#[test]
fn begin_refuses_spec_inside_target_and_missing_spec() {
    let fx = Fx::new();

    let inside = write_spec(fx.root(), "spec.toml", "inside", &[("R1", "x")]);
    let out = fx.fde(&["begin", "--spec", inside.to_str().unwrap()]);
    assert_code(&out, 1, "begin with a spec inside the target");
    let nested = fx.repo.subdir("docs");
    let inside_nested = write_spec(&nested, "spec.toml", "inside2", &[("R1", "x")]);
    let out = fx.fde(&["begin", "--spec", inside_nested.to_str().unwrap()]);
    assert_code(&out, 1, "begin with a spec in a subdir of the target");

    let missing = fx.state.path.join("specs").join("does-not-exist.toml");
    let out = fx.fde(&["begin", "--spec", missing.to_str().unwrap()]);
    assert_code(&out, 1, "begin with a missing spec");

    // Refused begins registered nothing.
    let out = fx.status();
    assert_code(&out, 1, "status after refused begins");
    assert!(
        stderr(&out).to_lowercase().contains("not registered"),
        "refused begins must leave the target not registered: {}",
        show(&out)
    );

    // Positive control: a spec outside the target is accepted.
    assert_code(&fx.begin(), 0, "begin with a spec outside the target");
}

// (8) a non-git target is undetermined for begin, ratify and status.
#[test]
fn non_git_target_is_undetermined() {
    let state = TempPlace::new("fde-state-");
    let plain = TempPlace::new("fde-plain-");
    assert_not_in_git(&plain.path);
    let specs = state.path.join("specs");
    fs::create_dir_all(&specs).unwrap();
    let spec = write_spec(&specs, "plain.toml", "plain", &[("R1", "x")]);
    let spec_s = spec.to_str().unwrap();
    let plain_s = plain.path.to_str().unwrap();

    // Positive control: the same commands on a git repo answer 0.
    let fx = Fx::with_state(TempPlace::new("fde-state-"));
    assert_code(&fx.begin(), 0, "control: begin on a git repo");
    assert_code(&fx.ratify(), 0, "control: ratify on a git repo");
    assert_code(&fx.status(), 0, "control: status on a git repo");

    for (cwd, args, what) in [
        (&plain.path, vec!["begin", "--spec", spec_s], "begin (cwd)"),
        (
            &plain.path,
            vec!["ratify", "--spec", spec_s],
            "ratify (cwd)",
        ),
        (&plain.path, vec!["status"], "status (cwd)"),
        (
            &state.path,
            vec!["begin", plain_s, "--spec", spec_s],
            "begin (arg)",
        ),
        (
            &state.path,
            vec!["ratify", plain_s, "--spec", spec_s],
            "ratify (arg)",
        ),
        (&state.path, vec!["status", plain_s], "status (arg)"),
    ] {
        let out = run_fde(cwd, &state.path, &args);
        assert_code(&out, 2, &format!("non-git target: {what}"));
    }
}

// (9) ratifying repo A does not ratify repo B.
#[test]
fn ratification_is_per_target() {
    let shared = TempPlace::new("fde-state-");
    let shared_path = shared.path.clone();
    let a = Fx::with_state(shared);
    // B shares A's state dir and gets its own spec there.
    let b = TempRepo::new();
    let b_spec = write_spec(
        &shared_path.join("specs"),
        "b.toml",
        "b",
        &[("R1", "b requirement")],
    );
    let b_status = || run_fde(&b.root, &shared_path, &["status"]);

    assert_code(&a.begin(), 0, "A begin");
    assert_code(&a.ratify(), 0, "A ratify");
    assert_code(&a.status(), 0, "A status after ratify");

    let out = b_status();
    assert_code(&out, 1, "B status (unregistered) while A is ratified");
    assert!(
        stderr(&out).to_lowercase().contains("not registered"),
        "B must be reported as not registered: {}",
        show(&out)
    );

    let out = run_fde(
        &b.root,
        &shared_path,
        &["begin", "--spec", b_spec.to_str().unwrap()],
    );
    assert_code(&out, 0, "B begin");
    assert_code(
        &b_status(),
        1,
        "B status (registered, unratified) while A is ratified",
    );
    assert_code(&a.status(), 0, "A still ratified after B begin");

    // A third repo C registered with A's (ratified) spec is still not ratified.
    let c = TempRepo::new();
    let out = run_fde(&c.root, &shared_path, &["begin", "--spec", a.spec_str()]);
    assert_code(&out, 0, "C begin with A's spec");
    assert_code(
        &run_fde(&c.root, &shared_path, &["status"]),
        1,
        "C registered with A's ratified spec is not ratified by A's ratification",
    );
}
