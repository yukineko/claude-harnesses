// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Contract tests for `fde resolve [TARGET]` (target resolution and the
//! state key), written by an independent test author (CLAUDE.md §2(a)).
//!
//! Contract under test (`crates/fde/src/main.rs` + user rulings 2026-11-03):
//!   - `TARGET` omitted: the target is the git top-level containing the
//!     invocation cwd (walk up; never the bare cwd).
//!   - relative `TARGET`: resolved against the invocation cwd (not the project
//!     root), canonicalized, then must be inside a git work tree.
//!   - stdout JSON `{"root", "state_key"}`, exit 0; undeterminable => exit 2
//!     with a reason on stderr.
//!   - state is keyed by the canonical root: every spelling of the same repo
//!     (subdir, symlink) gives the same key; different repos differ.
//!   - monorepo: the scope is the whole git top-level, never a sub-project
//!     detected via Cargo.toml / package.json.
//!   - inherited `GIT_DIR` / `GIT_WORK_TREE` / `GIT_INDEX_FILE` do not change
//!     the answer.
//!
//! Every refusal test is paired with a positive control in the same test, so a
//! binary that always exits 2 fails it.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::{
    code, git, init_repo_at, parse_resolved, run_fde, run_fde_with_env, show, stderr, stdout,
    TempPlace, TempRepo,
};

/// Resolve with no TARGET from `cwd`.
fn resolve_here(cwd: &Path, state: &Path) -> (PathBuf, String) {
    parse_resolved(&run_fde(cwd, state, &["resolve"]))
}

/// Resolve `target` (as given, possibly relative) from `cwd`.
fn resolve_arg(cwd: &Path, state: &Path, target: &str) -> (PathBuf, String) {
    parse_resolved(&run_fde(cwd, state, &["resolve", target]))
}

/// Precondition: `dir` really is outside any git work tree (so the refusal
/// tests are not vacuous on a machine whose temp dir sits inside a repo).
fn assert_not_in_git(dir: &Path) {
    let mut c = Command::new("git");
    c.current_dir(dir)
        .args(["rev-parse", "--is-inside-work-tree"])
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE");
    let out = c.output().expect("spawn git");
    assert!(
        !out.status.success(),
        "fixture precondition: {} must not be inside a git work tree, git said: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stdout)
    );
}

/// A refusal: exit 2, a non-empty reason on stderr, and stdout never names
/// `forbidden_root` (in particular never the bare cwd) as a root.
fn assert_refused(out: &std::process::Output, forbidden_root: &Path, what: &str) {
    assert_eq!(
        code(out),
        Some(2),
        "{what}: must be refused with exit 2: {}",
        show(out)
    );
    assert!(
        !stderr(out).trim().is_empty(),
        "{what}: a refusal must carry a reason on stderr: {}",
        show(out)
    );
    let so = stdout(out);
    let forbidden = forbidden_root.to_str().expect("utf-8 path");
    assert!(
        !so.contains(forbidden),
        "{what}: stdout must never name {forbidden} as the root: {}",
        show(out)
    );
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(so.trim()) {
        assert!(
            v.get("root").is_none(),
            "{what}: a refused resolve must not print a root: {}",
            show(out)
        );
    }
}

// (1) no TARGET from root/a/b: root is the git top-level, not the cwd.
#[test]
fn no_arg_from_nested_subdir_resolves_to_git_toplevel_not_cwd() {
    let repo = TempRepo::new();
    let state = TempPlace::new("fde-state-");
    let deep = repo.subdir("a/b");

    let (root, _key) = resolve_here(&deep, &state.path);

    assert_eq!(
        root,
        repo.root,
        "no TARGET from {} must resolve to the git top-level",
        deep.display()
    );
    assert_ne!(root, deep, "the bare cwd must never be the answer");
}

// (2) relative TARGET resolves against the invocation cwd, not the project root.
//
// Layout (all under one container):
//   C/proj        project repo P (cwd's repo), with cwd C/proj/a
//   C/proj/x      a SEPARATE nested repo X  (cwd-relative `../x`  => X)
//   C/x           a SEPARATE sibling repo S (root-relative `../x` => S)
#[test]
fn relative_arg_resolves_against_invocation_cwd_not_project_root() {
    let container = TempPlace::new("fde-container-");
    let state = TempPlace::new("fde-state-");
    let proj = init_repo_at(&container.path.join("proj"));
    let a = proj.join("a");
    fs::create_dir_all(&a).unwrap();
    let nested_x = init_repo_at(&proj.join("x"));
    let sibling_x = init_repo_at(&container.path.join("x"));
    assert_ne!(
        nested_x, sibling_x,
        "fixture: the two interpretations must differ"
    );

    let (root, key) = resolve_arg(&a, &state.path, "../x");

    assert_eq!(
        root,
        nested_x,
        "`../x` from {} must resolve against the cwd (=> {}), not the project root (=> {})",
        a.display(),
        nested_x.display(),
        sibling_x.display()
    );
    // The key must be X's own key, i.e. the same one X gets when resolved directly.
    let (x_root, x_key) = resolve_here(&nested_x, &state.path);
    assert_eq!(x_root, nested_x);
    assert_eq!(key, x_key, "`../x` must share the state key of X itself");
    let (_, s_key) = resolve_here(&sibling_x, &state.path);
    assert_ne!(
        key, s_key,
        "`../x` must not get the sibling repo's state key"
    );
}

