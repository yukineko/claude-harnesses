// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Behavioural coverage for step 6 of [`harness_core::projkey::main_worktree_root`]
//! — the CANDIDATE-VERIFICATION guard.
//!
//! # Why this file exists
//!
//! `main_worktree_root` follows a linked worktree's `.git` FILE to its admin
//! dir, reads `commondir`, and takes the common git dir's PARENT as the main
//! worktree root. That parent is only a *candidate*: a worktree OF A SUBMODULE
//! has a common dir of `<super>/.git/modules/<name>`, whose parent
//! `<super>/.git/modules` is not a worktree root at all. Returning it unverified
//! would be a fail-open — a bogus-but-`Known` identity reads downstream (e.g. as
//! condukt's claim-registry project root) as a successfully resolved project,
//! and CLAUDE.md §3 forbids exactly that: "cannot determine" must never be
//! written as a resolved answer.
//!
//! The mutation battery run when that guard landed reported it as SURVIVING —
//! deleting the whole `match std::fs::metadata(&candidate_git)` block broke no
//! test. A property nothing can kill is not a property, so these tests reach
//! each verification arm with a filesystem fixture and assert the VARIANT is
//! `Undetermined`. The reason tag is asserted too, but only as a secondary
//! locator: the property is the variant.
//!
//! Fixtures are pure filesystem (tempdir + hand-written `.git` file +
//! `commondir` file) for the failure arms — `main_worktree_root` runs no
//! subprocess, so no real git is needed to reach them. The happy path is pinned
//! against a REAL `git worktree add` layout so the success arm cannot drift away
//! from what git actually writes on disk.

use harness_core::projkey::main_worktree_root;
use harness_core::verdict::Determination;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

