//! Store-divergence detection (backlog 5ba13c3e, completion criterion 2).
//!
//! The queue moved from a single global `~/.backlog/tasks.toml` to a per-repo
//! `<root>/.backlog/tasks.toml`. Nothing migrated the old file, so a reader
//! standing in a repo whose store had not been populated yet got a perfectly
//! ordinary `no tasks` / `[]` — byte-identical to a genuinely empty queue —
//! while hundreds of pending items sat in the legacy store. That is the
//! collapse CLAUDE.md §3 forbids: "I am looking at a different file" answered
//! as "there is nothing to do".
//!
//! What these tests pin, in both directions:
//!   * DETECTION — a resolved store that answers EMPTY while the legacy store
//!     holds pending work for the SAME project must not answer with an
//!     ordinary empty result. It exits non-zero with a diagnostic (the shape
//!     `main.rs` already uses for an undetermined project scope, and the shape
//!     `autoflow::backlog::find_open` reads as `Determination::Undetermined`
//!     rather than "no work").
//!   * NO FALSE POSITIVES — the controls below are the whole point. A test
//!     suite that only proves "it warns" is satisfied by an implementation
//!     that warns unconditionally, which would be worse than no detection at
//!     all: a fresh clone (no legacy store), a legacy store holding only OTHER
//!     projects' work, a store that is populated, and the legacy store being
//!     the resolved store itself must all stay silent and exit 0.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A unique, isolated temp HOME. Every test in this file writes a *fake*
/// `~/.backlog/tasks.toml`; the real user store must never be read or written,
/// so HOME is swapped for every single invocation (`harness_core::config::home`
/// prefers `$HOME`, which is what makes this work).
fn temp_home(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "backlog-divergence-home-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "backlog-divergence-dir-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// A real git repo (so `repo_root`/`canonical_project_id` resolve the same way
/// they do in production), returned as its CANONICAL path — the system temp
/// root is a symlink on macOS, and the store canonicalizes project labels, so
/// an uncanonicalized path here would make the test measure the symlink rather
/// than the divergence.
fn init_repo(tag: &str) -> PathBuf {
    assert!(
        git_available(),
        "git is required for store-divergence tests but is not available in PATH"
    );
    let repo = temp_dir(tag);
    assert!(Command::new("git")
        .args(["init", "-q"])
        .current_dir(&repo)
        .status()
        .unwrap()
        .success());
    std::fs::canonicalize(&repo).unwrap()
}

/// One `[[task]]` block, pending, scoped to `project`.
fn task_block(id: &str, title: &str, project: &Path, status: &str) -> String {
    format!(
        "[[task]]\nid = \"{id}\"\ntitle = \"{title}\"\nproject = \"{}\"\ntags = []\nstatus = \"{status}\"\nnotes = \"\"\ncreated_at = 1000\nupdated_at = 1000\n\n",
        project.display()
    )
}

fn write_store(dir: &Path, body: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("tasks.toml"), body).unwrap();
}

/// Run `backlog <args>` under an isolated HOME with an explicit cwd.
fn run_in(args: &[&str], home: &Path, cwd: &Path) -> (i32, String, String) {
    run_with_stdin(args, "", home, cwd)
}

fn run_with_stdin(args: &[&str], payload: &str, home: &Path, cwd: &Path) -> (i32, String, String) {
    use std::io::Write;
    let bin = env!("CARGO_BIN_EXE_backlog");
    let mut child = Command::new(bin)
        .args(args)
        .env("HOME", home)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary spawns");
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(payload.as_bytes());
    }
    let out = child.wait_with_output().expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The core detection, on the path a downstream reader actually uses.
///
/// The repo store is absent (a checkout that predates the migration), the
/// legacy store holds a pending task for THIS project. Answering `no tasks` /
/// `[]` here is what made 490 items invisible; the answer must instead be a
/// non-zero exit with the reason on stderr and nothing empty on stdout.
#[test]
fn empty_repo_store_with_pending_legacy_work_is_not_answered_as_empty() {
    let repo = init_repo("detect");
    let home = temp_home("detect");
    write_store(
        &home.join(".backlog"),
        &task_block("legacy01", "stranded p0", &repo, "pending"),
    );

    let (code, stdout, stderr) = run_in(&["list", "--json"], &home, &repo);

    assert_ne!(
        code, 0,
        "an empty answer that is really 'wrong store' must not exit 0 \
         (autoflow reads exit 0 + [] as 'no work'). stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stderr.contains(".backlog"),
        "the diagnostic must name the legacy store so the reader can act on it; stderr={stderr:?}"
    );
    assert!(
        !stdout.contains('['),
        "nothing resembling an empty result may reach stdout; stdout={stdout:?}"
    );
}

