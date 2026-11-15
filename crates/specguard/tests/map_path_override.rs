// Integration tests: unwrap/expect are fine here (workspace lints are for production code).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Tests for the global `--map-path <PATH>` option of `specguard`.
//!
//! Contract (SPEC section D): when `--map-path P` is given, every `map` action
//! (including `map gate-check`) reads and writes P instead of
//! `<repo_root>/<[map].path>`; a relative P resolves against the current
//! directory; `map build` with a nonexistent P creates a fresh map there; the
//! repo's own `.specguard/spec-map.toml` is never read or written. Without the
//! flag, behaviour is unchanged.
//!
//! The flag exists to close a fail-open in the pre-push gate: a machine-local
//! repo map whose entry carries a `spec_doc` would otherwise satisfy
//! `map gate-check` for a pushed change, although that spec_doc is untracked.
//!
//! Written by an independent test author (CLAUDE.md §2(a)) who did not write the
//! implementation. Fixture style mirrors `gate_check_spec_docs.rs`.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

const GATE_PATH: &str = "crates/blastguard/src/a.rs";
const REPO_MAP: &str = ".specguard/spec-map.toml";

fn git(repo: &Path, args: &[&str]) -> String {
    let o = harness_core::git_env::scrub(&mut Command::new("git"))
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

fn write(repo: &Path, rel: &str, body: &str) {
    let p = repo.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, body).unwrap();
}

/// Base commit (config, gate file, README); the repo map is gitignored, as in
/// the real repo. Returns the base sha.
fn fixture(repo: &Path) -> String {
    git(repo, &["init", "-q", "-b", "main"]);
    git(repo, &["config", "user.email", "t@t.t"]);
    git(repo, &["config", "user.name", "t"]);
    git(repo, &["config", "commit.gpgsign", "false"]);
    write(
        repo,
        "specguard.toml",
        "[project]\nname = \"Demo\"\nroot = \".\"\n[output]\nreport_dir = \"reports\"\nsentinel = \".pending\"\n\n[[area]]\nname = \"crates\"\nglobs = [\"crates/**\"]\ncanon = []\n",
    );
    write(repo, ".gitignore", ".specguard/spec-map.toml\nreports/\n");
    write(repo, GATE_PATH, "fn a() {}\n");
    write(repo, "README.md", "# Demo\n\nA real, non-blank readme.\n");
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "base"]);
    git(repo, &["rev-parse", "HEAD"])
}

fn commit_change(repo: &Path) -> String {
    write(repo, GATE_PATH, "fn changed() {}\n");
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "change"]);
    git(repo, &["rev-parse", "HEAD"])
}

/// A local repo map whose entry covers GATE_PATH with a non-empty spec_doc.
/// This alone satisfies `map gate-check` when the repo map is used.
const LOCAL_MAP_WITH_SPEC_DOC: &str =
    "last_synced = \"x\"\n\n[entries.\"blast\"]\nkey = \"blast\"\n\
     spec_doc = \"docs/specs/blastguard.md\"\nstatus = \"changed\"\n\
     impl_files = [\"crates/blastguard/src/a.rs\"]\n";

/// Runs the specguard binary with cwd `cwd` and an absolute `--config`.
fn sg(cwd: &Path, repo: &Path, home: &Path, args: &[&str]) -> Output {
    let o = harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_specguard")))
        .current_dir(cwd)
        .env("HOME", home)
        .env_remove("SPECGUARD_BASELINE_REF")
        .arg("--config")
        .arg(repo.join("specguard.toml"))
        .args(args)
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        !err.contains("unexpected argument") && !err.contains("unrecognized subcommand"),
        "a CLI usage error must not satisfy any assertion below: {}",
        describe(&o)
    );
    o
}

