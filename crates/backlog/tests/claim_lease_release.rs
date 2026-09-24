//! The DERIVED `claimed` view must not mask a claimant's own `fail`
//! (backlog f09db5ce follow-up).
//!
//! Claims are leases in the untracked ledger `$HOME/.backlog/claims/<slug>.json`
//! and `claimed` is derived: a pending/failed row with a LIVE lease (< 3600s).
//! Regression observed at 47b8b61c: after `next --claim` then `fail <id>`,
//! `list --json` still reported the task `claimed` (not `failed`) and `list
//! --status failed` was empty.
//!
//! Spec (coordinator revision, after a first draft of these tests showed that
//! RELEASING the lease on done/fail lets a diverged checkout re-dispatch
//! finished or just-failed work): NOTHING releases the lease; exclusion is
//! unchanged and ends only when the lease ages out at CLAIM_STALE_SECS. The fix
//! is confined to the derived view:
//!   * `pending` row + live lease: "claimed" (unchanged);
//!   * `failed` row + live lease whose `claimed_at` is STRICTLY greater than
//!     the row's `updated_at` (a re-claim of an older failed row): "claimed";
//!   * `failed` row updated AT or AFTER the claim (the claimant ran `fail`):
//!     "failed", both displayed and filtered;
//!   * terminal rows are never "claimed".
//!
//! Written by an independent test writer BEFORE the fix (CLAUDE.md §2(a)/(b)).
//! Fixture approach mirrors `tests/claim_lease_untracked.rs`: the real binary,
//! `HOME` pinned to a temp dir, a real git repo with a COMMITTED
//! `.backlog/tasks.toml`, and a real linked worktree B of it. B's tracked row is
//! never touched by A's `fail`/`done` (separate checkout), so B observes the
//! ledger's exclusion alone.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Mirror of `store::CLAIM_STALE_SECS`; a lease older than this no longer
/// excludes its task.
const CLAIM_STALE_SECS: i64 = 3600;

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn unique_root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "backlog-release-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    // macOS' temp dir is a symlink (/var -> /private/var): canonicalize so the
    // project label written here equals what the binary resolves.
    std::fs::canonicalize(&dir).unwrap()
}