/// Same divergence, plain (non-JSON) `list`: `no tasks` is the human-facing
/// spelling of the same false answer.
#[test]
fn plain_list_does_not_print_no_tasks_when_the_legacy_store_holds_work() {
    let repo = init_repo("detect-plain");
    let home = temp_home("detect-plain");
    write_store(
        &home.join(".backlog"),
        &task_block("legacy02", "stranded", &repo, "pending"),
    );

    let (code, stdout, _stderr) = run_in(&["list"], &home, &repo);

    assert_ne!(code, 0, "stdout={stdout:?}");
    assert!(
        !stdout.contains("no tasks"),
        "'no tasks' is the false answer this ticket exists to remove; stdout={stdout:?}"
    );
}

/// `next` answers the same question ("is there work?") for drivers, so it must
/// not answer `no pending tasks` while the legacy store holds pending work.
#[test]
fn next_does_not_report_no_pending_tasks_when_the_legacy_store_holds_work() {
    let repo = init_repo("detect-next");
    let home = temp_home("detect-next");
    write_store(
        &home.join(".backlog"),
        &task_block("legacy03", "stranded", &repo, "pending"),
    );

    let (code, stdout, _stderr) = run_in(&["next"], &home, &repo);

    assert_ne!(code, 0, "stdout={stdout:?}");
    assert!(!stdout.contains("no pending tasks"), "stdout={stdout:?}");
}

/// An UNREADABLE legacy store is not an observation that the legacy store is
/// empty. With the resolved store also answering empty, nothing at all has
/// been established about whether work exists, so this must not exit 0 with an
/// empty result either (CLAUDE.md §3: undetermined resolves to the restrictive
/// side, never to "fine").
#[test]
fn an_unparseable_legacy_store_is_not_read_as_an_empty_legacy_store() {
    let repo = init_repo("unreadable");
    let home = temp_home("unreadable");
    write_store(&home.join(".backlog"), "this is not { valid toml [[[\n");

    let (code, stdout, stderr) = run_in(&["list", "--json"], &home, &repo);

    assert_ne!(
        code, 0,
        "an unreadable legacy store + an empty resolved store establishes nothing; \
         stdout={stdout:?} stderr={stderr:?}"
    );
}

// ---- controls: the detection must not fire -----------------------------------
//
// Each of these would pass against a "warn unconditionally" implementation's
// opposite, so they are what makes the tests above mean something.

/// A FRESH CLONE on a machine that never had the legacy store. There is
/// nothing to diverge from, so an empty queue is a real observation and must
/// be reported exactly as before. This is the required no-false-positive case.
#[test]
fn no_legacy_store_at_all_reports_an_ordinary_empty_queue() {
    let repo = init_repo("fresh");
    let home = temp_home("fresh");
    assert!(!home.join(".backlog").exists());

    let (code, stdout, stderr) = run_in(&["list", "--json"], &home, &repo);

    assert_eq!(code, 0, "stderr={stderr:?}");
    assert_eq!(stdout.trim(), "[]");
    assert!(
        !stderr.to_lowercase().contains("legacy"),
        "a machine with no legacy store must produce no divergence diagnostic; stderr={stderr:?}"
    );
}

/// The legacy store exists but holds only ANOTHER project's work. This
/// checkout's empty queue is then a true observation — warning here would fire
/// in every repo on a machine that has any legacy store at all.
#[test]
fn a_legacy_store_holding_only_other_projects_is_silent() {
    let repo = init_repo("other-proj");
    let other = init_repo("other-proj-b");
    let home = temp_home("other-proj");
    write_store(
        &home.join(".backlog"),
        &task_block("legacy04", "someone else's", &other, "pending"),
    );

    let (code, stdout, stderr) = run_in(&["list", "--json"], &home, &repo);

    assert_eq!(code, 0, "stderr={stderr:?}");
    assert_eq!(stdout.trim(), "[]");
    assert!(
        !stderr.to_lowercase().contains("legacy"),
        "stderr={stderr:?}"
    );
}

/// The legacy store holds only NON-pending work for this project. Migration is
/// about work that is still queued; a done/failed remnant is not a reason to
/// refuse an empty answer.
#[test]
fn a_legacy_store_holding_only_done_work_is_silent() {
    let repo = init_repo("done-only");
    let home = temp_home("done-only");
    write_store(
        &home.join(".backlog"),
        &task_block("legacy05", "already finished", &repo, "done"),
    );

    let (code, stdout, stderr) = run_in(&["list", "--json"], &home, &repo);

    assert_eq!(code, 0, "stderr={stderr:?}");
    assert_eq!(stdout.trim(), "[]");
    assert!(
        !stderr.to_lowercase().contains("legacy"),
        "stderr={stderr:?}"
    );
}

