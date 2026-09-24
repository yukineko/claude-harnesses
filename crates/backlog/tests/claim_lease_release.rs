//! Finishing a claimed task RELEASES its lease (backlog f09db5ce follow-up).
//!
//! Claims are leases in the untracked ledger `$HOME/.backlog/claims/<slug>.json`
//! and `claimed` is derived: a pending/failed row with a LIVE lease (< 3600s).
//! Regression observed at 47b8b61c: after `next --claim` then `fail <id>`,
//! `list --json` still reported the task `claimed` (not `failed`), `list
//! --status failed` was empty, and no checkout could re-pick it for up to 1h,
//! because `done` / `fail` / `edit --status` never touched the ledger. Before
//! the lease change these commands overwrote the stored `claimed` status, so
//! the claimant finishing its turn ended the claim. Required: `done`, `fail`
//! and `edit --status <non-claimed>` release the lease; a release that cannot
//! be recorded (ledger unreadable) must not be reported as plain success.
//!
//! Written by an independent test writer BEFORE the fix (CLAUDE.md §2(a)/(b)).
//! Fixture approach mirrors `tests/claim_lease_untracked.rs`: the real binary,
//! `HOME` pinned to a temp dir, a real git repo with a COMMITTED
//! `.backlog/tasks.toml`, and a real linked worktree B of it. B's tracked row is
//! never touched by A's `fail`/`done`/`edit` (separate checkout), so in B the
//! only thing that can make the task look `claimed` is the ledger lease — which
//! is exactly what these tests observe.

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

/// Ids holding a LIVE entry (age < CLAIM_STALE_SECS) in any project ledger.
/// A ledger that exists but cannot be parsed fails the test (never "no ids").
fn live_ledger_ids(f: &Fixture) -> Vec<String> {
    let now = now_unix();
    let mut ids = Vec::new();
    for l in f.ledger_files() {
        let raw = std::fs::read_to_string(&l).expect("ledger readable");
        let v: serde_json::Value = serde_json::from_str(&raw)
            .unwrap_or_else(|e| panic!("ledger {} unparseable ({e}): {raw}", l.display()));
        if let Some(entries) = v["entries"].as_array() {
            for e in entries {
                let at = e["claimed_at"].as_i64().unwrap_or(now);
                if now - at < CLAIM_STALE_SECS {
                    ids.push(e["id"].as_str().unwrap_or_default().to_string());
                }
            }
        }
    }
    ids
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

/// 1. claim in A, `fail` in A: the task is `failed` in A (listed, filterable),
///    not `claimed` anywhere, the lease is gone from the ledger, and B (whose
///    tracked row is still pending and not deferred) can claim it again.
///
///    Note: `fail` has no `--defer` flag and ALWAYS sets `defer_until = now +
///    2 days` in A's store, so A itself cannot re-pick it — re-claimability is
///    observed from B, whose row `fail` in A does not touch.
#[test]
fn fail_releases_the_lease() {
    let f = fixture("f1");
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
        "after `fail`, A must list the task as failed, not claimed: {rows_a:?}"
    );
    let rows_b = list_json(&f, &f.b);
    assert_ne!(
        status_of(&rows_b, "First task"),
        "claimed",
        "after `fail` in A the lease is over; B must not list the task claimed: {rows_b:?}"
    );

    let (code, out, err) = run(&["list", "--json", "--status", "failed"], &f.a, &f.home);
    assert_eq!(code, 0, "list --status failed must succeed: {err}");
    let failed = json(&out);
    assert!(
        failed
            .as_array()
            .expect("array")
            .iter()
            .any(|r| r["id"] == id.as_str()),
        "`list --status failed` must include the failed task {id}: {out}"
    );

    assert!(
        !live_ledger_ids(&f).contains(&id),
        "the ledger must no longer hold a live lease for {id} after `fail`"
    );

    let again = claim(&f, &f.b);
    assert_eq!(
        again["title"], "First task",
        "with the lease released, B must be able to claim the task again: {again}"
    );

    assert_only_row_changed(&f, &before, &id);
}

/// 2. claim in A, `edit --status pending` in A: listed pending (not claimed)
///    in both checkouts, and B can claim it.
#[test]
fn edit_status_pending_releases_the_lease() {
    let f = fixture("f2");
    let before = f.store_bytes(&f.a);
    let got = claim(&f, &f.a);
    assert_eq!(got["title"], "First task", "fixture: p0 wins: {got}");
    let id = got["id"].as_str().unwrap().to_string();

    let (code, out, err) = run(&["edit", &id, "--status", "pending"], &f.a, &f.home);
    assert_eq!(code, 0, "edit must succeed: stdout={out} stderr={err}");

    for (name, cwd) in [("A", &f.a), ("B", &f.b)] {
        let rows = list_json(&f, cwd);
        assert_eq!(
            status_of(&rows, "First task"),
            "pending",
            "after `edit --status pending`, {name} must list the task pending: {rows:?}"
        );
    }

    let again = claim(&f, &f.b);
    assert_eq!(
        again["title"], "First task",
        "B must be able to claim the un-claimed task: {again}"
    );

    assert_only_row_changed(&f, &before, &id);
}

/// 3. claim in A, `done` in A: A lists it done; the lease no longer marks it
///    claimed anywhere (B's tracked row is still pending, so a surviving lease
///    is the only way B could show `claimed`); B's `next --claim` still works.
#[test]
fn done_releases_the_lease() {
    let f = fixture("f3");
    let before = f.store_bytes(&f.a);
    let got = claim(&f, &f.a);
    assert_eq!(got["title"], "First task", "fixture: p0 wins: {got}");
    let id = got["id"].as_str().unwrap().to_string();

    let (code, out, err) = run(&["done", &id], &f.a, &f.home);
    assert_eq!(code, 0, "done must succeed: stdout={out} stderr={err}");

    let rows_a = list_json(&f, &f.a);
    assert_eq!(
        status_of(&rows_a, "First task"),
        "done",
        "A must list the task done: {rows_a:?}"
    );
    let rows_b = list_json(&f, &f.b);
    assert_ne!(
        status_of(&rows_b, "First task"),
        "claimed",
        "after `done` in A the lease is over; B must not list the task claimed: {rows_b:?}"
    );

    let (code, out, err) = run(&["next", "--claim"], &f.b, &f.home);
    assert_eq!(
        code, 0,
        "B's next --claim must not error after A's done: stdout={out} stderr={err}"
    );

    // `done` may move the row to the done file; only require that no
    // `claimed` bytes appear and the other rows are untouched in tasks.toml.
    assert_only_row_changed(&f, &before, &id);
}

/// 4. claim in A, corrupt the ledger, `fail` in A: the release cannot be
///    recorded, so the command must NOT report plain success silently — it
///    either exits non-zero or warns on stderr that the lease is still held.
#[test]
fn fail_with_unreadable_ledger_is_not_silent() {
    let f = fixture("f4");
    let got = claim(&f, &f.a);
    let id = got["id"].as_str().unwrap().to_string();

    let ledgers = f.ledger_files();
    assert!(!ledgers.is_empty(), "fixture: a claim must create a ledger");
    for l in &ledgers {
        std::fs::write(l, "{ this is not json").unwrap();
    }

    let (code, out, err) = run(&["fail", &id, "--reason", "boom"], &f.a, &f.home);
    assert!(
        code != 0 || err.to_lowercase().contains("lease"),
        "`fail` whose lease release cannot be recorded must exit non-zero or warn \
         about the lease on stderr; got exit {code}, stdout={out} stderr={err}"
    );
}