fn run_with_stdin(args: &[&str], cwd: &Path, home: &Path, stdin: &str) -> (i32, String, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_backlog"))
        .args(args)
        .env("HOME", home)
        .env_remove("BACKLOG_DISABLE")
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary spawns");
    if let Some(mut s) = child.stdin.take() {
        let _ = s.write_all(stdin.as_bytes());
    }
    let out = child.wait_with_output().expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn run(args: &[&str], cwd: &Path, home: &Path) -> (i32, String, String) {
    run_with_stdin(args, cwd, home, "")
}

fn git_cmd(args: &[&str], cwd: &Path, home: &Path) -> std::process::Output {
    Command::new("git")
        .args(args)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .expect("git is available")
}

/// Run `git`, asserting success. A fixture that cannot build its own repo
/// must fail loudly, never skip.
fn git(args: &[&str], cwd: &Path, home: &Path) -> String {
    let out = git_cmd(args, cwd, home);
    assert!(
        out.status.success(),
        "git {args:?} failed in {}: {}{}",
        cwd.display(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

struct Fixture {
    home: PathBuf,
    /// Main working tree (checkout A).
    a: PathBuf,
    /// Real linked worktree of `a` (checkout B), same committed tasks.
    b: PathBuf,
}

impl Fixture {
    fn store(&self, checkout: &Path) -> PathBuf {
        checkout.join(".backlog").join("tasks.toml")
    }

    /// The tracked store's exact bytes (as a lossless UTF-8 string, so a
    /// mismatch prints readable TOML rather than a byte array).
    fn store_bytes(&self, checkout: &Path) -> String {
        String::from_utf8(std::fs::read(self.store(checkout)).expect("tasks.toml readable"))
            .expect("tasks.toml is UTF-8")
    }

    fn porcelain(&self, checkout: &Path) -> String {
        git(&["status", "--porcelain"], checkout, &self.home)
    }

    /// Every `*.json` directly under `$HOME/.backlog/claims/`.
    fn ledger_files(&self) -> Vec<PathBuf> {
        let dir = self.home.join(".backlog").join("claims");
        // Absent dir = genuinely no ledger yet; any other error must fail the
        // test rather than read as "no ledger" (fail closed).
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
            Err(e) => panic!("cannot read {}: {e}", dir.display()),
        };
        entries
            .map(|e| e.expect("claims dir entry readable").path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect()
    }
}

/// Build a repo with `.backlog/tasks.toml` COMMITTED, then a linked worktree
/// branched from that commit (so both checkouts hold the same bytes, same ids).
/// `seed` writes the store into the main tree before the commit.
fn fixture_with(tag: &str, seed: impl FnOnce(&Path, &Path)) -> Fixture {
    let root = unique_root(tag);
    let home = root.join("home");
    let a = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&a).unwrap();

    git(&["init", "-q", "-b", "main"], &a, &home);
    git(&["config", "user.name", "t"], &a, &home);
    git(&["config", "user.email", "t@example.invalid"], &a, &home);
    git(&["commit", "-q", "--allow-empty", "-m", "init"], &a, &home);

    seed(&a, &home);

    git(&["add", ".backlog/tasks.toml"], &a, &home);
    git(&["commit", "-q", "-m", "seed backlog"], &a, &home);

    let b = root.join("wt");
    git(
        &["worktree", "add", "-q", "-b", "side", b.to_str().unwrap()],
        &a,
        &home,
    );
    assert!(b.join(".git").is_file(), "B must be a linked worktree");

    let f = Fixture { home, a, b };
    // Fixture sanity: both checkouts start clean and identical. If this
    // fails, the fixture (e.g. an untracked lockfile left by `add`) is at
    // fault, not the implementation under test.
    assert_eq!(f.porcelain(&f.a), "", "fixture: A must start clean");
    assert_eq!(f.porcelain(&f.b), "", "fixture: B must start clean");
    assert_eq!(
        f.store_bytes(&f.a),
        f.store_bytes(&f.b),
        "fixture: A and B must hold the same committed store"
    );
    f
}

/// Default fixture: two tasks added through the real CLI.
fn fixture(tag: &str) -> Fixture {
    fixture_with(tag, |a, home| {
        let project = a.to_str().unwrap().to_string();
        for (title, prio) in [("First task", "p0"), ("Second task", "p1")] {
            let (code, _, err) = run(
                &[
                    "add",
                    "--title",
                    title,
                    "--project",
                    &project,
                    "--priority",
                    prio,
                ],
                a,
                home,
            );
            assert_eq!(code, 0, "fixture add must succeed: {err}");
        }
    })
}

fn json(stdout: &str) -> serde_json::Value {
    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("expected JSON ({e}), got: {stdout}"))
}

/// Claim in `cwd`, asserting it succeeded and handed out a task.
fn claim(f: &Fixture, cwd: &Path) -> serde_json::Value {
    let (code, out, err) = run(&["next", "--claim"], cwd, &f.home);
    assert_eq!(code, 0, "claim must succeed: stdout={out} stderr={err}");
    assert!(
        !out.contains("no pending tasks"),
        "claim must hand out a task: {out}"
    );
    json(&out)
}

/// `list --json` in `cwd`, asserting exit 0.
fn list_json(f: &Fixture, cwd: &Path) -> Vec<serde_json::Value> {
    let (code, out, err) = run(&["list", "--json"], cwd, &f.home);
    assert_eq!(code, 0, "list --json must succeed: stderr={err}");
    json(&out)
        .as_array()
        .expect("list --json is an array")
        .clone()
}

/// Status of the task titled `title` in a `list --json` result.
fn status_of(rows: &[serde_json::Value], title: &str) -> String {
    rows.iter()
        .find(|r| r["title"] == title)
        .unwrap_or_else(|| panic!("task {title:?} missing from list: {rows:?}"))["status"]
        .as_str()
        .unwrap()
        .to_string()
}

fn count_claimed_bytes(bytes: &str) -> usize {
    bytes.matches("status = \"claimed\"").count()
}

/// The `[[task]]` blocks of a tasks.toml that do NOT belong to `id`, so a test
/// can assert that a command on `id` rewrote only that row.
fn other_blocks(bytes: &str, id: &str) -> Vec<String> {
    let needle = format!("id = \"{id}\"");
    bytes
        .split("[[task]]")
        .filter(|b| !b.contains(&needle))
        .map(|b| b.trim().to_string())
        .filter(|b| !b.is_empty())
        .collect()
}

/// Shared post-condition on A's tracked store: no `claimed` status bytes were
/// ever written, and every row other than `id` is byte-identical to `before`.
fn assert_only_row_changed(f: &Fixture, before: &str, id: &str) {
    let after = f.store_bytes(&f.a);
    assert_eq!(
        count_claimed_bytes(&after),
        0,
        "no `claimed` status may be written into A's tracked store:\n{after}"
    );
    assert_eq!(
        other_blocks(&after, id),
        other_blocks(before, id),
        "only the row of {id} may change in A's tracked store:\n--- before\n{before}\n--- after\n{after}"
    );
}

/// Ids returned by `list --json --status <status>` in `cwd`.
fn ids_with_status(f: &Fixture, cwd: &Path, status: &str) -> Vec<String> {
    let (code, out, err) = run(&["list", "--json", "--status", status], cwd, &f.home);
    assert_eq!(code, 0, "list --status {status} must succeed: {err}");
    json(&out)
        .as_array()
        .expect("array")
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect()
}

/// `next --claim` in `cwd` must succeed and must NOT hand out `title`.
fn assert_claim_does_not_return(f: &Fixture, cwd: &Path, title: &str, why: &str) {
    let (code, out, err) = run(&["next", "--claim"], cwd, &f.home);
    assert_eq!(
        code, 0,
        "next --claim must succeed: stdout={out} stderr={err}"
    );
    if !out.contains("no pending tasks") {
        let v = json(&out);
        assert_ne!(v["title"], title, "{why}: {out}");
    }
}

/// Set `claimed_at` of every ledger entry for `id` to `claimed_at`.
fn set_lease_claimed_at(f: &Fixture, id: &str, claimed_at: i64) {
    let ledgers = f.ledger_files();
    assert_eq!(ledgers.len(), 1, "exactly one project ledger: {ledgers:?}");
    let mut v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&ledgers[0]).unwrap()).unwrap();
    let mut hit = 0;
    for e in v["entries"].as_array_mut().expect("ledger has entries") {
        if e["id"] == id {
            e["claimed_at"] = serde_json::json!(claimed_at);
            hit += 1;
        }
    }
    assert!(hit > 0, "fixture: ledger must hold an entry for {id}");
    std::fs::write(&ledgers[0], serde_json::to_string_pretty(&v).unwrap()).unwrap();
}

