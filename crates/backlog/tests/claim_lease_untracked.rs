//! The claim LEASE lives only in the untracked, project-wide claim ledger
//! (`~/.backlog/claims/<project-slug>.json`), never in the tracked store
//! `.backlog/tasks.toml` (backlog f09db5ce).
//!
//! Before: `next --claim` reserved in the ledger AND wrote `status =
//! "claimed"` + `updated_at` into the tracked `tasks.toml`, and SessionStart's
//! `requeue_expired` rewrote stale claimed rows — so every claim dirtied the
//! git worktree it ran in. After: the tracked store keeps only durable state,
//! and `claimed` is a DERIVED view = a pending row + a LIVE ledger lease
//! (age < `CLAIM_STALE_SECS` = 3600s).
//!
//! These tests were written by an independent test writer BEFORE the
//! implementation (CLAUDE.md §2(a)/(b)). They drive the real binary in
//! separate OS processes against a REAL git repo whose `.backlog/tasks.toml`
//! is COMMITTED, plus a REAL linked worktree of it, so `git status
//! --porcelain` is a meaningful observation. `HOME` is pinned to a temp dir
//! in every invocation so the ledger is the test's own, never the user's.

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
        "backlog-lease-{}-{}-{}",
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

/// 1. A claim leaves the tracked store byte-identical and the worktree clean,
///    yet the claim is visible (derived) from BOTH checkouts and excludes the
///    task from B.
#[test]
fn claim_does_not_dirty_the_tracked_store_and_is_visible_across_worktrees() {
    let f = fixture("t1");
    let before = f.store_bytes(&f.a);

    let got = claim(&f, &f.a);
    assert_eq!(got["title"], "First task", "p0 wins: {got}");
    assert_eq!(
        got["status"], "claimed",
        "the claim result must still report status claimed: {got}"
    );
    let hk = got["hashkey"].as_str().unwrap().to_string();

    assert_eq!(
        f.store_bytes(&f.a),
        before,
        "the tracked .backlog/tasks.toml in A must be byte-identical after a claim"
    );
    assert_eq!(
        f.porcelain(&f.a),
        "",
        "a claim must not dirty A's git worktree"
    );

    let other = claim(&f, &f.b);
    assert_ne!(
        other["title"], "First task",
        "B must not be handed the task A holds a lease on: {other}"
    );
    assert_ne!(other["hashkey"].as_str().unwrap(), hk);

    for (name, cwd) in [("A", &f.a), ("B", &f.b)] {
        let rows = list_json(&f, cwd);
        assert_eq!(
            status_of(&rows, "First task"),
            "claimed",
            "list --json in {name} must show the leased task as claimed (derived from the ledger): {rows:?}"
        );
    }
}

/// 2. Plain `next` (no --claim) never returns a leased task — in the claiming
///    checkout OR in another checkout of the same project.
#[test]
fn plain_next_skips_a_leased_task_in_every_checkout() {
    let f = fixture("t2");
    claim(&f, &f.a);

    for (name, cwd) in [("A", &f.a), ("B", &f.b)] {
        let (code, out, err) = run(&["next"], cwd, &f.home);
        assert_eq!(code, 0, "plain next in {name} must succeed: {err}");
        let v = json(&out);
        assert_ne!(
            v["title"], "First task",
            "plain next in {name} must not return the leased task: {out}"
        );
        assert_eq!(v["title"], "Second task", "in {name}: {out}");
    }
}

/// 3. A tracked row with `status = "claimed"` (what an OLD binary leaves) but
///    NO ledger lease is pending in the derived view: listed pending, and
///    claimable. The claim writes no NEW `claimed` bytes into the store.
#[test]
fn legacy_tracked_claimed_row_without_lease_is_pending() {
    let f = fixture_with("t3", |a, _home| {
        let dir = a.join(".backlog");
        std::fs::create_dir_all(&dir).unwrap();
        // updated_at = now: under the OLD model this is a LIVE claim (not
        // stale), so only the new derived view makes it claimable.
        let now = now_unix();
        std::fs::write(
            dir.join("tasks.toml"),
            format!(
                "[[task]]\nid = \"legacy01\"\ntitle = \"Legacy claimed\"\nproject = \"{}\"\n\
                 tags = [\"p0\"]\nstatus = \"claimed\"\nnotes = \"\"\ncreated_at = 1000\n\
                 updated_at = {now}\n",
                a.display()
            ),
        )
        .unwrap();
    });
    let before = f.store_bytes(&f.a);
    assert_eq!(count_claimed_bytes(&before), 1, "fixture sanity");
    assert!(
        f.ledger_files().is_empty(),
        "fixture: no ledger lease exists"
    );

    let rows = list_json(&f, &f.a);
    assert_eq!(
        status_of(&rows, "Legacy claimed"),
        "pending",
        "a tracked `claimed` row with no ledger lease must be listed as pending: {rows:?}"
    );

    let got = claim(&f, &f.a);
    assert_eq!(
        got["title"], "Legacy claimed",
        "the lease-less row must be claimable: {got}"
    );

    let after = f.store_bytes(&f.a);
    assert!(
        count_claimed_bytes(&after) <= count_claimed_bytes(&before),
        "the claim must not write NEW `claimed` status bytes into the tracked store:\n{after}"
    );
}

