// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (observed 2026-11-16 .. 2026-11-17, run run-20261116-164820-25374,
//! condukt 0.7.435, worktree_base /mnt/c/tmp/aegis-worktrees on WSL drvfs;
//! repo rev 57cff59f1, i.e. AFTER the backlog c96e756a fix).
//!
//! Observation (verbatim from the report):
//!  1. After t1 (verified and merged) was torn down, the worktree directory
//!     held only `target/` and `tdd.toml`; the `.git` file was gone. A later
//!     `condukt worktree remove --path <dir>` refused with
//!     `refusing to remove worktree ...: cannot determine whether the task that
//!     owns it is verified (its checked-out branch: git symbolic-ref HEAD
//!     exited 128)` and left the directory. `condukt worktree cleanup` printed
//!     `kept ... reason: ... has no resolvable .git — it cannot be attributed
//!     to any repository, which is NOT the same as being condukt debris`.
//!  2. `condukt worktree discard --run R --task t2` printed
//!     `refusing to remove worktree ...: git declined to remove it, its content
//!     could not be preserved (cannot build a content tree for preservation:
//!     seeding the temp index from HEAD), and whether it holds uncommitted work
//!     could not be determined (... is not the root of a usable git worktree
//!     ...). The directory is left in place.` After the discard the `.git` file
//!     was gone, some tracked files (specs/*.toml, tdd.toml; content identical
//!     to the branch commit) and the ignored `target/` (3.0G) remained; the
//!     branch was not deleted and the task stayed `done`. After a human `rm`'d
//!     the directory, re-running discard succeeded.
//!  3. backlog c96e756a (done on main) is "when `git worktree remove --force`
//!     fails part-way, quarantine-move the leftover directory (delete
//!     nothing)". Neither of the two cases above was quarantined.
//!
//! Two readings (this test does not choose between them):
//!  (a) binary defect: a leftover directory from a git worktree removal that
//!      stopped part-way is neither quarantined nor deleted by condukt; it
//!      demands a human `rm`. Reading of the code: the quarantine
//!      (src/worktree/quarantine.rs `fail_forced_removal`) is only reached from
//!      the `--force` retry in `remove_worktree_preserving`; the refusal arm
//!      "git declined ... could not be determined" (Determination::Undetermined)
//!      `bail!`s before any quarantine, and the teardown gate refuses before
//!      `worktree remove` runs at all when `.git` is unresolvable.
//!  (b) intended: a directory with no `.git` cannot be attributed to any
//!      repository, so keeping it is fail-closed. Distinguish: a quarantine
//!      MOVE deletes nothing, so it is compatible with fail-closed; the user's
//!      ruling on c96e756a (Q7, 2026-11-14) is "quarantine-move the leftover,
//!      delete nothing". If that ruling covers a leftover whose `.git` is gone
//!      regardless of which git step failed, (a); if it is limited to the
//!      `--force` step, (b) for the plain-remove-failed shape, and the ruling
//!      needs widening.
//!
//! Reproduced here without drvfs: `worktree create` + `state set --worktree`,
//! then the worktree's `.git` file is deleted by hand together with some
//! tracked files (a part-way removal), leaving other tracked files and an
//! ignored `target/`. `worktree discard` is then run. Asserted: the directory
//! is no longer at its original path (quarantine-moved, or the task discarded).
//! A CONTROL in the same file: discard of an intact worktree succeeds.
//!
//! DOES NOT PROVE: that drvfs produces this shape (the shape is hand-made);
//! the `worktree remove --path` / `worktree cleanup` variants of observation 1
//! (only `discard`, observation 2, is exercised); which reading above is
//! right; where the quarantine must live (the assertion accepts any location
//! as long as the original path is vacated and the content is not deleted
//! outright: it checks the marker file survives somewhere under the tmp tree).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const RUN: &str = "run-partial";