/// The `list --json` row titled `title` in `cwd`.
fn row_of(f: &Fixture, cwd: &Path, title: &str) -> serde_json::Value {
    list_json(f, cwd)
        .into_iter()
        .find(|r| r["title"] == title)
        .unwrap_or_else(|| panic!("task {title:?} missing"))
}

/// 1. claim in A, `fail` in A: A shows it `failed` (displayed and filtered;
///    not under `--status claimed`). The lease is NOT released: B still shows
///    it `claimed` and B's `next --claim` does not hand it out.
#[test]
fn claimant_fail_is_displayed_failed_but_still_excludes() {
    let f = fixture("v1");
    let before = f.store_bytes(&f.a);
    let got = claim(&f, &f.a);
    assert_eq!(got["title"], "First task", "fixture: p0 wins: {got}");
    let id = got["id"].as_str().expect("claim result has id").to_string();

    let (code, out, err) = run(&["fail", &id, "--reason", "boom"], &f.a, &f.home);
    assert_eq!(code, 0, "fail must succeed: stdout={out} stderr={err}");

    let rows_a = list_json(&f, &f.a);
    assert_eq!(
        status_of(&rows_a, "First task"),
        "failed",
        "after the claimant's `fail`, A must list the task failed, not claimed: {rows_a:?}"
    );
    assert!(
        ids_with_status(&f, &f.a, "failed").contains(&id),
        "`list --status failed` in A must include {id}"
    );
    assert!(
        !ids_with_status(&f, &f.a, "claimed").contains(&id),
        "`list --status claimed` in A must not include the failed task {id}"
    );

    let rows_b = list_json(&f, &f.b);
    assert_eq!(
        status_of(&rows_b, "First task"),
        "claimed",
        "the lease is not released by `fail`: B (row still pending) must show claimed: {rows_b:?}"
    );
    assert_claim_does_not_return(
        &f,
        &f.b,
        "First task",
        "B must not re-dispatch a task A just failed while its lease is live",
    );

    assert_only_row_changed(&f, &before, &id);
}