fn describe(o: &Output) -> String {
    format!(
        "exit={:?}\nstdout={}\nstderr={}",
        o.status.code(),
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn read_opt(p: &Path) -> Option<Vec<u8>> {
    fs::read(p).ok()
}

// ---- 1. map build writes P, never the repo map ------------------------------

#[test]
fn map_build_with_map_path_creates_map_there_and_not_in_repo() {
    let d = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let base = fixture(d.path());
    commit_change(d.path());
    let p = out.path().join("m.toml");
    let o = sg(
        d.path(),
        d.path(),
        home.path(),
        &[
            "--map-path",
            p.to_str().unwrap(),
            "map",
            "build",
            "--baseline",
            &base,
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", describe(&o));
    let body = fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("map build must create the map at --map-path {p:?}: {e}"));
    assert!(
        body.contains(GATE_PATH),
        "the map built at --map-path must contain the changed path {GATE_PATH}:\n{body}"
    );
    assert!(
        !d.path().join(REPO_MAP).exists(),
        "with --map-path, map build must NOT create the repo map {REPO_MAP}"
    );
}

#[test]
fn map_build_with_map_path_leaves_existing_repo_map_byte_identical() {
    let d = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let base = fixture(d.path());
    commit_change(d.path());
    write(d.path(), REPO_MAP, LOCAL_MAP_WITH_SPEC_DOC);
    let before = read_opt(&d.path().join(REPO_MAP));
    let p = out.path().join("m.toml");
    let o = sg(
        d.path(),
        d.path(),
        home.path(),
        &[
            "--map-path",
            p.to_str().unwrap(),
            "map",
            "build",
            "--baseline",
            &base,
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", describe(&o));
    assert!(p.is_file(), "map build must create {p:?}\n{}", describe(&o));
    assert_eq!(
        read_opt(&d.path().join(REPO_MAP)),
        before,
        "with --map-path, map build must not modify the repo map {REPO_MAP}"
    );
    let built = fs::read_to_string(&p).unwrap();
    assert!(
        !built.contains("docs/specs/blastguard.md"),
        "the fresh map must not inherit the repo map's spec_doc (it must not read \
         the repo map):\n{built}"
    );
}

// ---- 2. gate-check ignores the repo map's spec_doc (the fail-open) ----------

#[test]
fn gate_check_with_fresh_map_path_ignores_local_repo_map_spec_doc() {
    let d = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let base = fixture(d.path());
    let head = commit_change(d.path());
    write(d.path(), REPO_MAP, LOCAL_MAP_WITH_SPEC_DOC);
    let before = read_opt(&d.path().join(REPO_MAP));
    let p = out.path().join("spec-map.toml");
    let ps = p.to_str().unwrap();

    let b = sg(
        d.path(),
        d.path(),
        home.path(),
        &["--map-path", ps, "map", "build", "--baseline", &base],
    );
    assert_eq!(b.status.code(), Some(0), "build: {}", describe(&b));
    let o = sg(
        d.path(),
        d.path(),
        home.path(),
        &[
            "--map-path",
            ps,
            "map",
            "gate-check",
            "--base",
            &base,
            "--head",
            &head,
        ],
    );
    assert_eq!(
        o.status.code(),
        Some(1),
        "a local repo-map spec_doc must NOT satisfy gate-check when a fresh \
         --map-path map is given: the fresh map has no spec_doc, so the changed gate \
         path is a violation (exit 1)\n{}",
        describe(&o)
    );
    assert!(
        describe(&o).contains(GATE_PATH),
        "the unsatisfied gate path must be reported\n{}",
        describe(&o)
    );
    assert_eq!(
        read_opt(&d.path().join(REPO_MAP)),
        before,
        "the repo map must not be modified by build or gate-check with --map-path"
    );
}

// ---- 3. without the flag, the repo map is used (unchanged behaviour) --------

#[test]
fn control_without_map_path_gate_check_uses_repo_map_spec_doc() {
    let d = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let base = fixture(d.path());
    let head = commit_change(d.path());
    write(d.path(), REPO_MAP, LOCAL_MAP_WITH_SPEC_DOC);
    let o = sg(
        d.path(),
        d.path(),
        home.path(),
        &["map", "gate-check", "--base", &base, "--head", &head],
    );
    assert_eq!(
        o.status.code(),
        Some(0),
        "without --map-path the repo map (whose entry has a spec_doc) is read, so \
         the change passes\n{}",
        describe(&o)
    );
}

#[test]
fn control_without_map_path_map_build_writes_repo_map() {
    let d = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let base = fixture(d.path());
    commit_change(d.path());
    let o = sg(
        d.path(),
        d.path(),
        home.path(),
        &["map", "build", "--baseline", &base],
    );
    assert_eq!(o.status.code(), Some(0), "{}", describe(&o));
    let body = fs::read_to_string(d.path().join(REPO_MAP))
        .unwrap_or_else(|e| panic!("without --map-path, map build writes {REPO_MAP}: {e}"));
    assert!(body.contains(GATE_PATH), "{body}");
}

// ---- 4. relative --map-path resolves against the cwd ------------------------

#[test]
fn relative_map_path_resolves_against_cwd_not_repo_root() {
    let d = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let base = fixture(d.path());
    commit_change(d.path());
    let o = sg(
        cwd.path(),
        d.path(),
        home.path(),
        &[
            "--map-path",
            "sub/m.toml",
            "map",
            "build",
            "--baseline",
            &base,
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", describe(&o));
    assert!(
        cwd.path().join("sub/m.toml").is_file(),
        "a relative --map-path must resolve against the current directory\n{}",
        describe(&o)
    );
    assert!(
        !d.path().join("sub/m.toml").exists(),
        "a relative --map-path must NOT resolve against the repo root"
    );
    assert!(
        !d.path().join(REPO_MAP).exists(),
        "the repo map must not be created when --map-path is given"
    );
}

#[test]
fn control_map_build_from_a_cwd_outside_the_repo_works_without_the_flag() {
    // Premise for the relative-path test: running with cwd outside the repo and
    // an absolute --config is otherwise supported, so a failure there is about
    // --map-path resolution, not about the cwd.
    let d = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let base = fixture(d.path());
    commit_change(d.path());
    let o = sg(
        cwd.path(),
        d.path(),
        home.path(),
        &["map", "build", "--baseline", &base],
    );
    assert_eq!(o.status.code(), Some(0), "{}", describe(&o));
    assert!(d.path().join(REPO_MAP).is_file(), "{}", describe(&o));
}

#[test]
fn control_freshly_built_map_without_spec_doc_is_a_violation() {
    // Premise for the fail-open test: a map freshly built by `map build
    // --baseline <base>` carries no spec_doc, so gate-check on it exits 1. Here
    // it is the repo map (no flag, no prior local map).
    let d = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let base = fixture(d.path());
    let head = commit_change(d.path());
    let b = sg(
        d.path(),
        d.path(),
        home.path(),
        &["map", "build", "--baseline", &base],
    );
    assert_eq!(b.status.code(), Some(0), "build: {}", describe(&b));
    let o = sg(
        d.path(),
        d.path(),
        home.path(),
        &["map", "gate-check", "--base", &base, "--head", &head],
    );
    assert_eq!(o.status.code(), Some(1), "{}", describe(&o));
}