/// Fresh, per-pid, per-call temp base so concurrent test binaries (and repeated
/// runs) never share state. Removed first in case a previous run died mid-test.
fn tmp_base(tag: &str) -> PathBuf {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let base =
        std::env::temp_dir().join(format!("harness-core-mwr-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    base
}

fn write(path: &Path, contents: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

/// Asserts the VARIANT is `Undetermined` (the property) and returns its reason
/// so the caller can additionally pin the tag.
#[track_caller]
fn expect_undetermined(d: Determination<PathBuf>, what: &str) -> String {
    match d {
        Determination::Undetermined(why) => why.as_str().to_string(),
        Determination::Known(p) => panic!(
            "{what}: main_worktree_root must return Undetermined for a candidate it \
             cannot verify (CLAUDE.md §3: a candidate that was not verified is not a \
             Known answer), but it returned Known({})",
            p.display()
        ),
    }
}

fn run_git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

// ---------------------------------------------------------------------------
// The shape the guard was written for: a worktree OF A SUBMODULE.
// ---------------------------------------------------------------------------

/// `<super>/.git/modules/sub/worktrees/w1` is the admin dir; its `commondir`
/// (`../..`) names `<super>/.git/modules/sub`; that dir's PARENT is
/// `<super>/.git/modules`, which is NOT a worktree root — it has no `.git` of
/// its own. Without the step-6 guard this path is handed back as `Known`.
#[test]
fn submodule_worktree_candidate_is_undetermined_not_a_bogus_known() {
    let base = tmp_base("submodule-wt");
    let superproject = base.join("super");
    let admin = superproject.join(".git/modules/sub/worktrees/w1");
    std::fs::create_dir_all(&admin).unwrap();
    // `commondir` is resolved relative to the admin dir -> <super>/.git/modules/sub
    write(&admin.join("commondir"), "../..\n");
    // The submodule's own git dir must exist for the shape to be realistic.
    std::fs::create_dir_all(superproject.join(".git/modules/sub")).unwrap();

    let wt = base.join("sub-wt");
    std::fs::create_dir_all(&wt).unwrap();
    write(&wt.join(".git"), &format!("gitdir: {}\n", admin.display()));

    let why = expect_undetermined(
        main_worktree_root(&wt),
        "submodule worktree (candidate <super>/.git/modules)",
    );
    assert!(
        why.starts_with("main-worktree-root:"),
        "every undetermined reason from this function must be prefixed \
         `main-worktree-root:` so each arm stays greppable; got: {why}"
    );
    assert!(
        why.contains("candidate-has-no-git"),
        "expected the `candidate-has-no-git` arm (the parent of the common git \
         dir has no .git entry at all); got: {why}"
    );
}

// ---------------------------------------------------------------------------
// Candidate HAS a `.git` directory, but it is a different repo's.
// ---------------------------------------------------------------------------

/// The candidate carries a real `.git` DIRECTORY that is not the common git dir
/// named by `commondir`. It is therefore some other repository's main tree, and
/// claiming it as this worktree's identity would silently address a foreign
/// project's state.
#[test]
fn candidate_owning_a_different_git_dir_is_undetermined() {
    let base = tmp_base("git-mismatch");
    // The linked worktree's admin dir, whose commondir names a NON-`.git` dir.
    let admin = base.join("holder/.git/worktrees/w1");
    std::fs::create_dir_all(&admin).unwrap();
    let common = base.join("other/.git-real");
    std::fs::create_dir_all(&common).unwrap();
    write(&admin.join("commondir"), &format!("{}\n", common.display()));

    // candidate = <base>/other ; candidate/.git exists as a DIRECTORY but is
    // NOT <base>/other/.git-real.
    std::fs::create_dir_all(base.join("other/.git")).unwrap();

    let wt = base.join("wt");
    std::fs::create_dir_all(&wt).unwrap();
    write(&wt.join(".git"), &format!("gitdir: {}\n", admin.display()));

    let why = expect_undetermined(
        main_worktree_root(&wt),
        "candidate with a foreign .git directory",
    );
    assert!(
        why.contains("candidate-git-mismatch"),
        "expected the `candidate-git-mismatch` arm (candidate's .git directory \
         is not the common git dir); got: {why}"
    );
}

// ---------------------------------------------------------------------------
// Candidate's own `.git` is a gitfile pointing somewhere else.
// ---------------------------------------------------------------------------

/// A submodule's MAIN worktree has a `.git` FILE, so the guard accepts one — but
/// only when it designates a path INSIDE the common git dir. Here it points at
/// an unrelated git dir, so the candidate does not own `common_dir`.
#[test]
fn candidate_gitfile_pointing_elsewhere_is_undetermined() {
    let base = tmp_base("gitfile-mismatch");
    let admin = base.join("holder/.git/worktrees/w1");
    std::fs::create_dir_all(&admin).unwrap();
    let common = base.join("other/.git-real");
    std::fs::create_dir_all(&common).unwrap();
    write(&admin.join("commondir"), &format!("{}\n", common.display()));

    // candidate = <base>/other ; its .git is a FILE designating a git dir that
    // is not inside <base>/other/.git-real.
    write(
        &base.join("other/.git"),
        &format!("gitdir: {}\n", base.join("elsewhere/.git").display()),
    );

    let wt = base.join("wt");
    std::fs::create_dir_all(&wt).unwrap();
    write(&wt.join(".git"), &format!("gitdir: {}\n", admin.display()));

    let why = expect_undetermined(
        main_worktree_root(&wt),
        "candidate gitfile designating a foreign git dir",
    );
    assert!(
        why.contains("candidate-gitfile-mismatch"),
        "expected the `candidate-gitfile-mismatch` arm; got: {why}"
    );
}

// ---------------------------------------------------------------------------
// Happy path, pinned against a REAL `git worktree add` layout.
// ---------------------------------------------------------------------------

/// Anti-vacuity control for the three assertions above: if the verification
/// guard rejected everything, they would all pass while the function was
/// useless. A real linked worktree created by real git must resolve to the main
/// tree — `Known`, and equal to the main tree's canonical path.
#[test]
fn real_linked_worktree_resolves_to_the_main_tree() {
    let base = tmp_base("real-git");
    let main_tree = base.join("main-tree");
    let wt = base.join("wt1");
    std::fs::create_dir_all(&main_tree).unwrap();

    run_git(&main_tree, &["init", "-q"]);
    run_git(&main_tree, &["config", "user.email", "t@t.t"]);
    run_git(&main_tree, &["config", "user.name", "t"]);
    std::fs::write(main_tree.join("README.md"), "seed\n").unwrap();
    run_git(&main_tree, &["add", "README.md"]);
    run_git(&main_tree, &["commit", "-q", "-m", "seed"]);
    run_git(
        &main_tree,
        &["worktree", "add", "-q", "-b", "wt1", wt.to_str().unwrap()],
    );

    // Fixture invariant: a linked worktree's `.git` is a FILE.
    assert!(
        wt.join(".git").is_file(),
        "fixture invariant broken: {} must be a gitdir FILE",
        wt.join(".git").display()
    );

    match main_worktree_root(&wt) {
        Determination::Known(got) => assert_eq!(
            got.canonicalize().unwrap(),
            main_tree.canonicalize().unwrap(),
            "a real linked worktree must resolve to its main tree"
        ),
        Determination::Undetermined(why) => panic!(
            "a REAL `git worktree add` layout must resolve to Known(main tree); \
             got Undetermined: {}",
            why.as_str()
        ),
    }

    // And the main tree itself is its own main worktree root.
    match main_worktree_root(&main_tree) {
        Determination::Known(got) => assert_eq!(
            got.canonicalize().unwrap(),
            main_tree.canonicalize().unwrap()
        ),
        Determination::Undetermined(why) => panic!(
            "the main tree must resolve to itself; got Undetermined: {}",
            why.as_str()
        ),
    }
}