// (3) from root/a and root/b with no TARGET: the same state key as at root.
#[test]
fn subdirs_share_the_state_key_of_the_root() {
    let repo = TempRepo::new();
    let state = TempPlace::new("fde-state-");
    let a = repo.subdir("a");
    let b = repo.subdir("b");

    let (r0, k0) = resolve_here(&repo.root, &state.path);
    let (ra, ka) = resolve_here(&a, &state.path);
    let (rb, kb) = resolve_here(&b, &state.path);

    assert_eq!(r0, repo.root);
    assert_eq!(ra, repo.root);
    assert_eq!(rb, repo.root);
    assert_eq!(ka, k0, "root/a must have the root's state key");
    assert_eq!(kb, k0, "root/b must have the root's state key");
}

// (4) a non-git dir is refused, with no arg (cwd non-git) and with an arg at it.
// Positive control: a real repo resolves in the same test.
#[test]
fn non_git_dir_is_refused_with_and_without_arg() {
    let plain = TempPlace::new("fde-nongit-");
    let state = TempPlace::new("fde-state-");
    assert_not_in_git(&plain.path);
    let repo = TempRepo::new();

    // Positive control: the binary can resolve at all.
    let (root, _) = resolve_here(&repo.root, &state.path);
    assert_eq!(root, repo.root);

    let out = run_fde(&plain.path, &state.path, &["resolve"]);
    assert_refused(&out, &plain.path, "no TARGET with a non-git cwd");

    let plain_str = plain.path.to_str().expect("utf-8 path");
    let out = run_fde(&repo.root, &state.path, &["resolve", plain_str]);
    assert_refused(&out, &plain.path, "absolute TARGET at a non-git dir");
    assert!(
        !stdout(&out).contains(repo.root.to_str().unwrap()),
        "a refused TARGET must not fall back to the cwd's repo: {}",
        show(&out)
    );
}

// (5) inherited GIT_DIR / GIT_WORK_TREE / GIT_INDEX_FILE naming another repo
// do not change the answer.
#[test]
fn inherited_git_env_does_not_change_the_result() {
    let target = TempRepo::new();
    let other = TempRepo::new();
    let state = TempPlace::new("fde-state-");
    let sub = target.subdir("src/deep");

    let control = resolve_here(&sub, &state.path);
    assert_eq!(control.0, target.root);

    let other_git = other.root.join(".git");
    let other_index = other_git.join("index");
    let env: [(&str, &Path); 3] = [
        ("GIT_DIR", &other_git),
        ("GIT_WORK_TREE", &other.root),
        ("GIT_INDEX_FILE", &other_index),
    ];

    let polluted = parse_resolved(&run_fde_with_env(&sub, &state.path, &["resolve"], &env));
    assert_eq!(
        polluted,
        control,
        "inherited git env pointing at {} must not change the no-arg answer",
        other.root.display()
    );

    let sub_str = sub.to_str().unwrap();
    let polluted_arg = parse_resolved(&run_fde_with_env(
        &other.root,
        &state.path,
        &["resolve", sub_str],
        &env,
    ));
    assert_eq!(
        polluted_arg, control,
        "inherited git env must not change the answer for an explicit TARGET"
    );

    // Each variable alone, too.
    for (k, v) in env {
        let one = parse_resolved(&run_fde_with_env(
            &sub,
            &state.path,
            &["resolve"],
            &[(k, v)],
        ));
        assert_eq!(
            one, control,
            "inherited {k} alone must not change the answer"
        );
    }
}

