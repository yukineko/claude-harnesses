// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Black-box regression tests for `docs/audit-reviewgate-verdict-paths.md`
//! §1 P2..P8 — 8 permissive verdict-path fail-opens found by that read-only
//! audit and **not yet fixed** as of this file's authorship.
//!
//! Each test encodes the CONTRACT the audit's "是正の方向性" section describes,
//! not the crate's current behaviour. Tests pinned to a P-item are therefore
//! **expected to FAIL (RED) on today's code**; that is the point — they are
//! the reproduction proof a fix should turn GREEN. Controls are expected to
//! PASS already; they pin behaviour the fix must not regress.
//!
//! This file does not modify `crates/reviewgate/src/`. It drives the built
//! binary as a black box (`env!("CARGO_BIN_EXE_reviewgate")`), using an
//! isolated `$HOME` per test (never the real one) and, where a fake `git` is
//! needed, a `PATH` that resolves to a shim delegating to the REAL git for
//! everything except one deliberately-injected failure.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// Guards process-global `$HOME` mutation (only the P5 violation-store readback
/// needs it — `overwatch::store` resolves its root via `harness_core::config::
/// home()`, which reads the `HOME` env var of the CURRENT process, not a
/// child's). Scoped to this file/binary; integration test files are separate
/// binaries/processes so no cross-file coordination is needed, but `#[test]`s
/// within one binary run concurrently by default and must not race a shared
/// env var.
static HOME_ENV_LOCK: Mutex<()> = Mutex::new(());

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn unique_dir(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let p = std::env::temp_dir().join(format!(
        "reviewgate-p2p8-{}-{}-{}",
        tag,
        std::process::id(),
        n
    ));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).expect("create scratch dir");
    p
}

fn write_exec(path: &Path, contents: &str) {
    std::fs::write(path, contents).expect("write script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perm = std::fs::metadata(path).expect("stat script").permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(path, perm).expect("chmod script");
    }
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(dir)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("git runs");
    assert!(status.success(), "git {args:?} failed in {}", dir.display());
}

fn init_repo(root: &Path) {
    std::fs::create_dir_all(root).expect("mkdir repo");
    git(root, &["init", "-q"]);
    git(root, &["config", "user.email", "t@t.com"]);
    git(root, &["config", "user.name", "t"]);
}

/// Resolve the real `git` binary's absolute path (used by the fake-git shims
/// below so they can delegate everything except the one injected failure).
fn real_git_path() -> PathBuf {
    let out = Command::new("sh")
        .arg("-c")
        .arg("command -v git")
        .output()
        .expect("locate real git");
    let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert!(!p.is_empty(), "could not locate a real git on PATH");
    PathBuf::from(p)
}

/// Write a `git` shim onto `bin_dir` that delegates every invocation to the
/// real git EXCEPT one whose joined args start with `fail_prefix`, which it
/// fails with exit 1 (no stdout, no stderr) — reproducing a git subcommand
/// erroring out (spawn success, non-zero exit) rather than being unreachable.
fn write_selective_fail_git(bin_dir: &Path, fail_prefix: &str) -> PathBuf {
    std::fs::create_dir_all(bin_dir).expect("mkdir bin dir");
    let real = real_git_path();
    let script = bin_dir.join("git");
    write_exec(
        &script,
        &format!(
            "#!/bin/bash\ncase \"$*\" in\n  \"{prefix}\"*) exit 1 ;;\nesac\nexec \"{real}\" \"$@\"\n",
            prefix = fail_prefix,
            real = real.display(),
        ),
    );
    bin_dir.to_path_buf()
}

/// A PATH string with `bin_dir` prepended so a spawned child resolves `git`
/// to the shim before falling back to the real PATH for everything else the
/// shim's `exec` needs (e.g. `bash` itself).
fn path_with(bin_dir: &Path) -> std::ffi::OsString {
    let existing = std::env::var_os("PATH").unwrap_or_default();
    let mut joined = bin_dir.as_os_str().to_owned();
    joined.push(":");
    joined.push(existing);
    joined
}

fn stop_payload(session: &str, cwd: &Path) -> String {
    serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": session,
        "stop_hook_active": false,
        "cwd": cwd.to_string_lossy(),
        "transcript_path": "",
    })
    .to_string()
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

