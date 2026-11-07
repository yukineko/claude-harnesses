// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repros for operator rules that today live only in LLM memory
//! (memory audit 2026-11-07, class B; user ruling 2026-11-07: "実装で補えるものは
//! 実装にして。LLMの推論での防御はすべてあてにならない").
//!
//! * item 1 — memory `worktree-always-merge-then-delete`: deleting a branch
//!   whose commits are NOT reachable from the default branch, or force-removing
//!   a DIRTY linked worktree, destroys unmerged work and must be Ask.
//! * item 3 — memory `condukt-worker-dirty-tree-commit`: a whole-tree stage
//!   (`git add -A` / `--all` / `.`, `git commit -a`) sweeps in foreign WIP from
//!   other sessions and must be Ask; staging explicit paths stays Allow.
//! * item 16 — memory `scratch-artifacts-in-deletable-places`: creating
//!   scratch trees (`git clone`, `mkdir -p`, `cp -r`) OUTSIDE the managed
//!   places (session scratchpad / temp roots, `.harness-worktrees`, the
//!   project) must be Ask; inside them stays Allow.
//!
//! Each test drives the REAL hook binary against a REAL fixture repo, and
//! pins controls so that a blanket Ask cannot pass it.

#[path = "support/git_env.rs"]
mod git_env;
#[path = "support/neutral_base.rs"]
mod neutral_base;

#[path = "support/bounded.rs"]
mod bounded;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn git(dir: &Path, args: &[&str]) {
    let mut cmd = git_env::pinned(dir);
    cmd.args([
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@example.invalid",
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "init.defaultBranch=main",
    ])
    .args(args)
    .env("GIT_CONFIG_GLOBAL", "/dev/null")
    .env("GIT_CONFIG_NOSYSTEM", "1");
    let out =
        bounded::run_bounded_stdin(&mut cmd, bounded::ChildStdin::Null, bounded::TOOL_DEADLINE);
    assert!(
        out.status.success(),
        "git {args:?} failed in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn write(p: &Path, content: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, content).unwrap();
}

struct Fixture {
    base: PathBuf,
    home: PathBuf,
    /// Primary working tree, on `main`; also CLAUDE_PROJECT_DIR.
    repo: PathBuf,
    /// `<base>/src/.harness-worktrees/dirty` — linked worktree, dirty.
    dirty_wt: PathBuf,
    /// `<base>/src/.harness-worktrees/clean` — linked worktree, clean.
    clean_wt: PathBuf,
}

impl Fixture {
    /// Repo on `main` (2 commits) with branches:
    ///   * `merged`   — points at main~1 (fully merged);
    ///   * `unmerged` — one commit NOT reachable from main;
    ///   * `dirty`, `clean` — checked out in linked worktrees.
    fn new(name: &str) -> Fixture {
        let base = neutral_base::neutral_base()
            .join("audit_b_memory_rules_20261107")
            .join(name);
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        let repo = base.join("src/repo");
        let hw = base.join("src/.harness-worktrees");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&hw).unwrap();

        git(&repo, &["init", "-q"]);
        write(&repo.join("tracked.txt"), "one\n");
        git(&repo, &["add", "tracked.txt"]);
        git(&repo, &["commit", "-q", "-m", "one"]);
        git(&repo, &["branch", "merged"]);
        write(&repo.join("tracked.txt"), "two\n");
        git(&repo, &["commit", "-q", "-am", "two"]);

        git(&repo, &["switch", "-q", "-c", "unmerged"]);
        write(&repo.join("unique.txt"), "unique work\n");
        git(&repo, &["add", "unique.txt"]);
        git(&repo, &["commit", "-q", "-m", "unique work"]);
        git(&repo, &["switch", "-q", "main"]);

