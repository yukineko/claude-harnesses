// このファイルは丸ごと integration test なので unwrap/expect を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED, 2026-11-14, by a non-implementer (flow-8cca7ef5 RED agent).
//!
//! Property: blastguard's integration-test fixtures — in particular every git
//! repository they create — are never placed under the workspace `target/`.
//!
//! Why: the forced `git worktree remove` check answers Ask (subagent: Deny)
//! when an ignored build dir inside the named worktree holds any nested `.git`
//! entry (a nested repo can hold uncommitted work). Fixture repos left in
//! `<worktree>/target/tmp/blastguard-worktree-root-fixtures` therefore make
//! every harness worktree that ever ran these tests un-removable without a
//! prompt (observed: 842 `.git` entries in one worktree's `target/tmp`).
//!
//! Observed now (origin/main 0be167b3):
//!   * `neutral_base()` resolves, canonically, under `<workspace>/target/tmp`
//!     (candidate `CARGO_TARGET_TMPDIR` wins when no
//!     `BLASTGUARD_TEST_FIXTURE_DIR` override is set).
//!   * Several test files build git fixtures from `neutral_base` or from
//!     `CARGO_TARGET_TMPDIR` directly (listed in the failure message).
//!
//! Both RED tests assert decisions about observed paths / file contents and
//! are `#[ignore]`d until the fix lands.
//!
//! DOES NOT PROVE: (0) the scan is file-level: a file that mentions
//! `neutral_base` / `CARGO_TARGET_TMPDIR` AND runs git `init` is listed even if
//! its git repo lives elsewhere (e.g. a `tempfile` dir), so the list is an
//! upper bound of offenders, not a per-repo path observation; the per-path
//! observation is `neutral_base_not_under_workspace_target`. (a) the textual scan (`git_fixture_files_not_under_target`)
//! sees only source text: a file that builds its repo through a helper that
//! hides `"init"` / `"worktree"` from the file, or an env override pointing
//! under target/ at run time, is invisible to it. (b) `neutral_base()` being
//! outside target/ says nothing about OTHER fixture roots (`tempfile::TempDir`
//! defaults, `HARNESS_SANDBOX_HOME`); it also does not prove that the chosen
//! location is off blastguard's safe roots (`/tmp`) — that is deliberately not
//! forbidden here, so the fix must keep that property by other means.
//! (c) Passing does not prove old fixtures already on disk are gone.

#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/neutral_base.rs"]
mod neutral_base;

use std::path::{Path, PathBuf};

/// `<workspace>/target`, canonical. The workspace target dir must exist: this
/// very test binary lives under it, so a failure to resolve it is a panic
/// (undetermined), never a vacuous "not under target".
fn workspace_target() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target")
        .canonicalize()
        .expect("workspace target dir must exist and canonicalize")
}

fn is_under(path: &Path, root: &Path) -> bool {
    path.starts_with(root)
}

/// Does `src` both place something under CARGO_TARGET_TMPDIR (directly or via
/// `neutral_base`) and run fixture git `init` / `worktree add`?
fn builds_git_fixture_under_target(src: &str) -> bool {
    let places = src.contains("CARGO_TARGET_TMPDIR") || src.contains("neutral_base");
    let compact: String = src.split_whitespace().collect();
    let runs_git = compact.contains("\"init\"")
        && (compact.contains("[\"init\"") || compact.contains("\"init\",\"-q\""))
        || compact.contains("git init")
        || compact.contains("\"worktree\",\"add\"")
        || compact.contains("\"worktree\",\n\"add\"");
    places && runs_git
}

/// CONTROL (passes now): the predicates used below are meaningful.
#[test]
fn control_predicates_discriminate() {
    let target = workspace_target();
    // The cargo-provided tmpdir really is under the workspace target dir, so
    // "under target" is the property that the observed leak violates.
    let tmpdir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .canonicalize()
        .unwrap();
    assert!(is_under(&tmpdir, &target), "{tmpdir:?} vs {target:?}");
    assert!(!is_under(Path::new("/var/folders/x/T/fx"), &target));
    assert!(!is_under(
        &std::env::temp_dir().canonicalize().unwrap(),
        &target
    ));

    // The scanner flags a synthetic offender and ignores benign files.
    let offender = "let b = neutral_base::neutral_base();\ngit(&repo, &[\"init\", \"-q\"]);";
    assert!(builds_git_fixture_under_target(offender));
    let offender2 =
        "Path::new(env!(\"CARGO_TARGET_TMPDIR\"));\nrun(&[\"worktree\", \"add\", \"x\"]);";
    assert!(builds_git_fixture_under_target(offender2));
    assert!(!builds_git_fixture_under_target(
        "Path::new(env!(\"CARGO_TARGET_TMPDIR\")); no vcs here"
    ));
    assert!(!builds_git_fixture_under_target(
        "git(&[\"init\", \"-q\"]); // tempfile only"
    ));

    // The scan has something to scan.
    let tests = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let n = std::fs::read_dir(&tests)
        .unwrap()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "rs"))
        .count();
    assert!(n > 20, "only {n} test files found under {tests:?}");
}

/// RED (observed failing): the shared fixture base must not resolve under
/// the workspace `target/`.
#[test]
#[ignore = "RED: neutral_base() resolves under <workspace>/target/tmp; fixture git repos there make `git worktree remove --force` Ask"]
fn neutral_base_not_under_workspace_target() {
    let base = neutral_base::neutral_base();
    // The lexical spelling may be an alias symlink: judge the real path.
    let real = base
        .canonicalize()
        .unwrap_or_else(|e| panic!("neutral_base {base:?} does not canonicalize: {e}"));
    let target = workspace_target();
    let cargo_tmp = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .canonicalize()
        .unwrap();
    assert!(
        !is_under(&real, &target) && !is_under(&real, &cargo_tmp),
        "neutral_base() lexical {base:?} resolves to {real:?}, under the workspace \
         target dir {target:?} (CARGO_TARGET_TMPDIR {cargo_tmp:?}); override env \
         BLASTGUARD_TEST_FIXTURE_DIR = {:?}",
        std::env::var_os("BLASTGUARD_TEST_FIXTURE_DIR")
    );
}

/// RED (observed failing): no integration test file builds a git repository
/// under CARGO_TARGET_TMPDIR (directly or through `neutral_base`).
#[test]
#[ignore = "RED: integration tests create git fixture repos under CARGO_TARGET_TMPDIR (see offender list)"]
fn git_fixture_files_not_under_target() {
    let tests = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let me = Path::new(file!())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let mut offenders: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(&tests).unwrap().flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|x| x != "rs") {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if name == me {
            continue;
        }
        let src =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {path:?}: {e}"));
        if builds_git_fixture_under_target(&src) {
            offenders.push(name);
        }
    }
    offenders.sort();
    assert!(
        offenders.is_empty(),
        "{} test file(s) create git repos under CARGO_TARGET_TMPDIR / neutral_base: {offenders:#?}",
        offenders.len()
    );
}
