//! Regression coverage for the real merge invocation path.
//!
//! The first version of the main-tree guard detected an in-progress
//! integration only from on-disk markers (`MERGE_HEAD`, `CHERRY_PICK_HEAD`,
//! `REVERT_HEAD`, `rebase-merge/`, `rebase-apply/`). Measured here and in a
//! scratch repo: at `pre-merge-commit` time — the hook a clean `git merge
//! --no-ff` actually fires — git has written **none** of them. `MERGE_HEAD`
//! only appears when a merge *stops* (a conflict), i.e. exactly when the merge
//! is NOT proceeding. So the exclusion that CLAUDE.md §8 requires ("integration
//! in the main tree is permitted") never fired on the path that matters, and
//! the gate blocked the merges the worktree workflow depends on.
//!
//! These tests drive the real path: a real temp repo, a real `--no-ff` merge,
//! the repository's own `.githooks/pre-merge-commit` copied verbatim (it is the
//! file under test — it must hand the invocation context to `pre-commit` across
//! its `exec`), and a `pre-commit` that runs the real guard binary.
//!
//! The peer session is injected, via stub `overwatch`/`backlog` executables on
//! a narrowed `PATH`. A second live Claude session cannot be spawned from a
//! test; everything else here is real.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

/// The repository's own hooks directory (this crate lives at crates/condukt).
fn repo_githooks() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/condukt has a repo root")
        .join(".githooks")
}

fn git_dir_on_path() -> PathBuf {
    let out = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .expect("which git");
    PathBuf::from(String::from_utf8_lossy(&out.stdout).trim().to_string())
        .parent()
        .expect("git has a parent dir")
        .to_path_buf()
}

struct Fixture {
    base: PathBuf,
    repo: PathBuf,
    stubs: PathBuf,
    home: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let pid = std::process::id();
        let mut base = std::env::temp_dir();
        base.push(format!("condukt-maintree-merge-{pid}-{tag}"));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let hooks = repo.join("hooks");
        let stubs = base.join("stubs");
        let home = base.join("home");
        for d in [&repo, &hooks, &stubs, &home] {
            std::fs::create_dir_all(d).unwrap();
        }

        let f = Fixture {
            base,
            repo,
            stubs,
            home,
        };

        f.git(&["init", "-q", "-b", "main"]);
        f.git(&["config", "user.email", "t@t.t"]);
        f.git(&["config", "user.name", "t"]);
        f.git(&["config", "core.hooksPath", hooks.to_str().unwrap()]);

        // The repository's own merge hook, verbatim: it is the file under test.
        let real = repo_githooks().join("pre-merge-commit");
        std::fs::copy(&real, hooks.join("pre-merge-commit")).expect("copy pre-merge-commit");
        // A pre-commit that runs only the guard. The real pre-commit also runs
        // six python scanners against $REPO/scripts, which do not exist in a
        // temp repo; that the real one invokes the guard is pinned separately
        // by `the_repository_pre_commit_invokes_the_guard`.
        write_exec(
            &hooks.join("pre-commit"),
            &format!("#!/bin/sh\nexec {} guard main-tree\n", shell_quote(bin())),
        );

        std::fs::write(f.repo.join("base.txt"), "base\n").unwrap();
        f.git(&["add", "base.txt"]);
        f.commit_no_hooks("base");
        f.git(&["checkout", "-q", "-b", "side"]);
        std::fs::write(f.repo.join("side.txt"), "side\n").unwrap();
        f.git(&["add", "side.txt"]);
        f.commit_no_hooks("side work");
        f.git(&["checkout", "-q", "main"]);
        std::fs::write(f.repo.join("main.txt"), "main\n").unwrap();
        f.git(&["add", "main.txt"]);
        f.commit_no_hooks("main work");