        let dirty_wt = hw.join("dirty");
        let clean_wt = hw.join("clean");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "dirty",
                dirty_wt.to_str().unwrap(),
            ],
        );
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "clean",
                clean_wt.to_str().unwrap(),
            ],
        );
        write(&dirty_wt.join("tracked.txt"), "uncommitted edit\n");
        write(&dirty_wt.join("new.txt"), "untracked work\n");

        // Primary tree: a dirty tracked file and an untracked file (the
        // "foreign WIP" a whole-tree stage would sweep in).
        write(&repo.join("tracked.txt"), "foreign wip\n");
        write(&repo.join("foreign.txt"), "foreign untracked\n");

        Fixture {
            base,
            home,
            repo,
            dirty_wt,
            clean_wt,
        }
    }

    /// Hook stdout for a Bash `command` (cwd = CLAUDE_PROJECT_DIR = primary).
    fn bash(&self, command: &str) -> String {
        self.bash_cwd(command, &self.repo)
    }

    /// Like [`Fixture::bash`] but the session `cwd` in the hook payload is
    /// `cwd` (the hook PROCESS cwd stays `<base>`, never a repo).
    fn bash_cwd(&self, command: &str, cwd: &Path) -> String {
        let payload = serde_json::json!({
            "session_id": "audit-b-memory-rules",
            "hook_event_name": "PreToolUse",
            "cwd": cwd.display().to_string(),
            "tool_name": "Bash",
            "tool_input": { "command": command },
        })
        .to_string();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
        git_env::scrub(&mut cmd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("CLAUDE_PROJECT_DIR", &self.repo)
            .env("BLASTGUARD_APPROVALS_DIR", self.base.join("store"))
            .env("HOME", &self.home)
            .env("CLAUDE_CODE_ENTRYPOINT", "cli")
            .env("CLAUDECODE", "1")
            .env_remove("BLASTGUARD_ASK")
            .env_remove("TMPDIR")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .current_dir(&self.base);
        let out = bounded::run_bounded(&mut cmd, payload.as_bytes(), bounded::HOOK_DEADLINE);
        assert_eq!(
            out.status.code(),
            Some(0),
            "hook must exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

/// `"allow"` (empty stdout), `"deny"`, `"ask"`, or `"other"`.
fn verdict(out: &str) -> &'static str {
    if out.trim().is_empty() {
        "allow"
    } else if out.contains(r#""permissionDecision":"deny""#) {
        "deny"
    } else if out.contains(r#""permissionDecision":"ask""#) {
        "ask"
    } else {
        "other"
    }
}

/// Like [`check_all`] but each case carries its own session cwd.
#[track_caller]
fn check_all_in(f: &Fixture, cases: &[(String, PathBuf, &str)]) {
    let mut bad = Vec::new();
    for (cmd, cwd, want) in cases {
        let out = f.bash_cwd(cmd, cwd);
        let got = verdict(&out);
        if got != *want {
            bad.push(format!(
                "  `{cmd}` (cwd {}): want {want}, got {got} ({})",
                cwd.display(),
                out.trim()
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "{} of {} cases have the wrong verdict:\n{}",
        bad.len(),
        cases.len(),
        bad.join("\n")
    );
}

/// A standalone repo at `dir` whose first branch is `default`, with two
/// commits (`c1`, then `c2` on top).
fn seed_repo(dir: &Path, default: &str) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q", "-b", default]);
    write(&dir.join("f.txt"), "one\n");
    git(dir, &["add", "f.txt"]);
    git(dir, &["commit", "-q", "-m", "c1"]);
    write(&dir.join("f.txt"), "two\n");
    git(dir, &["commit", "-q", "-am", "c2"]);
}

/// Branch `name` with one commit that is not reachable from `from`; leaves
/// `from` checked out again.
fn add_unmerged_branch(dir: &Path, name: &str, from: &str) {
    git(dir, &["switch", "-q", "-c", name]);
    write(&dir.join(format!("{name}.txt")), "unique\n");
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", name]);
    git(dir, &["switch", "-q", from]);
}

/// Make `refs/remotes/origin/HEAD` resolve to `default` (no network).
fn set_origin_head(dir: &Path, default: &str) {
    let url = dir.display().to_string();
    git(dir, &["remote", "add", "origin", &url]);
    git(
        dir,
        &[
            "update-ref",
            &format!("refs/remotes/origin/{default}"),
            default,
        ],
    );
    git(
        dir,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            &format!("refs/remotes/origin/{default}"),
        ],
    );
}

/// Run every `(command, wanted)` case and fail once, listing every mismatch,
/// so the failure names exactly which shapes are wrong.
#[track_caller]
fn check_all(f: &Fixture, cases: &[(String, &str)]) {
    let mut bad = Vec::new();
    for (cmd, want) in cases {
        let out = f.bash(cmd);
        let got = verdict(&out);
        let ok = match *want {
            // "not allow": Ask or Deny both refuse to wave it through.
            "not-allow" => got != "allow",
            w => got == w,
        };
        if !ok {
            bad.push(format!(
                "  `{cmd}`: want {want}, got {got} ({})",
                out.trim()
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "{} of {} cases have the wrong verdict:\n{}",
        bad.len(),
        cases.len(),
        bad.join("\n")
    );
}

/// Item 1 (memory `worktree-always-merge-then-delete`).
#[test]
fn deleting_unmerged_branch_or_dirty_worktree_is_ask() {
    let f = Fixture::new("branch_delete");
    // Preconditions the verdict must depend on.
    let unique = f.repo.join("unique.txt");
    assert!(
        !unique.exists(),
        "unique.txt only exists on branch unmerged"
    );
    let r = f.repo.display().to_string();
    let dirty = f.dirty_wt.display().to_string();
    let clean = f.clean_wt.display().to_string();
    let cases = vec![
        // Destroys the only ref to an unmerged commit.
        (format!("git -C {r} branch -D unmerged"), "ask"),
        (
            format!("git -C {r} branch --delete --force unmerged"),
            "ask",
        ),
        // Discards uncommitted + untracked work in a linked worktree.
        (format!("git -C {r} worktree remove --force {dirty}"), "ask"),
        // Controls: nothing is lost.
        (format!("git -C {r} branch -d merged"), "allow"),
        (format!("git -C {r} branch -D merged"), "allow"),
        (format!("git -C {r} worktree remove {clean}"), "allow"),
    ];
    check_all(&f, &cases);
}

/// Item 3 (memory `condukt-worker-dirty-tree-commit`).
#[test]
fn whole_tree_stage_or_commit_all_is_ask() {
    let f = Fixture::new("stage_all");
    let r = f.repo.display().to_string();
    let cases = vec![
        (format!("git -C {r} add -A"), "ask"),
        (format!("git -C {r} add --all"), "ask"),
        (format!("git -C {r} add ."), "ask"),
        (format!("git -C {r} commit -a -m wip"), "ask"),
        (format!("git -C {r} commit -am wip"), "ask"),
        (format!("git -C {r} commit --all -m wip"), "ask"),
        // Controls: explicit paths only.
        (format!("git -C {r} add tracked.txt"), "allow"),
        (format!("git -C {r} commit -m msg"), "allow"),
        (format!("git -C {r} commit -m msg -- tracked.txt"), "allow"),
    ];
    check_all(&f, &cases);
}

/// Item 16 (memory `scratch-artifacts-in-deletable-places`).
#[test]
fn scratch_tree_outside_managed_places_is_ask() {
    let f = Fixture::new("scratch_place");
    let r = f.repo.display().to_string();
    let h = f.home.display().to_string();
    let hw = f.base.join("src/.harness-worktrees").display().to_string();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let tmp = format!(
        "/private/tmp/blastguard-audit-b-scratch-{}-{nanos}",
        std::process::id()
    );
    let cases = vec![
        // Unmanaged: a bare directory under $HOME.
        (format!("git clone {r} {h}/scratch-clone"), "ask"),
        // User ruling 2026-11-07 (binding, overrides the original RED):
        // `mkdir -p` is NOT in the Ask set — only git clone / cp -r outside
        // managed places. Pinned as an Allow control so the ruling holds.
        (format!("mkdir -p {h}/scratch/a/b"), "allow"),
        (format!("cp -r {r} {h}/scratch-copy"), "ask"),
        // Controls: managed places.
        (format!("git clone {r} {tmp}/clone"), "allow"),
        (format!("git clone {r} {hw}/clone"), "allow"),
        (format!("mkdir -p {r}/build/a"), "allow"),
        (format!("mkdir -p {tmp}/a/b"), "allow"),
        (format!("cp -r {r}/tracked.txt {tmp}/copy.txt"), "allow"),
    ];
    check_all(&f, &cases);
}

/// Item 16 rework: independent verification of 9f27dd1f observed
/// `cp -rt <home dir> a`, `cp -Rt ...`, `cp -at ...` as ALLOW: a `t` inside a
/// short-option cluster takes the next word as the target directory, but the
/// parser read it as a source and judged the last operand (a project-relative
/// `a`) as the landing. The landing is the `-t` directory, outside managed
/// places, so these must be Ask like the spelled-out `-r -t DIR`.
#[test]
fn scratch_cp_clustered_target_dir_is_ask() {
    let f = Fixture::new("scratch_cp_cluster_t");
    let r = f.repo.display().to_string();
    let h = f.home.display().to_string();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let tmp = format!(
        "/private/tmp/blastguard-audit-b-cluster-{}-{nanos}",
        std::process::id()
    );
    let cases = vec![
        (format!("cp -rt {h}/scratch-copy {r}"), "ask"),
        (format!("cp -Rt {h}/scratch-copy {r}"), "ask"),
        (format!("cp -at {h}/scratch-copy {r}"), "ask"),
        (format!("cp -r -t {h}/scratch-copy {r}"), "ask"),
        // Controls: same clusters into a managed place.
        (format!("cp -rt {tmp} {r}"), "allow"),
        (format!("cp -Rt {tmp} {r}"), "allow"),
    ];
    check_all(&f, &cases);
}

// ---------------------------------------------------------------------------
// t1b: gaps found by independent verification of commit 5c251e4b.
// ---------------------------------------------------------------------------

/// Finding 1: `-d` combined with a force flag (any spelling/order) deletes
/// like `-D`.
#[test]
fn t1b_f1_delete_plus_force_spellings_are_ask() {
    let f = Fixture::new("t1b_f1");
    let cases = vec![
        ("git branch -d -f unmerged".to_string(), "ask"),
        ("git branch -df unmerged".to_string(), "ask"),
        ("git branch -fd unmerged".to_string(), "ask"),
        ("git branch --delete -f unmerged".to_string(), "ask"),
        ("git branch -d --force unmerged".to_string(), "ask"),
        ("git branch --force --delete unmerged".to_string(), "ask"),
        // Controls: same spellings on a merged branch lose nothing.
        ("git branch -d -f merged".to_string(), "allow"),
        ("git branch -df merged".to_string(), "allow"),
        ("git branch -fd merged".to_string(), "allow"),
        ("git branch --delete -f merged".to_string(), "allow"),
        // Control: plain -d (git itself refuses unmerged) is not force.
        ("git branch -d merged".to_string(), "allow"),
    ];
    check_all(&f, &cases);
}

/// Finding 2: every operand is a deletion, not just the first.
#[test]
fn t1b_f2_every_branch_operand_is_checked() {
    let f = Fixture::new("t1b_f2");
    git(&f.repo, &["branch", "merged2", "merged"]);
    let cases = vec![
        ("git branch -D merged unmerged".to_string(), "ask"),
        ("git branch -D merged merged2 unmerged".to_string(), "ask"),
        ("git branch -d -f merged unmerged".to_string(), "ask"),
        ("git branch -D unmerged merged".to_string(), "ask"),
        // Controls.
        ("git branch -D merged merged2".to_string(), "allow"),
        ("git branch -D merged2 merged".to_string(), "allow"),
    ];
    check_all(&f, &cases);
}

/// Finding 3: the merge check must run in the repo git will act on
/// (`-C`, `--git-dir`), not the session cwd. Branch `x` is merged in the
/// session repo but unmerged in `other`; `y` is the reverse, so neither a
/// blanket Ask nor a check in the wrong repo can pass.
#[test]
fn t1b_f3_c_and_git_dir_target_repo_is_checked() {
    let f = Fixture::new("t1b_f3");
    git(&f.repo, &["branch", "x", "merged"]); // merged here
    git(&f.repo, &["branch", "y", "unmerged"]); // unmerged here
    let other = f.base.join("src/other");
    seed_repo(&other, "main");
    git(&other, &["branch", "y", "main~1"]); // merged there
    add_unmerged_branch(&other, "x", "main"); // unmerged there
    let o = other.display().to_string();
    let r = f.repo.clone();
    let cases = vec![
        (format!("git -C {o} branch -D x"), r.clone(), "ask"),
        (
            format!("git --git-dir={o}/.git branch -D x"),
            r.clone(),
            "ask",
        ),
        (
            format!("git --git-dir {o}/.git branch -D x"),
            r.clone(),
            "ask",
        ),
        // Controls: merged in `other` (unmerged in the session repo).
        (format!("git -C {o} branch -D y"), r.clone(), "allow"),
        (
            format!("git --git-dir={o}/.git branch -D y"),
            r.clone(),
            "allow",
        ),
        (
            format!("git --git-dir {o}/.git branch -D y"),
            r.clone(),
            "allow",
        ),
        // Controls: session repo itself.
        ("git branch -D x".to_string(), r.clone(), "allow"),
        ("git branch -D y".to_string(), r, "ask"),
    ];
    check_all_in(&f, &cases);
}

/// Finding 4: the default branch is resolved (origin/HEAD, init.defaultBranch),
/// not hard-coded to `main`; deleting the default branch itself is Ask;
/// unresolvable default is Ask.
#[test]
fn t1b_f4_default_branch_is_resolved_not_hardcoded() {
    let f = Fixture::new("t1b_f4");
    // master-default repo, resolvable via origin/HEAD, currently on `side`.
    let m = f.base.join("src/masterrepo");
    seed_repo(&m, "master");
    git(&m, &["branch", "merged-into-master", "master~1"]);
    add_unmerged_branch(&m, "unmerged", "master");
    git(&m, &["branch", "side", "master"]);
    set_origin_head(&m, "master");
    git(&m, &["config", "init.defaultBranch", "master"]);
    git(&m, &["switch", "-q", "side"]);

    // Repo whose default cannot be resolved: HEAD `develop`, no remote, no
    // config, no main/master/trunk.
    let nd = f.base.join("src/nodefault");
    seed_repo(&nd, "develop");
    git(&nd, &["branch", "feat", "develop~1"]);

    let cases = vec![
        // master-default repo (session cwd = that repo, no -C).
        (
            "git branch -D merged-into-master".to_string(),
            m.clone(),
            "allow",
        ),
        ("git branch -D unmerged".to_string(), m.clone(), "ask"),
        // Deleting the default branch itself, from another checked-out branch.
        ("git branch -D master".to_string(), m.clone(), "ask"),
        (
            "git branch --delete --force master".to_string(),
            m.clone(),
            "ask",
        ),
        // `main`-default fixture repo, from a linked worktree (cwd = clean).
        ("git branch -D main".to_string(), f.clean_wt.clone(), "ask"),
        (
            "git branch -D merged".to_string(),
            f.clean_wt.clone(),
            "allow",
        ),
        // Unresolvable default => cannot determine => Ask, even for a branch
        // that is in fact an ancestor of HEAD.
        ("git branch -D feat".to_string(), nd, "ask"),
    ];
    check_all_in(&f, &cases);
}

/// Finding 5: when the analysis cannot place the git base directory at all
/// (`base_for("git") == None`), the verdict must be Ask, not Allow.
///
/// Closest observable case: `cd "$VAR" && ...` makes the analysis lose its
/// base (`after_unknown_cd`), which is exactly the `None` branch. This proves
/// the unknown-cwd route through the real hook binary is Ask; it does NOT
/// prove the other way `base_for` can be None (no session cwd in the payload,
/// no project dir), which the hook binary's own fallbacks may fill in.
#[test]
fn t1b_f5_unknown_git_base_is_ask() {
    let f = Fixture::new("t1b_f5");
    let r = f.repo.clone();
    let cases = vec![
        (
            "cd \"$SOMEWHERE\" && git branch -D merged".to_string(),
            r.clone(),
            "ask",
        ),
        (
            "cd \"$SOMEWHERE\" && git branch -D unmerged".to_string(),
            r.clone(),
            "ask",
        ),
        (
            "cd \"$SOMEWHERE\" && git worktree remove --force clean".to_string(),
            r.clone(),
            "ask",
        ),
        // Control: a resolvable cd keeps the normal verdicts.
        (
            format!("cd {} && git branch -D merged", r.display()),
            r.clone(),
            "allow",
        ),
        (
            format!("cd {} && git branch -D unmerged", r.display()),
            r,
            "ask",
        ),
    ];
    check_all_in(&f, &cases);
}

/// Finding 6: a force-delete / force-remove with no operand cannot be judged.
#[test]
fn t1b_f6_missing_operand_is_ask() {
    let f = Fixture::new("t1b_f6");
    let r = f.repo.display().to_string();
    let cases = vec![
        ("git branch -D".to_string(), "ask"),
        ("git branch --delete --force".to_string(), "ask"),
        (format!("git -C {r} branch -D"), "ask"),
        ("git worktree remove --force".to_string(), "ask"),
        (format!("git -C {r} worktree remove -f"), "ask"),
        // Controls: no force / not a delete.
        ("git branch".to_string(), "allow"),
        ("git branch --list".to_string(), "allow"),
        ("git worktree list".to_string(), "allow"),
    ];
    check_all(&f, &cases);
}

/// Finding 7: the force flag may precede `remove`.
#[test]
fn t1b_f7_force_before_remove_is_ask() {
    let f = Fixture::new("t1b_f7");
    let r = f.repo.display().to_string();
    let dirty = f.dirty_wt.display().to_string();
    let clean = f.clean_wt.display().to_string();
    let cases = vec![
        (format!("git -C {r} worktree --force remove {dirty}"), "ask"),
        (format!("git -C {r} worktree -f remove {dirty}"), "ask"),
        (format!("git -C {r} worktree remove {dirty} --force"), "ask"),
        // Controls.
        (
            format!("git -C {r} worktree --force remove {clean}"),
            "allow",
        ),
        (format!("git -C {r} worktree -f remove {clean}"), "allow"),
        (
            format!("git -C {r} worktree remove --force {clean}"),
            "allow",
        ),
    ];
    check_all(&f, &cases);
}

/// Finding 8: a relative worktree path is joined to the session cwd / `-C`
/// dir, not the hook process cwd (`<base>`).
#[test]
fn t1b_f8_relative_worktree_path_resolved_against_right_base() {
    let f = Fixture::new("t1b_f8");
    let src = f.base.join("src");
    let rel_dirty = "../.harness-worktrees/dirty";
    let rel_clean = "../.harness-worktrees/clean";
    let cases = vec![
        // cwd = repo.
        (
            format!("git worktree remove --force {rel_dirty}"),
            f.repo.clone(),
            "ask",
        ),
        (
            format!("git worktree remove --force {rel_clean}"),
            f.repo.clone(),
            "allow",
        ),
        // -C relative to cwd, then the path relative to the -C dir.
        (
            format!("git -C repo worktree remove --force {rel_dirty}"),
            src.clone(),
            "ask",
        ),
        (
            format!("git -C repo worktree remove --force {rel_clean}"),
            src.clone(),
            "allow",
        ),
        // -C absolute, cwd elsewhere.
        (
            format!(
                "git -C {} worktree remove --force {rel_dirty}",
                f.repo.display()
            ),
            src.clone(),
            "ask",
        ),
        (
            format!(
                "git -C {} worktree remove --force {rel_clean}",
                f.repo.display()
            ),
            src,
            "allow",
        ),
    ];
    check_all_in(&f, &cases);
}

/// True when the filesystem under `dir` folds case (macOS APFS default).
fn fs_is_case_insensitive(dir: &Path) -> bool {
    let probe = dir.join("CaseProbe.tmp");
    write(&probe, "x");
    let folded = dir.join("caseprobe.tmp").exists();
    let _ = std::fs::remove_file(&probe);
    folded
}

/// Finding 4b: on a case-insensitive filesystem `git branch -D Main` really
/// deletes `main` (ref lookup goes through the filesystem), so a case-variant
/// of the default branch must be Ask, on both the local-name route and the
/// origin/HEAD route. Controls keep a blanket Ask from passing.
///
/// On a case-sensitive FS the variants name no branch, so the Ask assertions
/// are skipped there (detected at runtime); the controls always run. On
/// macOS APFS (default, case-insensitive) the Ask assertions DO run.
#[test]
fn t1b_f4b_case_variant_of_default_branch_is_ask() {
    let f = Fixture::new("t1b_f4b");
    let ci = fs_is_case_insensitive(&f.base);

    // Route 1: local `main` default (no origin). `main` has a commit that is
    // on no other branch; HEAD sits on `develop`.
    let m = f.base.join("src/cimain");
    seed_repo(&m, "main");
    git(&m, &["branch", "develop", "main~1"]);
    git(&m, &["branch", "feat", "main~1"]);
    add_unmerged_branch(&m, "unmerged", "main");
    write(&m.join("only-main.txt"), "only on main\n");
    git(&m, &["add", "."]);
    git(&m, &["commit", "-q", "-m", "only on main"]);
    git(&m, &["switch", "-q", "develop"]);

    // Route 2: origin/HEAD = develop, local develop ahead of origin/develop,
    // HEAD on `side`.
    let up = f.base.join("src/up");
    seed_repo(&up, "develop");
    let cl = f.base.join("src/cl");
    git(
        &up,
        &["clone", "-q", up.to_str().unwrap(), cl.to_str().unwrap()],
    );
    git(&cl, &["branch", "merged-b", "origin/develop"]);
    write(&cl.join("ahead.txt"), "local develop ahead\n");
    git(&cl, &["add", "."]);
    git(&cl, &["commit", "-q", "-m", "ahead"]);
    git(&cl, &["switch", "-q", "-c", "side"]);

    let mut cases: Vec<(String, PathBuf, &str)> = vec![
        // Controls (any FS).
        ("git branch -D feat".to_string(), m.clone(), "allow"),
        ("git branch -D unmerged".to_string(), m.clone(), "ask"),
        ("git branch -D main".to_string(), m.clone(), "ask"),
        ("git branch -D develop".to_string(), cl.clone(), "ask"),
        ("git branch -D merged-b".to_string(), cl.clone(), "allow"),
    ];
    if ci {
        cases.extend([
            ("git branch -D Main".to_string(), m.clone(), "ask"),
            ("git branch -D MAIN".to_string(), m.clone(), "ask"),
            (
                "git branch --delete --force Main".to_string(),
                m.clone(),
                "ask",
            ),
            ("git branch -d -f Main".to_string(), m.clone(), "ask"),
            ("git branch -D Develop".to_string(), cl.clone(), "ask"),
            ("git branch -D DEVELOP".to_string(), cl.clone(), "ask"),
            // A case-variant resolves exactly as git does: a merged
            // non-default branch stays Allow, an unmerged one stays Ask.
            ("git branch -D Feat".to_string(), m.clone(), "allow"),
            ("git branch -D Unmerged".to_string(), m.clone(), "ask"),
            ("git branch -D Merged-B".to_string(), cl.clone(), "allow"),
        ]);
    }
    check_all_in(&f, &cases);
}

/// True when git, run in `dir`, resolves `refs/heads/<name>` (the filesystem
/// folded the spelling onto an existing loose ref).
fn git_resolves_branch(dir: &Path, name: &str) -> bool {
    let mut cmd = git_env::pinned(dir);
    cmd.args(["rev-parse", "--verify", "-q", &format!("refs/heads/{name}")])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1");
    bounded::run_bounded_stdin(&mut cmd, bounded::ChildStdin::Null, bounded::TOOL_DEADLINE)
        .status
        .success()
}

/// Finding 4c: the filesystem may fold characters beyond ASCII case (APFS
/// folds U+017F LATIN SMALL LETTER LONG S onto `s`, which Rust's
/// `to_lowercase` does not), so `git branch -D maſter` deletes `master`.
/// Whatever spelling git resolves to the default branch, or to an unmerged
/// branch, must be Ask.
///
/// Each variant is asserted ONLY if git, in the fixture, really resolves it
/// to the target ref (probed with `rev-parse`); variants git does not resolve
/// are reported on stderr and skipped. Controls always run.
#[test]
fn t1b_f4c_non_ascii_fold_variant_of_default_branch_is_ask() {
    let f = Fixture::new("t1b_f4c");
    let m = f.base.join("src/foldrepo");
    seed_repo(&m, "master");
    git(&m, &["config", "init.defaultBranch", "master"]);
    git(&m, &["branch", "feat", "master~1"]);
    git(&m, &["branch", "work", "master~1"]);
    add_unmerged_branch(&m, "stale-work", "master");
    // `master` holds a commit that is on no other branch.
    write(&m.join("only-master.txt"), "only on master\n");
    git(&m, &["add", "."]);
    git(&m, &["commit", "-q", "-m", "only on master"]);
    git(&m, &["switch", "-q", "work"]);

    let mut cases: Vec<(String, PathBuf, &str)> = vec![
        // Controls (always run).
        ("git branch -D feat".to_string(), m.clone(), "allow"),
        ("git branch -D master".to_string(), m.clone(), "ask"),
        ("git branch -D stale-work".to_string(), m.clone(), "ask"),
    ];
    let variants = [
        ("ma\u{17f}ter", "master"),
        ("\u{ff4d}aster", "master"),
        ("MA\u{17f}TER", "master"),
        ("\u{17f}tale-work", "stale-work"),
        ("\u{17f}TALE-work", "stale-work"),
    ];
    for (v, target) in variants {
        if git_resolves_branch(&m, v) {
            eprintln!("t1b_f4c: RAN variant {v:?} (git resolves it to {target})");
            cases.push((format!("git branch -D {v}"), m.clone(), "ask"));
            if target == "master" {
                cases.push((format!("git branch --delete --force {v}"), m.clone(), "ask"));
            }
        } else {
            eprintln!("t1b_f4c: SKIPPED variant {v:?} (git does not resolve it on this FS)");
        }
    }
    check_all_in(&f, &cases);
}