// (6) a symlink to the repo (and to a subdir) resolves to the real root/key.
#[test]
fn symlinked_paths_resolve_to_the_real_root_and_key() {
    let repo = TempRepo::new();
    let state = TempPlace::new("fde-state-");
    let links = TempPlace::new("fde-links-");
    let sub = repo.subdir("pkg/inner");

    let real = resolve_here(&repo.root, &state.path);
    assert_eq!(real.0, repo.root);

    let link_root = links.path.join("repo-link");
    let link_sub = links.path.join("sub-link");
    std::os::unix::fs::symlink(&repo.root, &link_root).unwrap();
    std::os::unix::fs::symlink(&sub, &link_sub).unwrap();

    let via_root = resolve_arg(&links.path, &state.path, link_root.to_str().unwrap());
    assert_eq!(
        via_root, real,
        "a symlink to the repo must resolve to its real root/key"
    );

    let via_sub = resolve_arg(&links.path, &state.path, link_sub.to_str().unwrap());
    assert_eq!(
        via_sub, real,
        "a symlink to a subdir must resolve to the real root/key"
    );

    // Relative spellings through the symlinks, resolved against the cwd.
    let rel_root = resolve_arg(&links.path, &state.path, "repo-link");
    assert_eq!(
        rel_root, real,
        "relative `repo-link` must resolve to the real root/key"
    );
    let rel_inner = resolve_arg(&links.path, &state.path, "repo-link/pkg");
    assert_eq!(
        rel_inner, real,
        "relative `repo-link/pkg` must resolve to the real root/key"
    );

    // No TARGET with the process cwd entered through the symlink.
    let via_cwd = resolve_here(&link_sub, &state.path);
    assert_eq!(
        via_cwd, real,
        "cwd via a symlinked subdir must resolve to the real root/key"
    );
}

// (7) a TARGET outside any git work tree, or nonexistent, is refused.
// Positive control: the same cwd with a valid TARGET resolves.
#[test]
fn target_outside_git_or_nonexistent_is_refused() {
    let repo = TempRepo::new();
    let state = TempPlace::new("fde-state-");
    let plain = TempPlace::new("fde-nongit-");
    assert_not_in_git(&plain.path);
    let plain_sub = plain.path.join("deeper");
    fs::create_dir_all(&plain_sub).unwrap();

    // Positive control.
    let (root, _) = resolve_arg(&repo.root, &state.path, repo.root.to_str().unwrap());
    assert_eq!(root, repo.root);

    let out = run_fde(
        &repo.root,
        &state.path,
        &["resolve", plain_sub.to_str().unwrap()],
    );
    assert_refused(&out, &repo.root, "TARGET in a non-git dir");

    let missing_abs = plain.path.join("does-not-exist");
    let out = run_fde(
        &repo.root,
        &state.path,
        &["resolve", missing_abs.to_str().unwrap()],
    );
    assert_refused(&out, &repo.root, "nonexistent absolute TARGET");

    // Nonexistent path INSIDE the repo is still refused (must not fall back to
    // the enclosing repo by walking up a path that is not there).
    let out = run_fde(&repo.root, &state.path, &["resolve", "no/such/dir"]);
    assert_refused(
        &out,
        &repo.root,
        "nonexistent relative TARGET inside the repo",
    );

    // Relative TARGET escaping the repo into the non-git dir.
    let out = run_fde(&plain_sub, &state.path, &["resolve", ".."]);
    assert_refused(&out, &plain.path, "relative `..` into a non-git dir");
}

// (8) monorepo: a sub-project with its own Cargo.toml / package.json (no .git)
// resolves to the git top-level.
#[test]
fn monorepo_subproject_markers_do_not_narrow_the_scope() {
    let repo = TempRepo::new();
    let state = TempPlace::new("fde-state-");
    let foo = repo.subdir("crates/foo");
    fs::write(
        foo.join("Cargo.toml"),
        "[package]\nname = \"foo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(foo.join("package.json"), "{\"name\": \"foo\"}\n").unwrap();
    git(&repo.root, &["add", "crates/foo"]);
    git(&repo.root, &["commit", "-q", "-m", "add foo"]);
    assert!(!foo.join(".git").exists(), "fixture: foo has no own .git");

    let (k_root_root, k_root) = resolve_here(&repo.root, &state.path);
    assert_eq!(k_root_root, repo.root);

    let (r, k) = resolve_here(&foo, &state.path);
    assert_eq!(
        r, repo.root,
        "a sub-project cwd must resolve to the git top-level"
    );
    assert_ne!(r, foo);
    assert_eq!(
        k, k_root,
        "the sub-project must share the top-level state key"
    );

    let (r_arg, k_arg) = resolve_arg(&repo.root, &state.path, "crates/foo");
    assert_eq!(
        r_arg, repo.root,
        "TARGET `crates/foo` must resolve to the git top-level"
    );
    assert_eq!(k_arg, k_root);
}

// (9) two different repos get different state keys (each key is stable).
#[test]
fn different_repos_get_different_state_keys() {
    let one = TempRepo::new();
    let two = TempRepo::new();
    let state = TempPlace::new("fde-state-");

    let (r1, k1) = resolve_here(&one.root, &state.path);
    let (r2, k2) = resolve_here(&two.root, &state.path);
    assert_eq!(r1, one.root);
    assert_eq!(r2, two.root);
    assert_ne!(k1, k2, "distinct repos must have distinct state keys");

    let (_, k1_again) = resolve_here(&one.root, &state.path);
    assert_eq!(
        k1, k1_again,
        "the state key must be stable across invocations"
    );
}