        f
    }

    /// git with the real environment (used for setup, where hooks are bypassed).
    fn git(&self, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(&self.repo)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Setup commits skip the hooks: the guard is what the tests measure, and a
    /// setup commit is not part of the measurement.
    fn commit_no_hooks(&self, msg: &str) {
        self.git(&["commit", "-q", "--no-verify", "-m", msg]);
    }

    fn stub(&self, name: &str, stdout: &str, code: i32) {
        write_exec(
            &self.stubs.join(name),
            &format!("#!/bin/sh\nprintf '%s\\n' '{stdout}'\nexit {code}\n"),
        );
    }

    /// One live peer session, as `overwatch status --json` renders it.
    fn stub_peer(&self) {
        self.stub(
            "overwatch",
            r#"{"sessions":[{"session_id":"peer-session","leases":[],"live_count":1}]}"#,
            0,
        );
        self.stub("backlog", "none", 0);
    }

    /// A git invocation that goes THROUGH the hooks, with the peer injected.
    fn gated_git(&self, args: &[&str], envs: &[(&str, &str)]) -> Output {
        let path = format!("{}:{}", self.stubs.display(), git_dir_on_path().display());
        let mut cmd = Command::new("git");
        cmd.args(args)
            .current_dir(&self.repo)
            .env("PATH", path)
            .env("HOME", &self.home)
            .env("CLAUDE_CODE_SESSION_ID", "this-session")
            .env_remove("CONDUKT_MAINTREE_OVERRIDE")
            .env_remove("CONDUKT_GIT_HOOK");
        for (k, v) in envs {
            cmd.env(k, v);
        }
        cmd.output().expect("git runs")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn write_exec(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn combined(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// THE regression: a clean `--no-ff` merge in the primary tree, with a peer
/// session live, must complete. §8 permits integration in the main tree, and a
/// gate that blocks it makes the whole worktree workflow undischargeable —
/// nothing a worktree produces could ever land.
#[test]
fn a_clean_no_ff_merge_completes_in_the_primary_tree_with_a_peer_live() {
    let f = Fixture::new("merge");
    f.stub_peer();

    let out = f.gated_git(&["merge", "--no-ff", "-m", "merge side", "side"], &[]);
    assert!(
        out.status.success(),
        "the merge must not be blocked:\n{}",
        combined(&out)
    );

    // It passed for the RIGHT reason — the integration exclusion, announced,
    // via the invocation context. Without this the test would also pass if the
    // gate had simply stopped detecting anything.
    let text = combined(&out);
    assert!(
        text.contains("main-tree-guard: allowed") && text.contains("pre-merge-commit"),
        "the merge must be excluded by the announced integration exclusion:\n{text}"
    );

    // It really was a merge commit: two parents.
    let parents = Command::new("git")
        .args(["log", "-1", "--format=%P"])
        .current_dir(&f.repo)
        .output()
        .unwrap();
    let parents = String::from_utf8_lossy(&parents.stdout);
    assert_eq!(
        parents.split_whitespace().count(),
        2,
        "expected a two-parent merge commit, got parents {parents:?}"
    );
}

/// macos-14 regression, pinned host-independently. A git hook's PATH is
/// whatever invoked git; on some hosts (observed: macos-14 CI) that directory
/// carries no coreutils. The merge hook resolved its own dir with
/// `$(dirname "$0")`; with `dirname` off PATH that collapsed to "" and exec'd
/// "/pre-commit", so the hook died 126 and BLOCKED the integration merge §8
/// permits. The integration test above could not catch it: on hosts where
/// git's dir happens to also carry coreutils (ubuntu-latest, this mac) `dirname`
/// resolved and it passed — a host-dependent oracle. This gives PATH a single
/// empty directory, so `dirname` is absent on EVERY host: the old hook fails
/// here everywhere, the builtin-only hook passes everywhere.
#[test]
fn the_merge_hook_reaches_pre_commit_with_no_coreutils_on_path() {
    let pid = std::process::id();
    let mut base = std::env::temp_dir();
    base.push(format!("condukt-maintree-hookdir-{pid}"));
    let _ = std::fs::remove_dir_all(&base);
    let hooks = base.join("hooks");
    let emptybin = base.join("emptybin");
    std::fs::create_dir_all(&hooks).unwrap();
    std::fs::create_dir_all(&emptybin).unwrap();

    // The real hook, verbatim — the file under test.
    std::fs::copy(
        repo_githooks().join("pre-merge-commit"),
        hooks.join("pre-merge-commit"),
    )
    .expect("copy pre-merge-commit");
    // A pre-commit that only proves it was reached (absolute exec, no ext cmd).
    write_exec(
        &hooks.join("pre-commit"),
        "#!/bin/sh\nprintf 'PRE_COMMIT_REACHED\\n'\nexit 0\n",
    );

    // PATH = one empty dir: no `dirname`, no coreutils, on any host.
    let out = Command::new("/bin/sh")
        .arg(hooks.join("pre-merge-commit"))
        .env("PATH", emptybin.display().to_string())
        .output()
        .expect("hook runs");
    let text = combined(&out);
    let _ = std::fs::remove_dir_all(&base);

    assert!(
        out.status.success() && text.contains("PRE_COMMIT_REACHED"),
        "the merge hook must reach pre-commit using only shell builtins, with no \
         coreutils on PATH:\n{text}"
    );
    assert!(
        !text.contains("command not found"),
        "the hook must not depend on an external command being on PATH:\n{text}"
    );
}

/// The other half: the gate must still block an ORDINARY commit in the primary
/// tree while a peer is live. A fix that lets the merge through by letting
/// everything through has detected nothing.
#[test]
fn an_ordinary_commit_in_the_primary_tree_still_blocks_with_a_peer_live() {
    let f = Fixture::new("ordinary");
    f.stub_peer();
    std::fs::write(f.repo.join("edited.txt"), "edit\n").unwrap();
    f.git(&["add", "edited.txt"]);

    let out = f.gated_git(&["commit", "-m", "an ordinary commit"], &[]);
    assert!(
        !out.status.success(),
        "an ordinary shared-index commit must still be refused:\n{}",
        combined(&out)
    );
    assert!(
        combined(&out).contains("BLOCKED"),
        "expected the guard's block message:\n{}",
        combined(&out)
    );
}

/// The invocation-context declaration must not be usable to wave through an
/// ordinary commit. Setting it by hand on a plain `git commit` leaves git's own
/// `GIT_REFLOG_ACTION` unset — measured: git sets it for a merge and not for a
/// commit — so the declaration is uncorroborated and does not exclude.
#[test]
fn forging_the_hook_declaration_does_not_unlock_an_ordinary_commit() {
    let f = Fixture::new("forge");
    f.stub_peer();
    std::fs::write(f.repo.join("edited.txt"), "edit\n").unwrap();
    f.git(&["add", "edited.txt"]);

    let out = f.gated_git(
        &["commit", "-m", "trying it on"],
        &[("CONDUKT_GIT_HOOK", "pre-merge-commit")],
    );
    assert!(
        !out.status.success(),
        "a forged invocation-context declaration must not unlock an ordinary commit:\n{}",
        combined(&out)
    );
    assert!(
        combined(&out).contains("BLOCKED"),
        "expected the guard's block message:\n{}",
        combined(&out)
    );
}

/// A merge that stops on a conflict and is completed by `git commit` runs
/// `pre-commit` with `MERGE_HEAD` on disk. That path must keep working through
/// the on-disk detector, which is why the fix adds to it rather than replaces it.
#[test]
fn completing_a_conflicted_merge_by_hand_is_excluded_via_the_on_disk_marker() {
    let f = Fixture::new("conflict");
    f.stub_peer();
    // Make the same path differ on both branches.
    f.git(&["checkout", "-q", "side"]);
    std::fs::write(f.repo.join("both.txt"), "side\n").unwrap();
    f.git(&["add", "both.txt"]);
    f.commit_no_hooks("side both");
    f.git(&["checkout", "-q", "main"]);
    std::fs::write(f.repo.join("both.txt"), "main\n").unwrap();
    f.git(&["add", "both.txt"]);
    f.commit_no_hooks("main both");

    let merge = f.gated_git(&["merge", "--no-ff", "-m", "merge side", "side"], &[]);
    assert!(
        !merge.status.success(),
        "this merge is supposed to conflict:\n{}",
        combined(&merge)
    );
    assert!(
        f.repo.join(".git").join("MERGE_HEAD").exists(),
        "a stopped merge leaves MERGE_HEAD on disk"
    );

    std::fs::write(f.repo.join("both.txt"), "resolved\n").unwrap();
    f.git(&["add", "both.txt"]);
    let out = f.gated_git(&["commit", "-m", "resolved"], &[]);
    assert!(
        out.status.success(),
        "completing a conflicted merge is integration, which §8 permits:\n{}",
        combined(&out)
    );
}

/// The temp fixture uses a slim `pre-commit`; this pins that the repository's
/// real one still routes through the guard, so the two cannot drift apart
/// silently.
#[test]
fn the_repository_pre_commit_invokes_the_guard() {
    let text =
        std::fs::read_to_string(repo_githooks().join("pre-commit")).expect("read pre-commit");
    assert!(
        text.contains("guard main-tree"),
        "the repository's pre-commit must run the main-tree guard"
    );
}
