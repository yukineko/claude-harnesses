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
//!   * Several test files join onto `CARGO_TARGET_TMPDIR` directly and create
//!     git fixtures (listed in the failure message). `neutral_base` users are
//!     covered by the dynamic test 1, not by the text scan.
//!
//! Both RED tests assert decisions about observed paths / file contents. They
//! were `#[ignore]`d until the fix landed (backlog 37955e32) and now run.
//!
//! DOES NOT PROVE: (0) the scan is file-level: a file that mentions
//! directly joins `CARGO_TARGET_TMPDIR` AND runs git `init` is listed even if
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

/// Does `src` join a path DIRECTLY onto `CARGO_TARGET_TMPDIR` (i.e. under the
/// workspace `target/`) AND create a git repository / worktree?
///
/// Deliberately NOT keyed on `neutral_base`: a file that merely calls
/// `neutral_base()` is judged by its resolved path, which the dynamic test
/// `neutral_base_not_under_workspace_target` covers.
fn builds_git_fixture_under_target(src: &str) -> bool {
    let compact: String = src.split_whitespace().collect();
    // `...CARGO_TARGET_TMPDIR")` followed shortly by `.join(`: a direct join.
    let needle = "CARGO_TARGET_TMPDIR\"";
    let joins_directly = compact.match_indices(needle).any(|(i, _)| {
        let tail = &compact[i + needle.len()..];
        let window: String = tail.chars().take(8).collect();
        window.contains(".join(")
    });
    let runs_git = compact.contains("[\"init\"")
        || compact.contains("\"init\",\"-")
        || compact.contains("gitinit")
        || compact.contains("git init")
        || compact.contains("\"worktree\",\"add\"")
        || compact.contains("[\"clone\"")
        || compact.contains("\"clone\",\"");
    joins_directly && runs_git
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

    // The scanner flags a direct CARGO_TARGET_TMPDIR join + git init ...
    let offender = "let b = Path::new(env!(\"CARGO_TARGET_TMPDIR\")).join(\"x\");\ngit(&b, &[\"init\", \"-q\"]);";
    assert!(builds_git_fixture_under_target(offender));
    let offender2 = "let b = PathBuf::from(env!(\"CARGO_TARGET_TMPDIR\"))\n    .join(\"x\");\nrun(&[\"worktree\", \"add\", \"x\"]);";
    assert!(builds_git_fixture_under_target(offender2));
    // ... but NOT a neutral_base-only sample (covered dynamically by test 1),
    assert!(!builds_git_fixture_under_target(
        "let b = neutral_base::neutral_base();\ngit(&b, &[\"init\", \"-q\"]);"
    ));
    // NOT CARGO_TARGET_TMPDIR without git,
    assert!(!builds_git_fixture_under_target(
        "Path::new(env!(\"CARGO_TARGET_TMPDIR\")).join(\"x\"); no vcs here"
    ));
    // and NOT git without a direct target join.
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

/// Was RED (observed failing at 20baa9f2); GREEN since the 37955e32 fix: the
/// shared fixture base must not resolve under the workspace `target/`.
#[test]
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

/// Was RED (observed failing at 8ad4d3f7); GREEN since the 37955e32 fix: no
/// integration test file joins a path directly onto
/// `CARGO_TARGET_TMPDIR` and creates a git repository / worktree there.
///
/// Usage of `neutral_base` is NOT flagged here: its resolved location is
/// covered dynamically by test 1, `neutral_base_not_under_workspace_target`.
///
/// DOES NOT PROVE: this is a static text scan. It cannot see a git repo built
/// through a helper that hides the git args from the file, a `CARGO_TARGET_TMPDIR`
/// path built indirectly (stored in a variable/const then joined, or via
/// another env var), or where a repo from a `tempfile` dir actually lives; a
/// match means "joins CARGO_TARGET_TMPDIR and runs git init somewhere in the
/// file", an upper bound, not a per-repo path observation.
#[test]
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
        "{} test file(s) join onto CARGO_TARGET_TMPDIR directly and create git repos: {offenders:#?}",
        offenders.len()
    );
}
