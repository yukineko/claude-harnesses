//! Cross-checkout claim exclusion: two CHECKOUTS of the same project must not
//! be handed the same task (backlog 709ff549).
//!
//! The store follows the checkout on purpose (`config::locate`;
//! CLAUDE.md §8 forbids a worktree writing the main tree's tracked file), so
//! two checkouts of the same repo hold two `.backlog/tasks.toml` files that
//! diverge. The claim's mutual exclusion, however, was keyed on the RESOLVED
//! STORE PATH (`store::tasks_lock_path` = `<store>.lock`), so the two
//! checkouts took two disjoint locks over two disjoint files and both handed
//! out the SAME task id — the measured defect.
//!
//! These tests drive the real built binary in separate OS processes with a
//! REAL git worktree, because that is what two sessions actually are; the
//! in-process tests in `store.rs` all share one cwd and one store and so
//! cannot observe this at all. `HOME` is pinned to a temp dir in every
//! invocation so the machine-global ledger under `~/.backlog` is the test's
//! own, never the user's.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

fn unique_root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "backlog-xco-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    // macOS' temp dir is a symlink (/var -> /private/var). Canonicalize once,
    // here, so every path this test compares (project labels, ledger keys)
    // is the same string the binary itself resolves to.
    std::fs::canonicalize(&dir).unwrap()
}

fn spawn(args: &[&str], cwd: &Path, home: &Path) -> Child {
    Command::new(env!("CARGO_BIN_EXE_backlog"))
        .args(args)
        .env("HOME", home)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary spawns")
}