/// 2. claim in A, `done` in A: A shows done; B still shows `claimed` and B's
///    `next --claim` does not hand out finished work.
#[test]
fn claimant_done_is_not_redispatched_from_another_checkout() {
    let f = fixture("v2");
    let before = f.store_bytes(&f.a);
    let got = claim(&f, &f.a);
    assert_eq!(got["title"], "First task", "fixture: p0 wins: {got}");
    let id = got["id"].as_str().unwrap().to_string();

    let (code, out, err) = run(&["done", &id], &f.a, &f.home);
    assert_eq!(code, 0, "done must succeed: stdout={out} stderr={err}");

    assert_eq!(
        status_of(&list_json(&f, &f.a), "First task"),
        "done",
        "A must list the task done"
    );
    let rows_b = list_json(&f, &f.b);
    assert_eq!(
        status_of(&rows_b, "First task"),
        "claimed",
        "the lease is not released by `done`: B must still show claimed: {rows_b:?}"
    );
    assert_claim_does_not_return(
        &f,
        &f.b,
        "First task",
        "B must not re-dispatch work A already finished",
    );

    // `done` may move the row to the done file; tasks.toml must still gain
    // no `claimed` bytes and keep the other rows untouched.
    assert_only_row_changed(&f, &before, &id);
}

/// 3. A failed row last updated LONG before a live lease (a re-claim of an old
///    failed task) is displayed `claimed`, and filtered as such.
#[test]
fn reclaimed_old_failed_row_is_displayed_claimed() {
    let f = fixture_with("v3", |a, _home| {
        let dir = a.join(".backlog");
        std::fs::create_dir_all(&dir).unwrap();
        // updated_at and defer_until far in the past: failed, not deferred,
        // therefore claimable.
        std::fs::write(
            dir.join("tasks.toml"),
            format!(
                "[[task]]\nid = \"oldfail1\"\ntitle = \"Old failure\"\nproject = \"{}\"\n\
                 tags = [\"p0\"]\nstatus = \"failed\"\nnotes = \"boom\"\ncreated_at = 1000\n\
                 updated_at = 1000\ndefer_until = 1000\n",
                a.display()
            ),
        )
        .unwrap();
    });
    let before = f.store_bytes(&f.a);
    assert_eq!(
        status_of(&list_json(&f, &f.a), "Old failure"),
        "failed",
        "fixture: an unleased failed row is listed failed"
    );

    let got = claim(&f, &f.a);
    assert_eq!(
        got["title"], "Old failure",
        "the old failed row is claimable: {got}"
    );

    for (name, cwd) in [("A", &f.a), ("B", &f.b)] {
        assert_eq!(
            status_of(&list_json(&f, cwd), "Old failure"),
            "claimed",
            "{name}: a failed row re-claimed after its last update must show claimed"
        );
        assert!(
            ids_with_status(&f, cwd, "claimed").contains(&"oldfail1".to_string()),
            "{name}: `list --status claimed` must include the re-claimed row"
        );
        assert!(
            !ids_with_status(&f, cwd, "failed").contains(&"oldfail1".to_string()),
            "{name}: `list --status failed` must not include the re-claimed row"
        );
    }
    assert_eq!(
        f.store_bytes(&f.a),
        before,
        "claiming must not touch A's tracked store"
    );
}

/// 4. Boundary, pinned deterministically by editing the lease: for a failed
///    row, claimed_at == updated_at => "failed" (updated AT the claim counts as
///    the claimant's fail); claimed_at == updated_at + 1 => "claimed".
#[test]
fn failed_row_claimed_boundary_is_strict() {
    let f = fixture("v4");
    let got = claim(&f, &f.a);
    let id = got["id"].as_str().unwrap().to_string();
    let (code, out, err) = run(&["fail", &id, "--reason", "boom"], &f.a, &f.home);
    assert_eq!(code, 0, "fail must succeed: stdout={out} stderr={err}");

    let updated_at = row_of(&f, &f.a, "First task")["updated_at"]
        .as_i64()
        .expect("updated_at is an integer");
    assert!(
        now_unix() - updated_at < CLAIM_STALE_SECS,
        "fixture: updated_at is recent, so a lease stamped at it is live"
    );

    set_lease_claimed_at(&f, &id, updated_at);
    assert_eq!(
        status_of(&list_json(&f, &f.a), "First task"),
        "failed",
        "claimed_at == updated_at: the row was updated at the claim => failed"
    );

    set_lease_claimed_at(&f, &id, updated_at + 1);
    assert_eq!(
        status_of(&list_json(&f, &f.a), "First task"),
        "claimed",
        "claimed_at > updated_at: a claim after the failure => claimed"
    );
}