/// 4. An unreadable ledger makes every claim-aware READ undetermined: `list`
///    and plain `next` must exit non-zero with a reason, never exit 0
///    rendering the leased task as pending/unclaimed. (`next --claim` is also
///    checked; it already refused before this change.)
#[test]
fn unreadable_ledger_refuses_list_and_next() {
    let f = fixture("t4");
    claim(&f, &f.a);

    let ledgers = f.ledger_files();
    assert!(!ledgers.is_empty(), "a claim must create a ledger");
    for l in &ledgers {
        std::fs::write(l, "{ this is not json").unwrap();
    }

    for (label, args) in [
        ("list", vec!["list"]),
        ("list --json", vec!["list", "--json"]),
        ("next", vec!["next"]),
        ("next --claim", vec!["next", "--claim"]),
    ] {
        for (name, cwd) in [("A", &f.a), ("B", &f.b)] {
            let (code, out, err) = run(&args, cwd, &f.home);
            assert_ne!(
                code, 0,
                "`{label}` in {name} with an unreadable ledger must exit non-zero, \
                 got stdout={out} stderr={err}"
            );
            assert!(
                err.to_lowercase().contains("ledger"),
                "`{label}` in {name}: stderr must name the ledger as the reason, got: {err}"
            );
            assert!(
                !out.contains("First task"),
                "`{label}` in {name}: the leased task must not be rendered on a refusal: {out}"
            );
        }
    }
}

/// 5. A STALE lease (claimed_at older than CLAIM_STALE_SECS) stops excluding:
///    the task is claimable again from B, and no checkout lists it claimed.
#[test]
fn stale_lease_releases_the_task() {
    let f = fixture("t5");
    claim(&f, &f.a);

    let ledgers = f.ledger_files();
    assert_eq!(ledgers.len(), 1, "exactly one project ledger: {ledgers:?}");
    let mut v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&ledgers[0]).unwrap()).unwrap();
    let entries = v["entries"].as_array_mut().expect("ledger has entries");
    assert!(!entries.is_empty());
    for e in entries.iter_mut() {
        e["claimed_at"] = serde_json::json!(now_unix() - CLAIM_STALE_SECS - 400);
    }
    std::fs::write(&ledgers[0], serde_json::to_string_pretty(&v).unwrap()).unwrap();

    for (name, cwd) in [("A", &f.a), ("B", &f.b)] {
        let rows = list_json(&f, cwd);
        assert_eq!(
            status_of(&rows, "First task"),
            "pending",
            "a stale lease must not show as claimed in {name}: {rows:?}"
        );
    }

    let got = claim(&f, &f.b);
    assert_eq!(
        got["title"], "First task",
        "a stale lease must let B claim the task again: {got}"
    );
}

/// 6. SessionStart (which runs `requeue_expired`) must not dirty the tracked
///    store on account of claims: after a claim in A, the hook leaves A's
///    committed store bytes untouched and the worktree clean.
#[test]
fn session_start_does_not_dirty_the_store_for_claims() {
    let f = fixture("t6");
    let committed = f.store_bytes(&f.a);
    claim(&f, &f.a);

    let payload = serde_json::json!({
        "hook_event_name": "SessionStart",
        "session_id": "lease-t6",
        "cwd": f.a.to_str().unwrap(),
    })
    .to_string();
    let (code, _out, err) = run_with_stdin(&["session-start"], &f.a, &f.home, &payload);
    assert_eq!(code, 0, "session-start hook exits 0: {err}");

    assert_eq!(
        f.store_bytes(&f.a),
        committed,
        "after claim + SessionStart, A's tracked store must equal the committed bytes"
    );
    assert_eq!(
        f.porcelain(&f.a),
        "",
        "claim + SessionStart must leave A's worktree clean"
    );
}

/// 7. Adding an exact duplicate (title + project) of a LEASED task is still
///    rejected, from either checkout. (Characterization: expected to pass
///    before the change too — the row is pending in the store either way.)
#[test]
fn duplicate_add_of_a_leased_task_is_rejected() {
    let f = fixture("t7");
    claim(&f, &f.a);
    let project = f.a.to_str().unwrap().to_string();

    for (name, cwd) in [("A", &f.a), ("B", &f.b)] {
        let (code, out, err) = run(
            &["add", "--title", "First task", "--project", &project],
            cwd,
            &f.home,
        );
        assert_ne!(
            code, 0,
            "duplicate add of a leased task in {name} must be rejected: stdout={out} stderr={err}"
        );
        assert!(
            err.contains("duplicate"),
            "{name}: rejection must say duplicate: {err}"
        );
    }
}