fn run(args: &[&str], cwd: &Path, home: &Path) -> (i32, String, String) {
    let out = spawn(args, cwd, home)
        .wait_with_output()
        .expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Run `git`, asserting it succeeded. A test that cannot build its own repo
/// must FAIL loudly, never quietly skip: a skipped exclusion test would be
/// indistinguishable from a passing one.
fn git(args: &[&str], cwd: &Path, home: &Path) {
    let out = Command::new("git")
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
        .expect("git is available");
    assert!(
        out.status.success(),
        "git {args:?} failed in {}: {}{}",
        cwd.display(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

struct Fixture {
    home: PathBuf,
    /// The main working tree (`.git` is a directory).
    repo: PathBuf,
    /// A REAL linked worktree of `repo` (`.git` is a file).
    wt: PathBuf,
}

impl Fixture {
    fn store(&self, checkout: &Path) -> PathBuf {
        checkout.join(".backlog").join("tasks.toml")
    }

    /// Every `*.json` under the machine-global claims dir.
    fn ledger_files(&self) -> Vec<PathBuf> {
        let dir = self.home.join(".backlog").join("claims");
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect()
    }
}

/// A main working tree + a real linked worktree of it, each holding its OWN
/// `.backlog/tasks.toml` with the SAME two task ids — the diverged state that
/// was actually measured on this machine (17 checkouts, 10 distinct store
/// sizes).
fn fixture(tag: &str) -> Fixture {
    let root = unique_root(tag);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();

    git(&["init", "-q"], &repo, &home);
    git(
        &["commit", "-q", "--allow-empty", "-m", "init"],
        &repo,
        &home,
    );
    let wt = root.join("wt");
    git(
        &["worktree", "add", "-q", "-b", "side", wt.to_str().unwrap()],
        &repo,
        &home,
    );
    assert!(
        wt.join(".git").is_file(),
        "the linked worktree's .git must be a FILE (that is the case under test)"
    );

    let project = repo.to_str().unwrap().to_string();
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
            &repo,
            &home,
        );
        assert_eq!(code, 0, "add must succeed: {err}");
    }

    // The divergence itself: the worktree's own store starts as a COPY of the
    // main tree's, so both checkouts hold the same ids (what a merge, or a
    // `git worktree add` from a commit that tracked `.backlog/`, produces).
    let src = repo.join(".backlog").join("tasks.toml");
    let dst_dir = wt.join(".backlog");
    std::fs::create_dir_all(&dst_dir).unwrap();
    std::fs::copy(&src, dst_dir.join("tasks.toml")).unwrap();

    Fixture { home, repo, wt }
}

fn json(stdout: &str) -> serde_json::Value {
    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("expected JSON ({e}), got: {stdout}"))
}

/// The id of a claim result, or `None` when the process claimed nothing.
fn claimed_id(code: i32, stdout: &str) -> Option<String> {
    if code != 0 || stdout.trim().is_empty() || stdout.contains("no pending tasks") {
        return None;
    }
    Some(json(stdout)["id"].as_str().unwrap().to_string())
}

/// THE EXCLUSION TEST. Two real processes, two real checkouts of ONE project,
/// each with its own diverged store holding the same ids: at most one of them
/// may be handed any given task.
#[test]
fn two_checkouts_of_the_same_project_never_claim_the_same_task() {
    let f = fixture("exclusion");

    let a = spawn(&["next", "--claim"], &f.repo, &f.home);
    let b = spawn(&["next", "--claim"], &f.wt, &f.home);
    let a = a.wait_with_output().unwrap();
    let b = b.wait_with_output().unwrap();

    let a_code = a.status.code().unwrap_or(-1);
    let b_code = b.status.code().unwrap_or(-1);
    let a_out = String::from_utf8_lossy(&a.stdout).into_owned();
    let b_out = String::from_utf8_lossy(&b.stdout).into_owned();
    let a_err = String::from_utf8_lossy(&a.stderr).into_owned();
    let b_err = String::from_utf8_lossy(&b.stderr).into_owned();

    let a_id = claimed_id(a_code, &a_out);
    let b_id = claimed_id(b_code, &b_out);

    // At most one claim of any given task is the invariant; either process
    // claiming nothing is fine here (the anti-vacuity control below is what
    // forbids an implementation that always refuses).
    if let (Some(x), Some(y)) = (&a_id, &b_id) {
        assert_ne!(
            x, y,
            "two checkouts of the SAME project were both handed task {x}\n\
             main tree: exit {a_code} stdout={a_out} stderr={a_err}\n\
             worktree : exit {b_code} stdout={b_out} stderr={b_err}"
        );
    }

    // The claim must also be RECORDED somewhere both checkouts can see, or the
    // exclusion above held only by luck of timing.
    assert!(
        !f.ledger_files().is_empty(),
        "a machine-global claim ledger must exist under {}/.backlog/claims after a claim\n\
         main tree: exit {a_code} stdout={a_out} stderr={a_err}\n\
         worktree : exit {b_code} stdout={b_out} stderr={b_err}",
        f.home.display()
    );
}

/// ANTI-VACUITY for the test above: an implementation that refuses every claim
/// would satisfy "at most one". A single claim in one checkout MUST return the
/// task.
#[test]
fn a_single_claim_in_one_checkout_still_returns_the_task() {
    let f = fixture("antivacuity");

    let (code, out, err) = run(&["next", "--claim"], &f.repo, &f.home);
    assert_eq!(code, 0, "a lone claim must succeed: stderr={err}");
    assert!(
        !out.contains("no pending tasks"),
        "a lone claim must be handed the pending p0 task, got: {out}"
    );
    let v = json(&out);
    assert_eq!(v["title"], "First task", "the p0 task must win: {out}");
    assert_eq!(v["status"], "claimed");
    assert!(
        f.store(&f.repo).exists(),
        "the claim must still be persisted in this checkout's own store"
    );

    // ...and from the OTHER checkout too, when it is the only claimer.
    let f2 = fixture("antivacuity-wt");
    let (code, out, err) = run(&["next", "--claim"], &f2.wt, &f2.home);
    assert_eq!(
        code, 0,
        "a lone claim from the worktree must succeed: {err}"
    );
    assert_eq!(json(&out)["title"], "First task", "got: {out}");
}

/// UNDETERMINED REFUSES: an unreadable ledger is not an empty ledger. The
/// claim must exit non-zero, name the reason on stderr, and must NOT print an
/// empty-queue answer (a driver reads that as "there is no work").
#[test]
fn an_unreadable_ledger_refuses_the_claim_instead_of_answering_empty() {
    let f = fixture("unreadable");

    let (code, _, err) = run(&["next", "--claim"], &f.repo, &f.home);
    assert_eq!(code, 0, "the first claim must succeed: {err}");

    let ledgers = f.ledger_files();
    assert!(
        !ledgers.is_empty(),
        "expected a claim ledger under {}/.backlog/claims",
        f.home.display()
    );
    for l in &ledgers {
        std::fs::write(l, "{ this is not json").unwrap();
    }

    // A second task is still pending, so a fail-open would happily hand it out
    // (or answer "no pending tasks"); neither is acceptable while the ledger —
    // the only cross-checkout record of what is already claimed — is unreadable.
    let (code, out, err) = run(&["next", "--claim"], &f.repo, &f.home);
    assert_ne!(
        code, 0,
        "an unreadable ledger must refuse the claim (non-zero exit), got stdout={out} stderr={err}"
    );
    assert!(
        err.to_lowercase().contains("ledger"),
        "stderr must name the reason (the ledger), got: {err}"
    );
    assert!(
        !out.contains("no pending tasks"),
        "a refusal must never be rendered as an empty queue, got: {out}"
    );
    assert!(
        out.trim().is_empty(),
        "nothing claimable may reach stdout on a refusal, got: {out}"
    );
}

/// One `[[task]]` block for a hand-written legacy store.
fn task_block(id: &str, title: &str, project: &Path, status: &str) -> String {
    format!(
        "[[task]]\nid = \"{id}\"\ntitle = \"{title}\"\nproject = \"{}\"\ntags = []\n\
         status = \"{status}\"\nnotes = \"\"\ncreated_at = 1000\nupdated_at = 1000\n\n",
        project.display()
    )
}

/// The CLAIM-side answer to store divergence, pinned deliberately (backlog
/// 709ff549 asked which of `Warn`/`Undetermined` is right for `--claim`
/// specifically, since it may differ from `list`).
///
/// **This is a characterization test, not an F→P oracle**: it passed before
/// this change too. It exists so the decision is a test rather than prose.
///
/// The decision: `Warn` stays on exit 0 for `--claim`, exactly as for `list`.
/// When the resolved store DOES hold queued work, the claim hands out a real,
/// existing task; refusing there would starve every driver on any machine that
/// still has legacy leftovers (permanently — this machine has them), which
/// pushes people to bypass the gate rather than protecting anything. The
/// warning is stderr-only and is NOT reflected in the verdict; what protects
/// the expensive case is the OTHER branch —
/// [`an_undetermined_divergence_refuses_the_claim`] — where the resolved store
/// is empty and an empty answer would be indistinguishable from a real one.
/// Cross-checkout exclusion is not the divergence check's job at all; it is
/// the claim ledger's.
#[test]
fn a_warn_level_divergence_still_lets_the_claim_through() {
    let f = fixture("warn-divergence");
    // Legacy store holds queued work for the SAME project, while the resolved
    // store also holds work → `Divergence::Warn`.
    let legacy = f.home.join(".backlog");
    std::fs::create_dir_all(&legacy).unwrap();
    std::fs::write(
        legacy.join("tasks.toml"),
        task_block("legacy01", "stranded", &f.repo, "pending"),
    )
    .unwrap();

    let (code, out, err) = run(&["next", "--claim"], &f.repo, &f.home);
    assert_eq!(code, 0, "a Warn-level divergence must not refuse: {err}");
    assert!(
        err.contains("legacy store"),
        "the incompleteness must still be reported on stderr: {err}"
    );
    assert_eq!(json(&out)["status"], "claimed", "got: {out}");
}

/// The other branch: the resolved store holds NO queued work while the legacy
/// store does. An empty answer here is indistinguishable from a genuinely
/// empty queue, so `--claim` must refuse (non-zero exit, nothing claimable on
/// stdout) rather than say `no pending tasks`. Also a characterization test —
/// `guard_store_divergence` already did this for `next`/`next --claim`; this
/// pins that the claim path did not lose it.
#[test]
fn an_undetermined_divergence_refuses_the_claim() {
    let f = fixture("undetermined-divergence");
    // Drain the resolved store so it holds nothing queued.
    for _ in 0..2 {
        let (code, _, err) = run(&["next", "--claim"], &f.repo, &f.home);
        assert_eq!(code, 0, "draining claim must succeed: {err}");
    }
    let legacy = f.home.join(".backlog");
    std::fs::create_dir_all(&legacy).unwrap();
    std::fs::write(
        legacy.join("tasks.toml"),
        task_block("legacy02", "stranded", &f.repo, "pending"),
    )
    .unwrap();

    let (code, out, err) = run(&["next", "--claim"], &f.repo, &f.home);
    assert_ne!(
        code, 0,
        "an empty resolved store + queued legacy work must refuse: stdout={out} stderr={err}"
    );
    assert!(
        !out.contains("no pending tasks"),
        "the refusal must not be rendered as an empty queue: {out}"
    );
}

/// ANTI-VACUITY for the test above: with a READABLE ledger the very same
/// second invocation succeeds and is handed the second task.
#[test]
fn a_readable_ledger_lets_the_second_claim_through() {
    let f = fixture("readable");

    let (code, out, err) = run(&["next", "--claim"], &f.repo, &f.home);
    assert_eq!(code, 0, "the first claim must succeed: {err}");
    let first = json(&out)["id"].as_str().unwrap().to_string();

    let (code, out, err) = run(&["next", "--claim"], &f.repo, &f.home);
    assert_eq!(
        code, 0,
        "a readable ledger must not block the claim: stderr={err}"
    );
    assert!(
        !out.contains("no pending tasks"),
        "the second task is pending and must be handed out: {out}"
    );
    let second = json(&out)["id"].as_str().unwrap().to_string();
    assert_ne!(first, second, "the second claim must be a different task");
}