/// Both stores populated: the migrated repo store answers with real work, so
/// the reader is NOT being handed a false empty. The detection must not
/// escalate here — a non-zero exit would erase real items from every consumer
/// that maps a non-zero exit to "none" (`overwatch`'s `shell_soft` →
/// `format_backlog(None)` → "(none)"), which is the very fault being fixed.
#[test]
fn a_populated_repo_store_still_exits_zero_and_lists_its_work() {
    let repo = init_repo("populated");
    let home = temp_home("populated");
    write_store(
        &home.join(".backlog"),
        &task_block("legacy06", "leftover", &repo, "pending"),
    );
    write_store(
        &repo.join(".backlog"),
        &task_block("repo01", "current work", &repo, "pending"),
    );

    let (code, stdout, stderr) = run_in(&["list", "--json"], &home, &repo);

    assert_eq!(code, 0, "stderr={stderr:?}");
    assert!(
        stdout.contains("repo01"),
        "the repo store's own work must still be listed; stdout={stdout:?}"
    );
    // Exit 0 is not silence: the listing IS incomplete, and saying so is the
    // difference between this and the pre-fix behaviour. Without this
    // assertion the test above would also pass against an implementation that
    // simply never noticed the legacy store.
    assert!(
        stderr.contains("legacy store") && stderr.contains("incomplete"),
        "the warn path must still name the legacy store; stderr={stderr:?}"
    );
}

/// The SessionStart hook has no exit code and no stderr the agent ever reads —
/// `additionalContext` is the only channel that reaches it, and injecting
/// NOTHING is exactly how a session was told "no queue" while the legacy store
/// held the work. So the notice must appear in the injected context, and the
/// hook must still exit 0 (its own invariant).
#[test]
fn session_start_injects_the_divergence_notice_when_the_queue_looks_empty() {
    let repo = init_repo("hook");
    let home = temp_home("hook");
    write_store(
        &home.join(".backlog"),
        &task_block("legacy08", "stranded", &repo, "pending"),
    );

    let payload = format!(
        r#"{{"session_id":"s","cwd":"{}","hook_event_name":"SessionStart"}}"#,
        repo.display()
    );
    let (code, stdout, _stderr) = run_with_stdin(&["session-start"], &payload, &home, &repo);

    assert_eq!(code, 0, "the hook must always exit 0; stdout={stdout:?}");
    assert!(
        stdout.contains("divergence"),
        "the empty injection is the silent failure this fixes; stdout={stdout:?}"
    );
}

/// Control for the hook: a machine with no legacy store injects nothing at
/// all, exactly as before. Without this, an implementation that always injects
/// a notice would satisfy the test above.
#[test]
fn session_start_injects_nothing_when_there_is_no_legacy_store() {
    let repo = init_repo("hook-clean");
    let home = temp_home("hook-clean");
    assert!(!home.join(".backlog").exists());

    let payload = format!(
        r#"{{"session_id":"s","cwd":"{}","hook_event_name":"SessionStart"}}"#,
        repo.display()
    );
    let (code, stdout, _stderr) = run_with_stdin(&["session-start"], &payload, &home, &repo);

    assert_eq!(code, 0);
    assert!(
        stdout.trim().is_empty(),
        "nothing to say ⇒ say nothing; stdout={stdout:?}"
    );
}

/// An operator can PIN `store_dir` at the legacy store, and then the resolved
/// store IS the legacy store. A store cannot diverge from itself, so comparing
/// the two would warn about exactly the tasks it just listed.
///
/// Re-pinned 2026-08-20: this used to reach the same short-circuit through a
/// cwd with no repo root above it, which resolved to `~/.backlog` by fallback.
/// That fallback is gone — such a cwd now refuses outright
/// (`tests/project_scope.rs::list_outside_any_repo_refuses_instead_of_reading_the_shared_store`)
/// — and the pin is both the remaining way to reach this case and the escape
/// hatch the refusal names. It is also strictly better as a fixture: a pinned
/// store wins regardless of cwd, so the old silent `return` on a machine whose
/// temp root happens to sit inside a repo (a case reporting green without ever
/// running) is gone with it.
#[test]
fn the_legacy_store_is_not_compared_against_itself() {
    let home = temp_home("self");
    let cwd = temp_dir("self-cwd");
    write_store(
        &home.join(".backlog"),
        &task_block("legacy07", "in the legacy store", &cwd, "pending"),
    );
    // `harness_core::config::base_dir("backlog")` is `$HOME/.backlog`, which is
    // what `divergence::legacy_path()` resolves to — so pinning here makes the
    // resolved store and the legacy store the same file, which is the whole
    // point of the case.
    std::fs::write(
        home.join(".backlog").join("config.toml"),
        format!(
            "store_dir = {:?}\n",
            home.join(".backlog").to_string_lossy()
        ),
    )
    .unwrap();

    let (code, stdout, stderr) = run_in(&["list", "--all", "--json"], &home, &cwd);

    assert_eq!(code, 0, "stderr={stderr:?}");
    // Anti-vacuity: the pin really took effect and this really is the legacy
    // file being read, not an empty store somewhere else.
    assert!(stdout.contains("legacy07"), "stdout={stdout:?}");
    assert!(
        !stderr.to_lowercase().contains("legacy store"),
        "the resolved store IS the legacy store here; stderr={stderr:?}"
    );
}