/// Run `reviewgate review` with the given payload, HOME, cwd and PATH.
fn run_review(home: &Path, cwd: &Path, path: &std::ffi::OsStr, payload: &str) -> Run {
    let bin = env!("CARGO_BIN_EXE_reviewgate");
    let mut cmd = Command::new(bin);
    cmd.arg("review")
        .current_dir(cwd)
        .env("HOME", home)
        .env("PATH", path)
        .env_remove("REVIEWGATE_DISABLE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("reviewgate spawns");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(payload.as_bytes())
        .expect("write payload");
    let out = child.wait_with_output().expect("reviewgate runs");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// Run `reviewgate status` with the given HOME/cwd.
fn run_status(home: &Path, cwd: &Path) -> Run {
    let bin = env!("CARGO_BIN_EXE_reviewgate");
    let out = Command::new(bin)
        .arg("status")
        .current_dir(cwd)
        .env("HOME", home)
        .env_remove("REVIEWGATE_DISABLE")
        .output()
        .expect("reviewgate status runs");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn is_block(stdout: &str) -> bool {
    stdout.contains("\"decision\":\"block\"") || stdout.contains("\"decision\": \"block\"")
}

/// The most recent `log.jsonl` line for `session`: `(verdict, ...)`.
fn last_log_verdict(home: &Path, session: &str) -> String {
    let path = home.join(".reviewgate/state/log.jsonl");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let line = text
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .rfind(|v| v.get("session").and_then(|s| s.as_str()) == Some(session))
        .unwrap_or_else(|| panic!("no log line for session {session} in:\n{text}"));
    line.get("verdict")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn write_home_config(home: &Path, toml_body: &str) {
    let dir = home.join(".reviewgate");
    std::fs::create_dir_all(&dir).expect("mkdir .reviewgate");
    std::fs::write(dir.join("config.toml"), toml_body).expect("write home config");
}

// ═══════════════════════════════════════════════════════════════════════════
// P2 — partial content-fetch failure certifies an incomplete diff as reviewed.
// ═══════════════════════════════════════════════════════════════════════════

/// Shared P2 fixture: a repo with one committed-then-modified tracked file
/// (`a.rs`) and one untracked reviewable file (`b.rs`) carrying a unique
/// marker, reviewed in subprocess mode by a reviewer that saves the prompt it
/// received and reports clean.
struct P2Fixture {
    home: PathBuf,
    repo: PathBuf,
    prompt_file: PathBuf,
}

impl P2Fixture {
    fn new(tag: &str) -> P2Fixture {
        let root = unique_dir(tag);
        let home = root.join("home");
        std::fs::create_dir_all(&home).expect("mkdir home");
        let repo = root.join("repo");
        init_repo(&repo);
        std::fs::write(repo.join("a.rs"), "pub fn a() -> u8 { 1 }\n").expect("write a.rs");
        git(&repo, &["add", "a.rs"]);
        git(
            &repo,
            &["-c", "commit.gpgsign=false", "commit", "-qm", "init"],
        );
        // Now modify the tracked file (unstaged diff) and add an untracked one.
        std::fs::write(repo.join("a.rs"), "pub fn a() -> u8 { 2 }\n").expect("modify a.rs");
        std::fs::write(
            repo.join("b.rs"),
            "// NEVER REVIEWED MARKER\npub fn b() {}\n",
        )
        .expect("write b.rs");

        let prompt_file = root.join("prompt.txt");
        let reviewer = root.join("reviewer.sh");
        write_exec(
            &reviewer,
            &format!(
                "#!/bin/bash\ncat > \"{prompt}\"\necho LGTM\nexit 0\n",
                prompt = prompt_file.display(),
            ),
        );
        write_home_config(
            &home,
            &format!(
                "mode = \"subprocess\"\nreviewer_cmd = \"{cmd}\"\n",
                cmd = reviewer.display(),
            ),
        );

        P2Fixture {
            home,
            repo,
            prompt_file,
        }
    }

    fn run(&self, session: &str, path: &std::ffi::OsStr) -> Run {
        run_review(
            &self.home,
            &self.repo,
            path,
            &stop_payload(session, &self.repo),
        )
    }
}

/// P2 (variant A): only the untracked-content fetch
/// (`ls-files --others --exclude-standard -- <files>`) fails. `changed_files`
/// still reports BOTH files as changed (its own `ls-files` call has no `--`
/// suffix and is untouched), so the diff handed to the reviewer is silently
/// missing `b.rs`'s content — yet nothing downstream can tell.
///
/// Contract (audit §1 P2): the stop must BLOCK, and the recorded verdict must
/// be neither "clean" (a partial fetch certified as reviewed) nor
/// "empty-diff" (P1's sibling collapse). EXPECTED RED on unfixed code —
/// probe I in the audit observed exactly `verdict":"clean"` here.
#[test]
fn partial_untracked_fetch_failure_does_not_certify_the_diff_as_clean() {
    if !git_available() {
        eprintln!("skipping: git not available");
        return;
    }
    let f = P2Fixture::new("p2-ls-files");
    let bin_dir = f.repo.parent().unwrap().join("fakebin-lsfiles");
    write_selective_fail_git(&bin_dir, "ls-files --others --exclude-standard -- ");
    let path = path_with(&bin_dir);

    let run = f.run("s-p2-lsfiles", &path);

    assert_eq!(run.code, 0, "the Stop hook must always exit 0");
    assert!(
        is_block(&run.stdout),
        "a diff assembled from a PARTIALLY failed git fetch is not a reviewed \
         diff; it must block. stdout: {:?}",
        run.stdout
    );
    let verdict = last_log_verdict(&f.home, "s-p2-lsfiles");
    assert_ne!(
        verdict, "clean",
        "the untracked file's content fetch failed silently; a 'clean' \
         verdict here means an incomplete diff was certified as reviewed \
         (audit P2)"
    );
    assert_ne!(
        verdict, "empty-diff",
        "changed_files() still reports 2 files; collapsing a partial-fetch \
         failure into 'nothing changed' is the P1 sibling of this bug"
    );
    // Apparatus check: if the reviewer subprocess ran at all (non-"clean"
    // verdicts that still invoke it, e.g. a future "partial-fetch" tag),
    // confirm what actually reached it was missing b.rs's content — this is
    // the concrete mechanism P2 describes, not just its downstream tag.
    if let Ok(prompt) = std::fs::read_to_string(&f.prompt_file) {
        assert!(
            !prompt.contains("NEVER REVIEWED MARKER"),
            "apparatus sanity: the injected failure must actually have kept \
             b.rs's content out of what the reviewer saw, or this test is not \
             exercising the fetch failure it claims to. prompt: {prompt}"
        );
    }
}

/// P2 (variant B): only the unstaged content diff (`diff -- <files>`, not
/// `--cached`/`--name-only`) fails. Same contract as variant A, different
/// git subcommand.
#[test]
fn partial_unstaged_diff_failure_does_not_certify_the_diff_as_clean() {
    if !git_available() {
        eprintln!("skipping: git not available");
        return;
    }
    let f = P2Fixture::new("p2-diff");
    let bin_dir = f.repo.parent().unwrap().join("fakebin-diff");
    write_selective_fail_git(&bin_dir, "diff -- ");
    let path = path_with(&bin_dir);

    let run = f.run("s-p2-diff", &path);

    assert_eq!(run.code, 0, "the Stop hook must always exit 0");
    assert!(
        is_block(&run.stdout),
        "a diff assembled from a PARTIALLY failed git fetch is not a reviewed \
         diff; it must block. stdout: {:?}",
        run.stdout
    );
    let verdict = last_log_verdict(&f.home, "s-p2-diff");
    assert_ne!(
        verdict, "clean",
        "a.rs's unstaged content diff failed silently; a 'clean' verdict here \
         means an incomplete diff was certified as reviewed (audit P2)"
    );
    assert_ne!(
        verdict, "empty-diff",
        "changed_files() still reports 2 files; collapsing a partial-fetch \
         failure into 'nothing changed' is the P1 sibling of this bug"
    );
    // Apparatus check: same rationale as the ls-files variant above — confirm
    // the mechanism, not just the downstream tag.
    if let Ok(prompt) = std::fs::read_to_string(&f.prompt_file) {
        assert!(
            !prompt.contains("pub fn a() -> u8 { 2 }"),
            "apparatus sanity: the injected failure must actually have kept \
             a.rs's unstaged hunk out of what the reviewer saw, or this test \
             is not exercising the fetch failure it claims to. prompt: {prompt}"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// P3 — non-UTF-8 reviewer stdout silently loses real findings into Clean.
// ═══════════════════════════════════════════════════════════════════════════

/// The reviewer writes real findings preceded by invalid UTF-8 bytes and exits
/// 0. `read_to_string`'s `Err` is currently swallowed (`let _ = ...`), leaving
/// `out` empty, so `classify("")` returns `Clean` — the findings vanish.
///
/// Contract (audit §1 P3): must BLOCK, with verdict "reviewer-unavailable"
/// (the crate's existing tag for "the reviewer could not run to a
/// conclusion"). EXPECTED RED — probe B in the audit observed
/// `"verdict":"clean"`.
#[test]
fn non_utf8_reviewer_stdout_is_undetermined_not_clean() {
    if !git_available() {
        eprintln!("skipping: git not available");
        return;
    }
    let root = unique_dir("p3");
    let home = root.join("home");
    std::fs::create_dir_all(&home).expect("mkdir home");
    let repo = root.join("repo");
    init_repo(&repo);
    std::fs::write(repo.join("a.rs"), "pub fn a() -> u8 { 1 }\n").expect("write a.rs");

    let reviewer = root.join("reviewer.sh");
    write_exec(
        &reviewer,
        "#!/bin/bash\nprintf '\\xff\\xfe'\nprintf -- '- high: real bug in a.rs:1\\n'\nexit 0\n",
    );
    write_home_config(
        &home,
        &format!(
            "mode = \"subprocess\"\nreviewer_cmd = \"{cmd}\"\n",
            cmd = reviewer.display(),
        ),
    );
    let path = std::env::var_os("PATH").unwrap_or_default();

    let run = run_review(&home, &repo, &path, &stop_payload("s-p3", &repo));

    assert_eq!(run.code, 0, "the Stop hook must always exit 0");
    assert!(
        is_block(&run.stdout),
        "a reviewer whose real findings could not be read back is NOT a clean \
         review. stdout: {:?}",
        run.stdout
    );
    assert_eq!(
        last_log_verdict(&home, "s-p3"),
        "reviewer-unavailable",
        "non-UTF-8 stdout means the reviewer's output could not be read; that \
         is the same undetermined-reviewer case already tagged \
         'reviewer-unavailable' for spawn/timeout/non-zero-exit failures"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// P4 — "exit 0, empty stdout" silently becomes Clean (empty-set fail-open).
// ═══════════════════════════════════════════════════════════════════════════

/// The reviewer never reads stdin, prints nothing, exits 0. The prompt asks
/// for an explicit `LGTM` token on a clean review; empty output is not that
/// token, and the crate's own doc requires it. `classify("")` nonetheless
/// returns `Clean`.
///
/// Contract (audit §1 P4): must BLOCK, verdict "reviewer-unavailable".
/// EXPECTED RED — probe F observed `"verdict":"clean"`.
#[test]
fn silent_reviewer_exit_zero_empty_stdout_is_undetermined_not_clean() {
    if !git_available() {
        eprintln!("skipping: git not available");
        return;
    }
    let root = unique_dir("p4");
    let home = root.join("home");
    std::fs::create_dir_all(&home).expect("mkdir home");
    let repo = root.join("repo");
    init_repo(&repo);
    std::fs::write(repo.join("a.rs"), "pub fn a() -> u8 { 1 }\n").expect("write a.rs");

    let reviewer = root.join("reviewer.sh");
    write_exec(&reviewer, "#!/bin/bash\nexit 0\n");
    write_home_config(
        &home,
        &format!(
            "mode = \"subprocess\"\nreviewer_cmd = \"{cmd}\"\n",
            cmd = reviewer.display(),
        ),
    );
    let path = std::env::var_os("PATH").unwrap_or_default();

    let run = run_review(&home, &repo, &path, &stop_payload("s-p4", &repo));

    assert_eq!(run.code, 0, "the Stop hook must always exit 0");
    assert!(
        is_block(&run.stdout),
        "a reviewer that never even looked at the diff is NOT a clean review. \
         stdout: {:?}",
        run.stdout
    );
    assert_eq!(
        last_log_verdict(&home, "s-p4"),
        "reviewer-unavailable",
        "exit 0 with empty stdout is not the contractual 'LGTM' clean signal; \
         it must be treated the same as any other undetermined reviewer"
    );
}

/// PASSING CONTROL — a reviewer that actually runs and reports `LGTM` really
/// is clean. Pins that the P3/P4 fix does not turn a genuine clean review into
/// a block.
#[test]
fn reviewer_that_prints_lgtm_is_allowed_and_clean() {
    if !git_available() {
        eprintln!("skipping: git not available");
        return;
    }
    let root = unique_dir("p4-control");
    let home = root.join("home");
    std::fs::create_dir_all(&home).expect("mkdir home");
    let repo = root.join("repo");
    init_repo(&repo);
    std::fs::write(repo.join("a.rs"), "pub fn a() -> u8 { 1 }\n").expect("write a.rs");

    let reviewer = root.join("reviewer.sh");
    write_exec(
        &reviewer,
        "#!/bin/bash\ncat >/dev/null\necho LGTM\nexit 0\n",
    );
    write_home_config(
        &home,
        &format!(
            "mode = \"subprocess\"\nreviewer_cmd = \"{cmd}\"\n",
            cmd = reviewer.display(),
        ),
    );
    let path = std::env::var_os("PATH").unwrap_or_default();

    let run = run_review(&home, &repo, &path, &stop_payload("s-p4-control", &repo));

    assert_eq!(run.code, 0);
    assert!(
        !is_block(&run.stdout),
        "a real LGTM must allow the stop. stdout: {:?}",
        run.stdout
    );
    assert_eq!(last_log_verdict(&home, "s-p4-control"), "clean");
}

// ═══════════════════════════════════════════════════════════════════════════
// P5 — the Violation-giveup path allows a KNOWN violation silently.
// ═══════════════════════════════════════════════════════════════════════════

struct P5Fixture {
    home: PathBuf,
    repo: PathBuf,
    session: &'static str,
}

impl P5Fixture {
    fn subprocess(tag: &str, session: &'static str) -> P5Fixture {
        let root = unique_dir(tag);
        let home = root.join("home");
        std::fs::create_dir_all(&home).expect("mkdir home");
        let repo = root.join("repo");
        init_repo(&repo);
        std::fs::write(repo.join("a.rs"), "pub fn a() -> u8 { 0 }\n").expect("write a.rs");
        git(&repo, &["add", "a.rs"]);
        git(
            &repo,
            &["-c", "commit.gpgsign=false", "commit", "-qm", "init"],
        );

        let reviewer = root.join("reviewer.sh");
        write_exec(
            &reviewer,
            "#!/bin/bash\ncat >/dev/null\necho '- high: bug in a.rs:1'\nexit 0\n",
        );
        write_home_config(
            &home,
            &format!(
                "mode = \"subprocess\"\nmax_attempts = 2\nreviewer_cmd = \"{cmd}\"\n",
                cmd = reviewer.display(),
            ),
        );
        P5Fixture {
            home,
            repo,
            session,
        }
    }

    fn inject(tag: &str, session: &'static str) -> P5Fixture {
        let root = unique_dir(tag);
        let home = root.join("home");
        std::fs::create_dir_all(&home).expect("mkdir home");
        let repo = root.join("repo");
        init_repo(&repo);
        std::fs::write(repo.join("a.rs"), "pub fn a() -> u8 { 0 }\n").expect("write a.rs");
        git(&repo, &["add", "a.rs"]);
        git(
            &repo,
            &["-c", "commit.gpgsign=false", "commit", "-qm", "init"],
        );

        write_home_config(&home, "mode = \"inject\"\nmax_attempts = 2\n");
        P5Fixture {
            home,
            repo,
            session,
        }
    }

    /// Run one stop, changing `a.rs`'s content first so the diff (and hash)
    /// differs from the previous round — otherwise `already-reviewed` would
    /// short-circuit before `max_attempts` is ever exercised.
    fn stop(&self, round: u32) -> Run {
        std::fs::write(
            self.repo.join("a.rs"),
            format!("pub fn a() -> u8 {{ {round} }}\n"),
        )
        .expect("modify a.rs");
        let path = std::env::var_os("PATH").unwrap_or_default();
        run_review(
            &self.home,
            &self.repo,
            &path,
            &stop_payload(self.session, &self.repo),
        )
    }
}

/// Subprocess mode: a reviewer that reports a REAL finding every round, run 3
/// times with `max_attempts = 2`. Rounds 1-2 must still block (bounded
/// escalation, unchanged). Round 3 crosses the cap and gives up — but per the
/// audit this giveup is currently silent, shares the inject-mode "giveup" tag,
/// and never reaches the overwatch violation stream, so a KNOWN violation
/// becomes invisible to every downstream consumer of that signal.
///
/// Contract (audit §1 P5): round 3 must still exit 0 and allow (no `block` in
/// stdout — the escape hatch itself is legitimate, D not P), but:
///   * stderr must contain "WARNING" (loud, like the other 3 giveup sites);
///   * the log verdict must be "review-giveup", distinct from inject mode's
///     "giveup";
///   * an overwatch violation event for this stop must exist whose
///     signature/check_kind names "review-giveup".
///
/// EXPECTED RED on unfixed code (probe C2 observed empty stderr, verdict
/// "giveup", and no violation event at all).
#[test]
fn violation_giveup_is_loud_distinctly_tagged_and_recorded_as_a_violation() {
    if !git_available() {
        eprintln!("skipping: git not available");
        return;
    }
    let _guard = HOME_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let f = P5Fixture::subprocess("p5-subprocess", "s-p5-sub");

    let r1 = f.stop(1);
    assert!(
        is_block(&r1.stdout),
        "round 1 must still block: {:?}",
        r1.stdout
    );
    let r2 = f.stop(2);
    assert!(
        is_block(&r2.stdout),
        "round 2 must still block: {:?}",
        r2.stdout
    );
    let r3 = f.stop(3);

    assert_eq!(r3.code, 0, "the Stop hook must always exit 0");
    assert!(
        !is_block(&r3.stdout),
        "round 3 crosses max_attempts and must give up (allow), not block \
         forever: {:?}",
        r3.stdout
    );
    assert!(
        r3.stderr.contains("WARNING"),
        "giving up on a KNOWN reviewer-reported violation must warn loudly, \
         exactly like the other 3 giveup sites (reviewer-error / truncated / \
         git-scan-failed) already do. stderr: {:?}",
        r3.stderr
    );
    let verdict = last_log_verdict(&f.home, "s-p5-sub");
    assert_eq!(
        verdict, "review-giveup",
        "a Violation-giveup must use a tag DISTINCT from inject mode's \
         'giveup' (the crate's own convention: giveup sites get a distinct \
         tag); reusing the same literal makes the two indistinguishable in \
         the log"
    );

    let prev_home = std::env::var_os("HOME");
    std::env::set_var("HOME", &f.home);
    let events = overwatch::store::scan_violations(&f.repo).events_or_empty();
    match prev_home {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
    let events = events.expect("read violations store");
    assert!(
        events.iter().any(|e| e.signature.contains("review-giveup")),
        "a KNOWN, reviewer-reported violation that the gate chose to let \
         through must still be recorded in the overwatch violation stream \
         (the 'real audit source' per benchkit::auditsample) under a \
         signature naming review-giveup, or it is permanently invisible to \
         every downstream consumer of that stream. events: {:?}",
        events.iter().map(|e| &e.signature).collect::<Vec<_>>()
    );
}

/// PASSING CONTROL — inject mode's giveup is UNCHANGED: tag stays "giveup".
/// The audit explicitly scopes the "bounded allow itself is fine" ruling
/// (D, not P) to inject mode, whose escape is genuinely "the diff kept
/// changing" rather than "a known violation was let through". This pins that
/// the P5 fix does not rename inject mode's tag out from under it.
#[test]
fn inject_giveup_keeps_the_plain_giveup_tag() {
    if !git_available() {
        eprintln!("skipping: git not available");
        return;
    }
    let f = P5Fixture::inject("p5-inject", "s-p5-inj");

    let r1 = f.stop(1);
    assert!(
        is_block(&r1.stdout),
        "round 1 must still block: {:?}",
        r1.stdout
    );
    let r2 = f.stop(2);
    assert!(
        is_block(&r2.stdout),
        "round 2 must still block: {:?}",
        r2.stdout
    );
    let r3 = f.stop(3);

    assert_eq!(r3.code, 0);
    assert!(
        !is_block(&r3.stdout),
        "round 3 must give up: {:?}",
        r3.stdout
    );
    assert_eq!(
        last_log_verdict(&f.home, "s-p5-inj"),
        "giveup",
        "inject-mode giveup ('the diff kept changing', no known violation) \
         must keep its existing tag"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// P6 — a partially-malformed `include` glob silently narrows the scope.
// ═══════════════════════════════════════════════════════════════════════════

/// One valid pattern (`**/*.md`) plus one malformed pattern (`**/*.rs{`,
/// unbalanced glob brace). `build_set` drops the unparseable pattern with no
/// diagnostic, so `a.rs` silently falls out of scope even though the operator
/// clearly intended `.rs` files to be reviewed.
///
/// Contract (audit §1 P6): the stop must still BLOCK (a partially malformed
/// include list must not silently narrow the review scope down to only the
/// patterns that happened to parse), and stderr must name the malformed
/// pattern. EXPECTED RED — probe H2 observed an unblocked, undiagnosed allow.
#[test]
fn partially_malformed_include_glob_does_not_silently_narrow_scope() {
    if !git_available() {
        eprintln!("skipping: git not available");
        return;
    }
    let root = unique_dir("p6");
    let home = root.join("home");
    std::fs::create_dir_all(&home).expect("mkdir home");
    let repo = root.join("repo");
    init_repo(&repo);
    std::fs::write(repo.join("a.rs"), "pub fn a() -> u8 { 1 }\n").expect("write a.rs");

    write_home_config(
        &home,
        "mode = \"inject\"\ninclude = [\"**/*.rs{\", \"**/*.md\"]\n",
    );
    let path = std::env::var_os("PATH").unwrap_or_default();

    let run = run_review(&home, &repo, &path, &stop_payload("s-p6", &repo));

    assert_eq!(run.code, 0, "the Stop hook must always exit 0");
    assert!(
        is_block(&run.stdout),
        "a.rs is a real, uncommitted change; a malformed sibling pattern must \
         not silently drop it from the review scope. stdout: {:?}",
        run.stdout
    );
    assert!(
        run.stderr.contains("**/*.rs{"),
        "the malformed pattern must be NAMED in stderr so an operator can fix \
         it, rather than silently dropped. stderr: {:?}",
        run.stderr
    );
}

/// PASSING CONTROL — a fully valid `include` that legitimately excludes
/// `a.rs` (by only covering `.md`) allows, with the ordinary
/// "no-reviewable-changes" tag. Pins that the P6 fix does not start blocking
/// on a deliberate, well-formed scope.
#[test]
fn valid_include_that_excludes_the_change_allows_normally() {
    if !git_available() {
        eprintln!("skipping: git not available");
        return;
    }
    let root = unique_dir("p6-control");
    let home = root.join("home");
    std::fs::create_dir_all(&home).expect("mkdir home");
    let repo = root.join("repo");
    init_repo(&repo);
    std::fs::write(repo.join("a.rs"), "pub fn a() -> u8 { 1 }\n").expect("write a.rs");

    write_home_config(&home, "mode = \"inject\"\ninclude = [\"**/*.md\"]\n");
    let path = std::env::var_os("PATH").unwrap_or_default();

    let run = run_review(&home, &repo, &path, &stop_payload("s-p6-control", &repo));

    assert_eq!(run.code, 0);
    assert!(
        !is_block(&run.stdout),
        "a well-formed include that legitimately excludes a.rs must allow. \
         stdout: {:?}",
        run.stdout
    );
    assert_eq!(
        last_log_verdict(&home, "s-p6-control"),
        "no-reviewable-changes"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// P7 — a broken home config reads as "so configured" and `status` misreports.
// ═══════════════════════════════════════════════════════════════════════════

/// The home `config.toml` has a TOML syntax error (a missing closing
/// bracket). `Config::load`'s `if let Ok(fc) = toml::from_str(...)` has no
/// `else`, so the parse failure is silent and defaults are used as if no
/// config existed — while `status`, which only checks `exists()`, reports the
/// broken file as the adopted config.
///
/// Contract (audit §1 P7 / §4): `reviewgate review`'s stderr must warn,
/// naming the config path; `reviewgate status`'s config line must say
/// "FAILED" rather than silently reporting the broken file as adopted.
/// EXPECTED RED — probe J observed silent fallback and an unqualified
/// `config: …/config.toml` in status.
#[test]
fn broken_home_config_warns_on_review_and_reports_failed_in_status() {
    let root = unique_dir("p7");
    let home = root.join("home");
    std::fs::create_dir_all(&home).expect("mkdir home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).expect("mkdir repo");
    // Deliberately malformed: missing closing bracket.
    write_home_config(&home, "include = [\"**/*.md\"\n");
    let cfg_path = home.join(".reviewgate/config.toml");
    assert!(
        cfg_path.exists(),
        "fixture precondition: config file exists"
    );

    let path = std::env::var_os("PATH").unwrap_or_default();
    let review = run_review(&home, &repo, &path, &stop_payload("s-p7", &repo));
    assert!(
        review
            .stderr
            .contains(&cfg_path.to_string_lossy().to_string()),
        "a config that exists but fails to parse must warn, naming the \
         broken path, so an operator does not believe unset options are in \
         effect. stderr: {:?}",
        review.stderr
    );

    let status = run_status(&home, &repo);
    assert_eq!(status.code, 0);
    let config_line = status
        .stdout
        .lines()
        .find(|l| l.starts_with("config:"))
        .unwrap_or_else(|| panic!("no config: line in status output:\n{}", status.stdout));
    assert!(
        config_line.contains("FAILED"),
        "status must not report a config file that failed to parse as if it \
         were adopted; the config: line must say FAILED. line: {config_line:?}"
    );
}

/// PASSING CONTROL — a syntactically valid config is adopted normally and
/// `status`'s config line carries no "FAILED" marker.
#[test]
fn valid_home_config_status_has_no_failed_marker() {
    let root = unique_dir("p7-control");
    let home = root.join("home");
    std::fs::create_dir_all(&home).expect("mkdir home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).expect("mkdir repo");
    write_home_config(&home, "include = [\"**/*.md\"]\n");

    let status = run_status(&home, &repo);
    assert_eq!(status.code, 0);
    let config_line = status
        .stdout
        .lines()
        .find(|l| l.starts_with("config:"))
        .unwrap_or_else(|| panic!("no config: line in status output:\n{}", status.stdout));
    assert!(
        !config_line.contains("FAILED"),
        "a valid config must not be reported as FAILED. line: {config_line:?}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// P8 — an unparseable hook payload demotes hook mode to manual-CLI mode.
// ═══════════════════════════════════════════════════════════════════════════

/// Non-empty, non-JSON stdin (garbage a real Stop hook would never send, but
/// distinct from the routine EMPTY-stdin manual-CLI case). `HookInput::parse`
/// returns `None` for it exactly as it would for empty stdin, so
/// `review_command` treats it as manual-CLI mode: a block becomes stderr text
/// plus exit 1, instead of the `{"decision":"block"}` JSON Claude Code's Stop
/// hook protocol actually reads.
///
/// Contract (audit §1 P8 / §4): exit code 0, and stdout must contain the
/// `{"decision":"block"` JSON — i.e. still hook mode. EXPECTED RED — probe G
/// observed exit 1 with the reason only on stderr and nothing on stdout.
#[test]
fn garbage_non_json_stdin_with_a_blocking_diff_still_speaks_hook_protocol() {
    if !git_available() {
        eprintln!("skipping: git not available");
        return;
    }
    let root = unique_dir("p8");
    let home = root.join("home");
    std::fs::create_dir_all(&home).expect("mkdir home");
    let repo = root.join("repo");
    init_repo(&repo);
    std::fs::write(repo.join("a.rs"), "pub fn a() -> u8 { 1 }\n").expect("write a.rs");
    let path = std::env::var_os("PATH").unwrap_or_default();

    let run = run_review(&home, &repo, &path, "not json {");

    assert_eq!(
        run.code, 0,
        "even with an unparseable payload, a real hook invocation must not \
         come back as exit 1 — Claude Code's Stop hook protocol reads the \
         `decision` field, not the exit code. stderr: {:?}",
        run.stderr
    );
    assert!(
        run.stdout.contains("{\"decision\":\"block\""),
        "the block must still be declared via the stdout JSON contract, not \
         demoted to stderr-only manual-CLI output. stdout: {:?} stderr: {:?}",
        run.stdout,
        run.stderr
    );
}

/// PASSING CONTROL — genuinely EMPTY stdin (the routine manual-CLI shape,
/// e.g. a developer running `reviewgate review` by hand with nothing piped
/// in) keeps behaving as manual-CLI mode: exit 1 on block, reason on stderr.
/// This is what distinguishes "no payload at all" from "a real but corrupted
/// hook payload" (P8's scenario) — the fix must not collapse the two.
#[test]
fn empty_stdin_keeps_manual_cli_mode_exit_1_on_block() {
    if !git_available() {
        eprintln!("skipping: git not available");
        return;
    }
    let root = unique_dir("p8-control");
    let home = root.join("home");
    std::fs::create_dir_all(&home).expect("mkdir home");
    let repo = root.join("repo");
    init_repo(&repo);
    std::fs::write(repo.join("a.rs"), "pub fn a() -> u8 { 1 }\n").expect("write a.rs");
    let path = std::env::var_os("PATH").unwrap_or_default();

    let run = run_review(&home, &repo, &path, "");

    assert_eq!(
        run.code, 1,
        "empty stdin is the routine manual-CLI shape and must keep exiting 1 \
         on a block, reason on stderr. stdout: {:?} stderr: {:?}",
        run.stdout, run.stderr
    );
    assert!(
        !run.stdout.contains("\"decision\""),
        "manual-CLI mode must not also emit the hook JSON on stdout: {:?}",
        run.stdout
    );
}
