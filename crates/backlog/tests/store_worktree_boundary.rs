//! Backlog 1e6f00ae: a state-store WRITE whose resolved store lies inside the
//! PRIMARY working tree of a git repository is refused (CLAUDE.md §8: main's
//! tree may only receive merges). Writes from a LINKED worktree succeed, reads
//! from the primary tree stay allowed, and a cwd with no git repo above it
//! keeps its existing behaviour (backlog refuses: "no project store").
//!
//! Every refusal test asserts on the FILE (existence / exact bytes), not just
//! the exit code: a refusal that still wrote is the defect, and a non-zero exit
//! alone cannot tell the two apart.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A primary checkout plus a linked worktree of it, and an isolated HOME.
struct Fixture {
    _tmp: tempfile::TempDir,
    primary: PathBuf,
    linked: PathBuf,
    home: PathBuf,
    shim: PathBuf,
}

fn git(cwd: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `seed`: optional tasks.toml body committed into the repo before the
/// worktree is cut, so both checkouts see it.
fn fixture(seed: Option<&dyn Fn(&Path) -> String>) -> Fixture {
    let tmp = tempfile::tempdir().expect("tempdir");
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    let primary = base.join("primary");
    let home = base.join("home");
    let shim = base.join("shim");
    std::fs::create_dir_all(&primary).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&shim).unwrap();

    // `condukt` shim: answers "not claimed" the way the real one does
    // (`{"claimed":false}` + exit 1; backlog 420f1eec trusts exit 1 only with
    // that field) so no machine state leaks in.
    let condukt = shim.join("condukt");
    std::fs::write(&condukt, "#!/bin/sh\necho '{\"claimed\":false}'\nexit 1\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&condukt, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    // Fake `gh`: logs every invocation next to itself and answers `issue create`
    // with a plausible issue URL (what `github::decide_issue_create` parses), so
    // any issue-sync path would REALLY write issue_number/issue_url. Also keeps
    // the real `gh` (and the network) out of every test.
    let gh = shim.join("gh");
    std::fs::write(
        &gh,
        "#!/bin/sh\necho \"$@\" >> \"$(dirname \"$0\")/gh.log\"\nif [ \"$1 $2\" = \"issue create\" ]; then echo https://github.com/o/r/issues/42; fi\nexit 0\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    git(&primary, &["init", "-q", "-b", "main"]);
    git(
        &primary,
        &["remote", "add", "origin", "https://github.com/o/r.git"],
    );
    std::fs::write(primary.join("README"), "x\n").unwrap();
    if let Some(f) = seed {
        std::fs::create_dir_all(primary.join(".backlog")).unwrap();
        std::fs::write(primary.join(".backlog/tasks.toml"), f(&primary)).unwrap();
    }
    git(&primary, &["add", "-A"]);
    git(&primary, &["commit", "-q", "-m", "init"]);

    let linked = base.join("linked");
    git(
        &primary,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature",
            linked.to_str().unwrap(),
        ],
    );
    Fixture {
        _tmp: tmp,
        primary,
        linked,
        home,
        shim,
    }
}

fn seeded_task(project: &Path) -> String {
    format!(
        "[[task]]\nid = \"seed0001\"\ntitle = \"seeded\"\nproject = \"{}\"\ntags = []\nstatus = \"pending\"\nnotes = \"\"\ncreated_at = 1000\nupdated_at = 1000\n\n",
        project.display()
    )
}

/// Two pending tasks, so `done seed0001 --duplicate-of seed0002` is a VALID
/// close-evidence form (a bare `done` is refused for missing evidence).
fn seeded_two(project: &Path) -> String {
    format!(
        "{}[[task]]\nid = \"seed0002\"\ntitle = \"other\"\nproject = \"{}\"\ntags = []\nstatus = \"pending\"\nnotes = \"\"\ncreated_at = 1000\nupdated_at = 1000\n\n",
        seeded_task(project),
        project.display()
    )
}

fn run(fx_home: &Path, shim: &Path, cwd: &Path, args: &[&str]) -> (i32, String, String) {
    let path = format!(
        "{}:{}",
        shim.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = Command::new(env!("CARGO_BIN_EXE_backlog"))
        .args(args)
        .env("HOME", fx_home)
        .env("PATH", path)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env_remove("BACKLOG_DISABLE")
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .expect("backlog runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn store(root: &Path) -> PathBuf {
    root.join(".backlog").join("tasks.toml")
}

#[test]
fn add_from_primary_checkout_is_refused_and_writes_nothing() {
    let fx = fixture(None);
    let (code, out, err) = run(
        &fx.home,
        &fx.shim,
        &fx.primary,
        &[
            "add",
            "--title",
            "t1",
            "--project",
            fx.primary.to_str().unwrap(),
        ],
    );
    assert_ne!(
        code, 0,
        "write from primary must be refused; out={out:?} err={err:?}"
    );
    assert!(
        !store(&fx.primary).exists(),
        "refused add must not create the store in the primary tree; out={out:?} err={err:?}"
    );
    assert!(
        !fx.primary.join(".backlog").exists(),
        "refused add must not even create .backlog/ in the primary tree"
    );
    assert!(
        err.to_lowercase().contains("worktree"),
        "the error must tell the caller to run from a linked worktree; stderr={err:?}"
    );
}

#[test]
fn add_from_linked_worktree_succeeds_and_lands_in_that_worktree() {
    let fx = fixture(None);
    let (code, out, err) = run(
        &fx.home,
        &fx.shim,
        &fx.linked,
        &[
            "add",
            "--title",
            "from-wt",
            "--project",
            fx.linked.to_str().unwrap(),
        ],
    );
    assert_eq!(
        code, 0,
        "linked-worktree write must succeed; out={out:?} err={err:?}"
    );
    let body = std::fs::read_to_string(store(&fx.linked)).expect("store written in worktree");
    assert!(
        body.contains("from-wt"),
        "task must land in the worktree store: {body}"
    );
    assert!(
        !store(&fx.primary).exists(),
        "the primary tree must stay untouched by a worktree write"
    );
}

#[test]
fn done_from_primary_checkout_is_refused_and_file_unchanged() {
    let fx = fixture(Some(&seeded_two));
    let before = std::fs::read(store(&fx.primary)).unwrap();
    // VALID evidence form: the refusal must come from the primary-tree guard,
    // not from close-evidence.
    let (code, out, err) = run(
        &fx.home,
        &fx.shim,
        &fx.primary,
        &["done", "seed0001", "--duplicate-of", "seed0002"],
    );
    assert_ne!(
        code, 0,
        "done from primary must be refused; out={out:?} err={err:?}"
    );
    assert!(
        err.contains("inside the primary working tree"),
        "refusal must be the primary-tree guard; stderr={err:?}"
    );
    assert!(
        !err.contains("evidence"),
        "refusal must not be a close-evidence refusal; stderr={err:?}"
    );
    assert_eq!(
        before,
        std::fs::read(store(&fx.primary)).unwrap(),
        "refused done must leave the primary store byte-identical; out={out:?} err={err:?}"
    );
}

#[test]
fn edit_from_primary_checkout_is_refused_and_file_unchanged() {
    let fx = fixture(Some(&seeded_task));
    let before = std::fs::read(store(&fx.primary)).unwrap();
    let (code, out, err) = run(
        &fx.home,
        &fx.shim,
        &fx.primary,
        &["edit", "seed0001", "--title", "renamed"],
    );
    assert_ne!(
        code, 0,
        "edit from primary must be refused; out={out:?} err={err:?}"
    );
    assert_eq!(
        before,
        std::fs::read(store(&fx.primary)).unwrap(),
        "refused edit must leave the primary store byte-identical; out={out:?} err={err:?}"
    );
}

#[test]
fn done_from_linked_worktree_succeeds_and_marks_done_there() {
    let fx = fixture(Some(&seeded_two));
    let main_before = std::fs::read(store(&fx.primary)).unwrap();
    let (code, out, err) = run(
        &fx.home,
        &fx.shim,
        &fx.linked,
        &["done", "seed0001", "--duplicate-of", "seed0002"],
    );
    assert_eq!(
        code, 0,
        "done from a linked worktree must succeed; out={out:?} err={err:?}"
    );
    // `done` archives the finished task out of tasks.toml into tasks.done.toml.
    let done = std::fs::read_to_string(fx.linked.join(".backlog").join("tasks.done.toml"))
        .unwrap_or_default();
    assert!(
        done.contains("seed0001"),
        "worktree archive must record the completion: {done}"
    );
    assert_eq!(
        main_before,
        std::fs::read(store(&fx.primary)).unwrap(),
        "primary store must be untouched by the worktree write"
    );
}

#[test]
fn list_from_primary_checkout_still_works() {
    let fx = fixture(Some(&seeded_task));
    let before = std::fs::read(store(&fx.primary)).unwrap();
    let (code, out, err) = run(&fx.home, &fx.shim, &fx.primary, &["list"]);
    assert_eq!(
        code, 0,
        "reads from the primary tree stay allowed; out={out:?} err={err:?}"
    );
    assert!(
        out.contains("seeded"),
        "list must show the seeded task: {out:?}"
    );
    assert_eq!(
        before,
        std::fs::read(store(&fx.primary)).unwrap(),
        "list must not modify the store"
    );
}

/// Control: a cwd with no git repo above it. Observed today: backlog refuses
/// (exit 1, "no git repo above ...") and creates nothing, neither in the dir
/// nor in HOME. The fix must not change this.
#[test]
fn add_outside_any_git_repo_keeps_its_current_refusal() {
    let fx = fixture(None);
    let nogit = tempfile::tempdir().unwrap();
    let cwd = std::fs::canonicalize(nogit.path()).unwrap();
    let (code, out, err) = run(
        &fx.home,
        &fx.shim,
        &cwd,
        &["add", "--title", "t", "--project", cwd.to_str().unwrap()],
    );
    assert_eq!(code, 1, "out={out:?} err={err:?}");
    assert!(err.contains("no git repo above"), "stderr={err:?}");
    assert!(
        !cwd.join(".backlog").exists(),
        "nothing may be created in the dir"
    );
    assert!(
        !fx.home.join(".backlog").join("tasks.toml").exists(),
        "no fallback to the cross-project ~/.backlog"
    );
}

/// Files under the temp HOME's claim ledger (`~/.backlog/claims`), i.e. the
/// recorded leases. Empty / absent means no claim was recorded.
fn ledger_files(home: &Path) -> Vec<PathBuf> {
    let dir = home.join(".backlog").join("claims");
    match std::fs::read_dir(&dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect(),
        Err(_) => Vec::new(),
    }
}

fn gh_log(shim: &Path) -> String {
    std::fs::read_to_string(shim.join("gh.log")).unwrap_or_default()
}

/// Overwrite the LINKED worktree's tasks.toml with a pending task labelled for
/// that worktree, so `--project <linked>` scoping matches.
fn seed_linked(fx: &Fixture) {
    std::fs::create_dir_all(fx.linked.join(".backlog")).unwrap();
    std::fs::write(store(&fx.linked), seeded_task(&fx.linked)).unwrap();
}

/// Pins a measured fact (not a refusal): the claim lease lives under
/// `$HOME/.backlog/claims`, outside the repo, so `next --claim` from the primary
/// checkout is NOT a store write into main's tree and stays allowed. It must
/// never dirty main (no tasks.toml change, no stray file, clean `git status`).
#[test]
fn next_claim_from_primary_checkout_is_allowed_and_never_dirties_main() {
    let fx = fixture(Some(&seeded_task));
    let before = std::fs::read(store(&fx.primary)).unwrap();
    let (code, out, err) = run(
        &fx.home,
        &fx.shim,
        &fx.primary,
        &["next", "--claim", "--project", fx.primary.to_str().unwrap()],
    );
    assert_eq!(code, 0, "claim stays allowed; out={out:?} err={err:?}");
    assert!(out.contains("seed0001"), "claimed task printed: {out:?}");
    assert_eq!(
        before,
        std::fs::read(store(&fx.primary)).unwrap(),
        "primary tasks.toml must be byte-identical"
    );
    let st = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(&fx.primary)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .expect("git status runs");
    assert!(st.status.success(), "git status failed");
    assert_eq!(
        String::from_utf8_lossy(&st.stdout),
        "",
        "primary working tree must stay clean after a claim"
    );
    assert_eq!(ledger_files(&fx.home).len(), 1, "lease lives under HOME");
}

#[test]
fn next_claim_from_linked_worktree_succeeds_and_records_a_lease() {
    let fx = fixture(Some(&seeded_task));
    seed_linked(&fx);
    let main_before = std::fs::read(store(&fx.primary)).unwrap();
    let (code, out, err) = run(
        &fx.home,
        &fx.shim,
        &fx.linked,
        &["next", "--claim", "--project", fx.linked.to_str().unwrap()],
    );
    assert_eq!(
        code, 0,
        "next --claim from a linked worktree must succeed; out={out:?} err={err:?}"
    );
    assert!(out.contains("seed0001"), "claimed task printed: {out:?}");
    assert_eq!(
        ledger_files(&fx.home).len(),
        1,
        "exactly one lease must be recorded"
    );
    assert_eq!(
        main_before,
        std::fs::read(store(&fx.primary)).unwrap(),
        "primary tree untouched"
    );
}

/// The GitHub issue-sync write that stamps issue_number/issue_url into
/// tasks.toml. In this tree it is `sync --apply` (and `add`/`done`), NOT
/// `next --claim`, which never calls `gh`.
#[test]
fn sync_apply_from_primary_checkout_is_refused_and_creates_no_issue() {
    let fx = fixture(Some(&seeded_task));
    let before = std::fs::read(store(&fx.primary)).unwrap();
    let (code, out, err) = run(&fx.home, &fx.shim, &fx.primary, &["sync", "--apply"]);
    assert_ne!(
        code, 0,
        "sync --apply from primary must be refused; out={out:?} err={err:?}"
    );
    assert_eq!(
        before,
        std::fs::read(store(&fx.primary)).unwrap(),
        "primary tasks.toml must be byte-identical (no issue_number stamped); out={out:?} err={err:?}"
    );
    assert!(
        !gh_log(&fx.shim).contains("issue create"),
        "a refused sync must not create issues: {}",
        gh_log(&fx.shim)
    );
}

#[test]
fn sync_apply_from_linked_worktree_succeeds_and_stamps_issue_number_there() {
    let fx = fixture(Some(&seeded_task));
    seed_linked(&fx);
    let main_before = std::fs::read(store(&fx.primary)).unwrap();
    let (code, out, err) = run(&fx.home, &fx.shim, &fx.linked, &["sync", "--apply"]);
    assert_eq!(
        code, 0,
        "sync --apply from a linked worktree must succeed; out={out:?} err={err:?}"
    );
    let body = std::fs::read_to_string(store(&fx.linked)).unwrap();
    assert!(
        body.contains("issue_number = 42"),
        "worktree store must carry the stamped issue number: {body}"
    );
    assert_eq!(main_before, std::fs::read(store(&fx.primary)).unwrap());
}