fn git(dir: &Path, args: &[&str]) -> String {
    let out = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

struct Fx {
    tmp: tempfile::TempDir,
    repo: PathBuf,
    home: PathBuf,
    wt_base: PathBuf,
}

impl Fx {
    fn new() -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let repo = base.join("repo");
        let home = base.join("home");
        let wt_base = base.join("wtbase");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&wt_base).unwrap();
        std::fs::create_dir_all(home.join(".claude").join("projects")).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "t@t.t"]);
        git(&repo, &["config", "user.name", "t"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join("base.txt"), "base\n").unwrap();
        std::fs::write(repo.join(".gitignore"), "target/\n").unwrap();
        git(&repo, &["add", "base.txt", ".gitignore"]);
        git(&repo, &["commit", "-q", "-m", "seed"]);
        let fx = Fx {
            tmp,
            repo,
            home,
            wt_base,
        };
        let dec = base.join("dec.json");
        std::fs::write(
            &dec,
            r#"{"goal":"g","tasks":[
              {"id":"t2","title":"a","kind":"chore","touched_files":["specs/a.toml"],"deps":[],"class":"parallel","done_criteria":"d"}]}"#,
        )
        .unwrap();
        let out = fx.condukt(&[
            "state",
            "init",
            "--run",
            RUN,
            "--file",
            dec.to_str().unwrap(),
        ]);
        assert!(out.status.success(), "state init: {out:?}");
        fx
    }

    fn condukt(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_condukt"));
        harness_core::git_env::scrub(&mut cmd);
        cmd.args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("CLAUDE_CODE_SESSION_ID", "sess-partial-remove")
            .env("CONDUKT_DEFAULT_BRANCH", "main")
            .env("CONDUKT_WORKTREE_BASE", &self.wt_base)
            .env("HARNESS_PROGRESS_WINDOW_SECS", "0")
            .env_remove("CONDUKT_DISABLE")
            .output()
            .expect("spawn condukt")
    }

    /// `worktree create` for t2, with tracked `specs/a.toml` + `tdd.toml`
    /// committed on the branch, an ignored `target/` and registered in run
    /// state as the task's worktree (status done).
    fn worktree(&self) -> PathBuf {
        let out = self.condukt(&[
            "worktree", "create", "--run", RUN, "--topic", "t2", "--branch", "b-t2",
        ]);
        assert!(out.status.success(), "worktree create: {out:?}");
        let wt = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
        std::fs::create_dir_all(wt.join("specs")).unwrap();
        std::fs::write(wt.join("specs").join("a.toml"), "a = 1\n").unwrap();
        std::fs::write(wt.join("tdd.toml"), "x = 1\n").unwrap();
        git(&wt, &["add", "specs/a.toml", "tdd.toml"]);
        git(&wt, &["commit", "-q", "-m", "task work"]);
        std::fs::create_dir_all(wt.join("target").join("debug")).unwrap();
        std::fs::write(wt.join("target").join("debug").join("artifact"), "bin\n").unwrap();
        // condukt namespaces the branch it creates; ask git for the real name.
        let branch = git(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]);
        let out = self.condukt(&[
            "state",
            "set",
            "--run",
            RUN,
            "--task",
            "t2",
            "--status",
            "done",
            "--worktree",
            wt.to_str().unwrap(),
            "--branch",
            &branch,
        ]);
        assert!(out.status.success(), "state set worktree: {out:?}");
        wt
    }

    fn discard(&self) -> Output {
        self.condukt(&["worktree", "discard", "--run", RUN, "--task", "t2"])
    }
}

/// Every file named `artifact` anywhere under the tmp tree (the ignored
/// `target/` residue), so "moved" can be told from "deleted".
fn find_marker(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                stack.push(p);
            } else if p.file_name().is_some_and(|n| n == "artifact") {
                found.push(p);
            }
        }
    }
    found
}

/// CONTROL: an intact worktree is discarded successfully.
#[test]
fn control_discard_of_intact_worktree_succeeds() {
    let fx = Fx::new();
    let wt = fx.worktree();
    let out = fx.discard();
    assert!(out.status.success(), "discard of intact worktree: {out:?}");
    assert!(!wt.exists(), "intact worktree should be gone");
}

// test-weakening-justified: new RED for an unfixed defect (2026-11-17).
#[test]
fn discard_does_not_leave_part_way_removed_dir_in_place() {
    let fx = Fx::new();
    let wt = fx.worktree();
    // Part-way removal: `.git` file and one tracked file are gone; other
    // tracked files (specs/a.toml, tdd.toml) and ignored target/ remain.
    std::fs::remove_file(wt.join(".git")).unwrap();
    std::fs::remove_file(wt.join("base.txt")).unwrap();
    assert!(wt.join("specs").join("a.toml").exists());
    assert!(wt.join("target").exists());

    let out = fx.discard();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let markers = find_marker(fx.tmp.path());
    assert!(
        !wt.exists(),
        "part-way-removed worktree dir {} was left in place at its original path \
         (neither quarantine-moved nor discarded). exit={:?}\nstdout: {stdout}\nstderr: {stderr}",
        wt.display(),
        out.status.code()
    );
    // Nothing may be deleted outright: the ignored residue must survive
    // somewhere (the quarantine), since a move deletes nothing.
    assert!(
        !markers.is_empty(),
        "the dir vanished but its target/ residue is nowhere under the tmp tree: deleted, not moved"
    );
}
