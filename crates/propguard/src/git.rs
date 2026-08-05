//! git helpers: which files changed, and the actual diff text for them. Pure
//! subprocess calls to `git`. `changed_files` returns a [`ChangeScan`]:
//! `NotRepo` means "confirmed out of scope" — git said so, or git could not
//! answer AND no `.git` exists anywhere above the path to contradict it. It
//! does NOT mean "git unavailable": an unreachable git over a real repo is
//! `Failed`, not `NotRepo`, via `harness_core::git_probe`. (The caller then has no
//! generated code to check and allows the stop), `Failed` means a git command
//! errored inside a real repo — including one that exited 0 but whose stdout
//! could not be read — (the changeset is UNDETERMINED — the gate fails
//! closed / blocks rather than treat it as clean), and `Files(v)` is the
//! (possibly-empty) changed set that was actually read.
//!
//! [`diff_text`] carries the same discipline one frame further: it answers
//! `Determination<DiffText>`, so "the diff is empty" and "the diff (or any
//! part of it) could not be read" are different values rather than the same
//! empty string. Nothing in this module hands a caller a diff with an
//! unobserved hole in it.

use harness_core::boundary;
use harness_core::git_probe::{probe_repo, RepoProbe};
use harness_core::verdict::Determination;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

/// CA-propguard-006: every git invocation below feeds `evaluate()`, which
/// runs *before* `run_checker` is ever reached — so a hung/slow `git`
/// (network-mounted repo, lock contention, corrupted pack, etc.) used to
/// block the whole Stop hook indefinitely with no bound at all. Generous
/// enough not to affect normal usage (git subcommands here are all local,
/// read-only, and expected to finish in well under a second).
const GIT_TIMEOUT: Duration = Duration::from_secs(10);

/// Run `git <args>` in `root` with a bounded wait. Answers
/// [`Determination<String>`]: `Known(stdout)` when the command ran to a
/// conclusion and its stdout was decoded through to EOF, `Undetermined(why)`
/// on spawn failure, non-zero exit, timeout, **or a stdout that could not be
/// read** (the pipe erroring, or still being held open by a lingering
/// descendant when the read budget expires). The bound, the process-group kill
/// on timeout (so no orphaned `git` process — or, if `git` were ever
/// shell-wrapped, none of its descendants either — is left behind), and the
/// bounded stdout read all live in `boundary::run_with_timeout`
/// (CA-propguard-004/005 moved there; see `harness_core::boundary` for the
/// shared implementation and its own tests).
///
/// **Why this is a `Determination` and not an `Option`.** The `Option` form
/// erased *why* a command produced no text, and only one of this module's
/// callers recovered it. That erasure was measured against the real `git`, not
/// argued:
///
/// * [`changed_files`] recovered it. [`collect`] maps an `Undetermined` to
///   `false`, [`scan_changed`] maps that to [`ChangeScan::Failed`], and the
///   gate resolves `Failed` to a block.
/// * [`run_diff`] (the `diff` and `diff --cached` reads) and the untracked
///   `ls-files` read inside [`diff_text`] did NOT: both dropped `None` on the
///   floor via `if let Some(text)`. A `git diff` that could not be read
///   contributed nothing to the diff string and left no trace of having
///   failed. A tracked file whose working-tree content is not valid UTF-8
///   produces exactly that shape (no NUL byte, so `git` emits a textual diff
///   carrying the undecodable bytes): `changed_files` answered
///   `Files(["bad.rs"])` while `diff_text` answered `text: ""` with
///   `truncated: false`, and `gate.rs`'s
///   `if diff.trim().is_empty() { return allow("empty-diff", st) }` ALLOWED.
///   The control with valid UTF-8 in the same file gave a 131-byte diff and a
///   BLOCK (backlog `87dbfbb8`). One step further, with one sub-command
///   readable and another not, the surviving half was handed downstream as a
///   COMPLETE diff — `truncated: false` — hiding the omitted file from the
///   gate's only incompleteness signal (backlog `d8e22b26`).
///
/// Both are now CLOSED: `diff_text` returns `Determination<DiffText>` and
/// forwards the first `Undetermined` from any sub-read, so an unread — or
/// half-read — diff is never a `String` the gate can mistake for the whole
/// change. See `gate.rs`'s `diff-read-failed` decision.
///
/// The `Undetermined` handed back here is always one **forwarded** from
/// `harness_core::boundary`; this module never mints a fresh one, so the
/// give-up telemetry counts each event exactly once (see
/// `harness_core::verdict`'s note on minting vs forwarding).
fn run_git(root: &Path, args: &[&str]) -> Determination<String> {
    run_git_bin(Path::new(GIT), root, args)
}

/// The program `run_git` invokes in production. Named so the seam below reads
/// as "the git binary", rather than a bare string literal repeated twice.
const GIT: &str = "git";

/// [`run_git`] with the invoked binary supplied by the caller.
///
/// The extra parameter exists for one reason: to make a subprocess failure
/// mode *injectable* from a test. There is no way to make the real `git` exit
/// 0 while its stdout cannot be read, so the regression coverage for exactly
/// that shape (a child whose status is observable and whose output is not)
/// hands in a stand-in binary here. Production has one call site and it
/// passes [`GIT`].
fn run_git_bin(git: &Path, root: &Path, args: &[&str]) -> Determination<String> {
    let mut cmd = Command::new(git);
    cmd.current_dir(root)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    match boundary::run_with_timeout(&mut cmd, GIT_TIMEOUT) {
        // `stdout_on_success` already answers `Undetermined` for a non-zero
        // exit and for a stdout that could not be decoded to EOF; hand its
        // answer on unchanged rather than re-deriving one.
        Determination::Known(out) => out.stdout_on_success(),
        // Spawn failure / timeout. FORWARD the boundary's reason — minting a
        // new `Undetermined` here would record the same give-up a second time.
        Determination::Undetermined(why) => Determination::Undetermined(why),
    }
}

/// Tri-state result of scanning the changed-files set. Distinguishes "no git
/// scope" (`NotRepo` → allow) from "a git command errored" (`Failed` →
/// undetermined → the gate fails closed / blocks), so a collapsed-empty scan
/// can never read as "nothing changed → allow". `Files(v)` is a success; an
/// EMPTY `Files` is a genuine no-changes → allow, NOT `Failed`.
///
/// That last sentence is only true because of what an empty `Files` now
/// *requires*: every sub-command spawned, exited 0, **and its stdout was read
/// through to EOF**. Until harness-core 0.2.3 the third clause was missing —
/// `boundary`'s bounded pipe read discarded a read error and folded a
/// read-budget expiry into an empty string, so a `git` that exited 0 while its
/// output never arrived produced `Files(vec![])`, i.e. the *identical* value a
/// clean repo produces. "I read the changed set and it was empty" and "I never
/// got the changed set" were the same bytes here. The boundary now answers
/// `Undetermined` for both of those, `run_git` forwards that `Undetermined`,
/// and `collect` maps it to `Failed`. A *timeout* on the process itself takes
/// the same route, so a hung git also fails the gate closed (bounded &
/// escapable in `gate.rs`), never silently allows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeScan {
    NotRepo,
    Failed,
    Files(Vec<String>),
}

/// Changed paths relative to the repo root: tracked changes vs HEAD, staged
/// changes, and untracked-but-not-ignored files. Returns `NotRepo` when there
/// is no git repo, `Failed` when any sub-command errored (spawn failure /
/// non-zero exit / timeout / unreadable stdout) inside a real repo, or
/// `Files(v)` on success (possibly empty).
pub fn changed_files(root: &Path) -> ChangeScan {
    scan_changed(root, Path::new(GIT))
}

/// [`changed_files`] with the invoked git binary injectable — the same seam,
/// and for the same reason, as [`run_git_bin`].
fn scan_changed(root: &Path, git: &Path) -> ChangeScan {
    match probe_repo(root) {
        RepoProbe::Repo => {}
        // Genuinely out of scope: git said so, or git could not answer and no
        // `.git` exists anywhere above to contradict it.
        RepoProbe::NotRepo => return ChangeScan::NotRepo,
        // git could not be run / refused to answer while a `.git` exists.
        // Undetermined is not "no scope" — resolve to the restricted side.
        RepoProbe::Undetermined => return ChangeScan::Failed,
    }
    let mut out = Vec::new();
    // If ANY sub-command errors, the changed set is undetermined → fail closed.
    // A clean repo's commands all exit 0 with empty stdout *that was actually
    // read to EOF* → Files(vec![]); an output that never arrived is not that.
    let ok = collect(root, git, &["diff", "--name-only"], &mut out)
        && collect(root, git, &["diff", "--cached", "--name-only"], &mut out)
        && collect(
            root,
            git,
            &["ls-files", "--others", "--exclude-standard"],
            &mut out,
        );
    if !ok {
        return ChangeScan::Failed;
    }
    out.sort();
    out.dedup();
    ChangeScan::Files(out)
}

/// Run one `git` sub-command, appending its trimmed non-empty stdout lines to
/// `out`. Returns `true` on success, `false` when `run_git_bin` answered
/// `Undetermined` (spawn error / non-zero exit / timeout / stdout that could
/// not be read) — the caller maps `false` to `ChangeScan::Failed`. A
/// successful command whose stdout was read to EOF and was EMPTY still returns
/// `true` (clean ≠ failed); an empty *because unread* stdout does not reach
/// this arm at all, it arrives as `Undetermined`.
///
/// Collapsing to `bool` is safe *here* and nowhere else in this module: the
/// one caller ([`scan_changed`]) maps `false` to the restrictive
/// [`ChangeScan::Failed`], so the collapse loses the reason string but never
/// the direction. Contrast [`run_diff`], which keeps the full
/// `Determination` because its caller has to distinguish "the diff is empty"
/// from "the diff could not be read".
fn collect(root: &Path, git: &Path, args: &[&str], out: &mut Vec<String>) -> bool {
    match run_git_bin(git, root, args) {
        Determination::Known(text) => {
            for line in text.lines() {
                let line = line.trim();
                if !line.is_empty() {
                    out.push(line.to_string());
                }
            }
            true
        }
        Determination::Undetermined(_) => false,
    }
}

/// A checkable diff plus whether it had to be truncated to fit `max_bytes`.
///
/// A `DiffText` value now means something stronger than it used to: **every**
/// read that feeds it succeeded. `truncated` says the tail was deliberately
/// cut at a known byte budget — it never means "a piece went missing". A read
/// that failed does not produce a `DiffText` with a hole in it, it produces a
/// [`Determination::Undetermined`] instead (see [`diff_text`]).
#[derive(Debug)]
pub struct DiffText {
    pub text: String,
    pub truncated: bool,
}

/// The combined diff for `files`: unstaged + staged hunks, plus the full text of
/// any untracked files (which have no diff). Truncated to `max_bytes` with a
/// marker so a huge diff can't blow up memory or the checker prompt.
///
/// Three answers, not two — the same discipline [`ChangeScan`] applies to the
/// changed-file scan, applied one frame further along:
///
/// * `Known(DiffText { text, truncated })` — every sub-read ran to a
///   conclusion and was decoded to EOF. `text` may legitimately be empty
///   (nothing to show for these paths) and `truncated` may be true (the tail
///   was cut at `max_bytes`), but nothing is missing that we failed to observe.
/// * `Undetermined(why)` — at least one sub-read did not run to a conclusion:
///   `git diff`, `git diff --cached`, the untracked `ls-files`, or the body of
///   an untracked file. The reason is FORWARDED from whichever read produced
///   it (never re-minted, so the give-up telemetry counts it once).
///
/// This is the fix for backlog `87dbfbb8` and `d8e22b26`. Previously every one
/// of those reads was `if let Some(text)`, so a failure contributed nothing and
/// left no trace: an unreadable diff came back as `text: ""` (which the gate
/// allowed as `empty-diff`) and a half-readable one came back as the surviving
/// half with `truncated: false` — announcing itself complete over a diff that
/// silently omitted a file. **A partial diff is deliberately not returned at
/// all.** Returning "here is most of it" and expecting the caller to notice is
/// the shape that failed; the first failed read decides the whole answer.
pub fn diff_text(root: &Path, files: &[String], max_bytes: usize) -> Determination<DiffText> {
    let mut s = String::new();

    if let Determination::Undetermined(why) = run_diff(root, &["diff", "--"], files, &mut s) {
        return Determination::Undetermined(why);
    }
    if let Determination::Undetermined(why) =
        run_diff(root, &["diff", "--cached", "--"], files, &mut s)
    {
        return Determination::Undetermined(why);
    }

    // untracked files among `files`: include their contents as "new file" diffs.
    let mut others = Vec::new();
    let mut args: Vec<&str> = vec!["ls-files", "--others", "--exclude-standard", "--"];
    args.extend(files.iter().map(String::as_str));
    match run_git(root, &args) {
        Determination::Known(text) => {
            for line in text.lines() {
                let line = line.trim();
                if !line.is_empty() {
                    others.push(line.to_string());
                }
            }
        }
        // We cannot tell whether there were untracked files to include. That
        // is an unobserved part of the change, not an absent one.
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    }
    for f in others {
        s.push_str(&format!("\n=== new file: {f} ===\n"));
        match harness_core::boundary::read_to_string(&root.join(&f)) {
            // The file's contents, as before.
            Determination::Known(Some(content)) => {
                s.push_str(&content);
                if !s.ends_with('\n') {
                    s.push('\n');
                }
            }
            // OBSERVED absent: `ls-files` listed it and it is gone by the time
            // we read it (deleted between the two calls). That is a real
            // observation, not a failure, so it does not make the diff
            // undetermined — but it is recorded in the text rather than left
            // as a bare header, so a reader is never shown an empty section
            // with no explanation.
            Determination::Known(None) => {
                s.push_str("(file no longer present when propguard read it)\n");
            }
            // NOT observed: the file exists and could not be read (permission
            // denied, not valid UTF-8, …). Skipping it here is exactly the
            // hole `87dbfbb8` measured one layer up — the diff would go on to
            // be judged without this file's contents in it.
            Determination::Undetermined(why) => return Determination::Undetermined(why),
        }
        if s.len() > max_bytes {
            break;
        }
    }

    Determination::Known(truncate_on_boundary(s, max_bytes))
}

/// Append one `git diff` variant's output to `out`. `Known(())` means the
/// command ran and its whole output landed in `out`; `Undetermined(why)`
/// forwards [`run_git`]'s reason and means `out` is missing this variant's
/// hunks entirely. The caller MUST NOT continue building a diff on top of an
/// `Undetermined` — that is precisely how a partial diff used to be presented
/// as complete (backlog `d8e22b26`).
fn run_diff(root: &Path, base: &[&str], files: &[String], out: &mut String) -> Determination<()> {
    let mut args: Vec<&str> = base.to_vec();
    args.extend(files.iter().map(String::as_str));
    match run_git(root, &args) {
        Determination::Known(text) => {
            out.push_str(&text);
            Determination::Known(())
        }
        Determination::Undetermined(why) => Determination::Undetermined(why),
    }
}

fn truncate_on_boundary(mut s: String, max_bytes: usize) -> DiffText {
    if s.len() <= max_bytes {
        return DiffText {
            text: s,
            truncated: false,
        };
    }
    let mut cut = max_bytes;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    s.truncate(cut);
    s.push_str("\n… (diff truncated by propguard max_diff_bytes)\n");
    DiffText {
        text: s,
        truncated: true,
    }
}

#[cfg(test)]
#[allow(clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn under_limit_is_not_truncated() {
        let d = truncate_on_boundary("short diff".to_string(), 1000);
        assert!(!d.truncated);
        assert_eq!(d.text, "short diff");
    }

    #[test]
    fn over_limit_is_flagged_and_marked() {
        let big = "x".repeat(500);
        let d = truncate_on_boundary(big, 32);
        assert!(d.truncated, "a diff larger than max_bytes must be flagged");
        assert!(d.text.contains("truncated"));
    }

    #[test]
    fn truncation_respects_char_boundaries() {
        let s = "あいうえお".to_string(); // 15 bytes
        let d = truncate_on_boundary(s, 7); // 7 is mid-character (6 is a boundary)
        assert!(d.truncated);
        assert!(d.text.starts_with("あい"));
    }

    // ── CA-propguard-006: git subprocess calls must be timeout-bounded ─────
    //
    // Every git call in this module fed evaluate() with no timeout at all
    // before this fix, so a hung/slow `git` invocation blocked the whole Stop
    // hook indefinitely. `run_git` (the single choke point all git calls in
    // this module now go through) must return promptly with a graceful
    // fallback (`Undetermined`, which every caller resolves to the restrictive
    // side) instead of hanging past `GIT_TIMEOUT`, and must not leave the hung
    // process running afterwards.
    #[test]
    fn hung_git_invocation_returns_promptly_with_graceful_fallback() {
        // Not a real git repo, but `run_git` doesn't care what program name
        // it invokes — it operates on `Command::new("git")` only. To
        // exercise the *timeout* path itself (rather than "git exited
        // quickly because the dir isn't a repo"), reach for the same
        // machinery `run_git` uses — `boundary::run_with_timeout` — directly
        // here, against a deliberately hanging shell command standing in for
        // a stuck `git`, with a short local timeout override so the test
        // doesn't have to wait out the production `GIT_TIMEOUT` (10s) or
        // depend on a real slow git binary being available.
        let tmp = std::env::temp_dir().join(format!(
            "propguard-git-timeout-test-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::create_dir_all(&tmp);

        // A marker unique to this test run's pid, so the liveness probe
        // below cannot match anything except the actual hung descendant —
        // not the probe command's own argv (see the equivalent guard in
        // harness-core's boundary.rs test of the same underlying mechanism).
        let marker = format!("propguard-git-rs-hang-marker-{}", std::process::id());

        let mut cmd = Command::new("sh");
        cmd.current_dir(&tmp)
            .args(["-c", &format!("sh -c 'sleep 30' {marker}")])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let short_timeout = Duration::from_millis(300);

        let start = std::time::Instant::now();
        let outcome = boundary::run_with_timeout(&mut cmd, short_timeout);
        let elapsed = start.elapsed();

        assert!(
            matches!(outcome, Determination::Undetermined(_)),
            "a timed-out invocation must fall back gracefully (Undetermined), not fabricate a \
             Known result: {outcome:?}"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "run_git (via boundary::run_with_timeout) must return promptly on timeout, took \
             {elapsed:?} (GIT_TIMEOUT is only used for the real `git` binary in production; this \
             test drives the same code path with a short local timeout override to stay fast)"
        );

        // Give the group-kill a moment to actually land, then confirm the
        // hung process is really gone — not merely abandoned to linger in
        // the background while this call returned. This is run_git's own
        // regression coverage for CA-propguard-006's "must not leave the
        // hung process running afterwards" contract, independent of
        // boundary's own unit test for the same mechanism.
        std::thread::sleep(Duration::from_millis(200));
        // `pgrep -f` (NOT `-fc`): the count flag `-c` is a Linux procps
        // extension that macOS's pgrep rejects with exit 2 + a usage message.
        // The boundary correctly reported that as Undetermined, so this test —
        // the only regression coverage for CA-propguard-006's "must not leave
        // the hung process running" contract — could never run on macOS and the
        // contract was UNVERIFIED there. Counting lines ourselves is portable:
        // exit 0 means matches were printed, exit 1 means none were.
        let still_running = expect_known(harness_core::boundary::run(
            Command::new("pgrep").arg("-f").arg(&marker),
        ));
        let count = expect_known(still_running.stdout_allowing(&[0, 1]))
            .lines()
            .filter(|l| !l.trim().is_empty())
            .count();
        assert_eq!(
            count, 0,
            "run_git must not leave a timed-out git invocation (marker {marker}) running"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[track_caller]
    fn expect_known<T>(d: Determination<T>) -> T {
        match d {
            Determination::Known(v) => v,
            Determination::Undetermined(why) => {
                panic!("expected Known, got Undetermined: {}", why.as_str())
            }
        }
    }

    /// evaluate()'s git-dependent step (`changed_files`) must itself return
    /// promptly rather than hang, given a `root` where `git` can't even run
    /// (simulating "git unavailable") — the graceful-fallback contract this
    /// finding requires end-to-end, not just at the `run_git` choke point.
    #[test]
    fn changed_files_returns_promptly_when_git_unavailable() {
        let tmp = std::env::temp_dir().join(format!(
            "propguard-git-unavailable-test-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::create_dir_all(&tmp);

        let start = std::time::Instant::now();
        let result = changed_files(&tmp);
        let elapsed = start.elapsed();

        assert_eq!(
            result,
            ChangeScan::NotRepo,
            "a non-repo dir must yield NotRepo (no scope → allow), not hang or fabricate a diff"
        );
        assert!(
            elapsed < Duration::from_secs(15),
            "changed_files must return within the git timeout budget, took {elapsed:?}"
        );
    }

    // ── changed_files tri-state: NotRepo / Failed / Files ──────────────────

    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// A fresh, unique scratch directory under the system temp dir (avoids a
    /// `tempfile` dev-dependency; mirrors this module's existing test style).
    /// Caller is responsible for cleanup.
    fn scratch_dir() -> std::path::PathBuf {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!(
            "propguard-git-tristate-test-{}-{}-{}",
            std::process::id(),
            line!(),
            n
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("create scratch dir");
        p
    }

    /// A clean repo is a SUCCESSFUL empty scan → `Files(vec![])`, never
    /// `Failed`. Pins that a clean tree (git commands exit 0 with empty stdout)
    /// does not start blocking under the fail-closed change.
    #[test]
    fn clean_repo_is_empty_files_not_failed() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = scratch_dir();
        let root = root.as_path();
        for args in [
            &["init", "-q"][..],
            &["config", "user.email", "t@t.com"][..],
            &["config", "user.name", "t"][..],
        ] {
            assert!(Command::new("git")
                .current_dir(root)
                .args(args)
                .status()
                .expect("git")
                .success());
        }
        std::fs::write(root.join("a.txt"), "x\n").unwrap();
        assert!(Command::new("git")
            .current_dir(root)
            .args(["add", "a.txt"])
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .current_dir(root)
            .args(["commit", "-qm", "init"])
            .status()
            .unwrap()
            .success());

        assert_eq!(
            changed_files(root),
            ChangeScan::Files(Vec::new()),
            "a clean repo must be a successful empty scan, not Failed"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// The `collect` helper's success/failure signal (which drives the Failed
    /// mapping) must distinguish an errored sub-command from an empty-but-
    /// successful one: a bogus git flag errors → `false`; a valid command with
    /// no output → `true`.
    #[test]
    fn collect_reports_error_vs_empty_success() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = scratch_dir();
        let root = root.as_path();
        assert!(Command::new("git")
            .current_dir(root)
            .args(["init", "-q"])
            .status()
            .expect("git")
            .success());

        let mut out = Vec::new();
        assert!(
            collect(root, Path::new(GIT), &["diff", "--name-only"], &mut out),
            "an empty-but-successful git command must report true (not Failed)"
        );
        assert!(out.is_empty());
        assert!(
            !collect(
                root,
                Path::new(GIT),
                &["diff", "--no-such-flag-xyzzy"],
                &mut out
            ),
            "a non-zero git exit must report false so the scan fails closed"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    // ── exit 0 with an UNREADABLE stdout must not read as a clean repo ─────
    //
    // The shape these three tests pin down is one the real `git` cannot be
    // made to produce, which is why the binary is injected: a child whose
    // exit *status* is observable (0) while its *output* is not. Before the
    // harness-core 0.2.3 boundary fix that answer collapsed to an empty
    // stdout, so `changed_files` returned `Files(vec![])` — byte-identical to
    // the answer a genuinely clean repo gives, and read downstream as
    // "nothing changed → allow".
    //
    // The injected fault here is a stdout the read itself *rejects* (bytes
    // that are not valid UTF-8), chosen because it is deterministic: the
    // child exits at once and the pipe reaches EOF at once, so the outcome
    // cannot drift with machine load. The other route to an unreadable pipe —
    // a read that never reaches EOF inside the budget — funnels into the very
    // same `Determination::Undetermined` one layer down, and is covered where
    // it can be driven without a wall-clock race, in harness-core's
    // `boundary::tests::run_with_timeout_pipe_held_open_after_clean_exit_is_
    // undetermined_not_empty`. What is asserted HERE is propguard's mapping:
    // an undetermined stdout must reach `ChangeScan::Failed`.
    //
    // They are written as a set on purpose. `..._is_failed_not_empty_files`
    // alone would still pass if every scan through this seam failed, so the
    // other two are its anti-vacuity partners: the SAME stand-in binary,
    // differing only in whether stdout is readable, must still produce
    // `Files(["src/lib.rs"])` and `Files(vec![])` respectively.

    /// A real git repo (so `probe_repo` answers `Repo`), with no commits —
    /// the change-scan sub-commands are served by the injected binary, not by
    /// this repo's contents.
    #[cfg(unix)]
    fn init_repo() -> std::path::PathBuf {
        let root = scratch_dir();
        assert!(Command::new("git")
            .current_dir(&root)
            .args(["init", "-q"])
            .status()
            .expect("git init")
            .success());
        root
    }

    /// Write an executable stand-in for the `git` binary whose body is `body`,
    /// and hand back its path.
    #[cfg(unix)]
    fn fake_git(dir: &Path, body: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join("fake-git");
        std::fs::write(&p, format!("#!/bin/sh\n{body}")).expect("write fake git");
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        p
    }

    /// THE claim: a `git` that exits 0 while its stdout cannot be read is a
    /// changeset this gate did NOT observe. It must be `Failed`, and
    /// specifically must not be the `Files(vec![])` that
    /// `git_with_readable_but_empty_stdout_is_still_empty_files` shows a clean
    /// repo produces.
    #[cfg(unix)]
    #[test]
    fn git_that_exits_zero_with_unreadable_stdout_is_failed_not_empty_files() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo();
        let bin = scratch_dir();
        // Exits 0 having written two bytes that are not valid UTF-8, so the
        // pipe read fails. Pre-fix this scan returned `Files(vec![])` — the
        // written bytes were dropped on the floor and the caller was told the
        // repo was clean.
        let git = fake_git(&bin, "printf '\\377\\376'\nexit 0\n");

        assert_eq!(
            scan_changed(&root, &git),
            ChangeScan::Failed,
            "a git that exited 0 but whose stdout could not be read is an UNOBSERVED changeset; \
             answering Files(..) here makes it indistinguishable from a clean repo"
        );
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&bin);
    }

    /// Anti-vacuity partner #1: the same injected-binary seam, same exit 0,
    /// readable stdout — the printed path must still arrive as `Files`.
    #[cfg(unix)]
    #[test]
    fn git_with_readable_stdout_still_yields_the_files_it_printed() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo();
        let bin = scratch_dir();
        let git = fake_git(&bin, "printf 'src/lib.rs\\n'\nexit 0\n");

        assert_eq!(
            scan_changed(&root, &git),
            ChangeScan::Files(vec!["src/lib.rs".to_string()]),
            "a readable stdout must still be parsed into the changed set — otherwise the \
             sibling test above would pass merely by everything failing"
        );
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&bin);
    }

    /// Anti-vacuity partner #2, and the exact contrast the fix turns on: empty
    /// stdout that WAS read is still a successful empty scan. `Failed` is
    /// reserved for output that could not be read, not for output that was
    /// legitimately empty.
    #[cfg(unix)]
    #[test]
    fn git_with_readable_but_empty_stdout_is_still_empty_files() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo();
        let bin = scratch_dir();
        let git = fake_git(&bin, "exit 0\n");

        assert_eq!(
            scan_changed(&root, &git),
            ChangeScan::Files(Vec::new()),
            "an empty stdout that was actually read is a clean scan, not a failure"
        );
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&bin);
    }

    // ── backlog 87dbfbb8 / d8e22b26 at THIS layer: `diff_text` must answer
    //    Undetermined when any read feeding it failed. ───────────────────────
    //
    // Honesty note on provenance: these unit tests were written WITH the fix,
    // not before it — the pre-fix `diff_text` returned a bare `DiffText`, so a
    // test asserting on a `Determination` could not compile against it and
    // could not have been observed failing. The RED that WAS observed before
    // any source change is the behavioural pair in `gate.rs`
    // (`a_diff_that_could_not_be_read_blocks_it_does_not_allow_as_empty` and
    // `a_partially_read_diff_is_not_handed_on_as_complete`), which drive the
    // same faults end to end through `evaluate` and failed with
    // `allow tag=empty-diff` and `tag=below-threshold` respectively. These are
    // the narrower pins on the mechanism those two exercise.

    #[track_caller]
    fn expect_undetermined<T: std::fmt::Debug>(d: Determination<T>, what: &str) {
        match d {
            Determination::Undetermined(_) => {}
            Determination::Known(v) => panic!("{what}; got Known({v:?})"),
        }
    }

    /// A committed repo, with each `(name, body)` written and committed, so a
    /// later edit is a change against HEAD.
    #[cfg(unix)]
    fn repo_with(files: &[(&str, &[u8])]) -> std::path::PathBuf {
        let root = scratch_dir();
        for args in [
            &["init", "-q"][..],
            &["config", "user.email", "t@t.com"][..],
            &["config", "user.name", "t"][..],
        ] {
            assert!(Command::new("git")
                .current_dir(&root)
                .args(args)
                .status()
                .expect("git")
                .success());
        }
        for (name, body) in files {
            std::fs::write(root.join(name), body).expect("write");
            assert!(Command::new("git")
                .current_dir(&root)
                .args(["add", name])
                .status()
                .unwrap()
                .success());
        }
        assert!(Command::new("git")
            .current_dir(&root)
            .args(["commit", "-qm", "init"])
            .status()
            .unwrap()
            .success());
        root
    }

    /// Not valid UTF-8, and no NUL — so `git` calls the file TEXT and puts
    /// these bytes into the diff body, where propguard's read cannot decode
    /// them. This needs no stand-in binary: the real `git` produces it.
    const BAD_BYTES: &[u8] = b"fn main() {\n    let s = \"\xff\xfe\xfd\";\n}\n";
    const GOOD_BYTES: &[u8] = b"fn main() {\n    let s = \"replaced\";\n}\n";

    /// THE claim (backlog 87dbfbb8): a `git diff` whose output cannot be
    /// decoded is a diff this gate did NOT observe. It must be `Undetermined`,
    /// never a `Known` empty string — the value a repo with nothing to show
    /// legitimately produces.
    #[cfg(unix)]
    #[test]
    fn diff_text_of_an_undecodable_diff_is_undetermined_not_empty() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = repo_with(&[("bad.rs", b"fn main() {}\n")]);
        std::fs::write(root.join("bad.rs"), BAD_BYTES).expect("write bad");

        expect_undetermined(
            diff_text(&root, &["bad.rs".to_string()], 100_000),
            "an undecodable `git diff` must be UNDETERMINED — returning an empty diff makes it \
             indistinguishable from having nothing to show, which the gate allows",
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Anti-vacuity partner: the same repo, same file, same edit, decodable
    /// bytes — the real diff text must still come back, and must actually
    /// mention the file. Without this, an implementation returning
    /// `Undetermined` unconditionally would pass the test above.
    #[cfg(unix)]
    #[test]
    fn diff_text_of_a_decodable_diff_is_known_and_carries_the_real_text() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = repo_with(&[("bad.rs", b"fn main() {}\n")]);
        std::fs::write(root.join("bad.rs"), GOOD_BYTES).expect("write good");

        let d = expect_known(diff_text(&root, &["bad.rs".to_string()], 100_000));
        assert!(
            d.text.contains("bad.rs") && d.text.contains("replaced"),
            "a readable diff must arrive with its real content: {:?}",
            d.text
        );
        assert!(!d.truncated);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// backlog d8e22b26: `git diff` succeeds (unstaged a.rs) while
    /// `git diff --cached` cannot be decoded (staged b.rs). The readable half
    /// must NOT be handed back as a complete diff. Before the fix it was:
    /// text mentioning only a.rs, `truncated: false`.
    #[cfg(unix)]
    #[test]
    fn diff_text_with_one_unreadable_subcommand_is_undetermined_not_a_partial_diff() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = repo_with(&[("a.rs", b"fn a() {}\n"), ("b.rs", b"fn b() {}\n")]);
        std::fs::write(root.join("a.rs"), b"fn a() { let x = 1; }\n").expect("write a");
        std::fs::write(root.join("b.rs"), BAD_BYTES).expect("write b");
        assert!(Command::new("git")
            .current_dir(&root)
            .args(["add", "b.rs"])
            .status()
            .unwrap()
            .success());

        expect_undetermined(
            diff_text(&root, &["a.rs".to_string(), "b.rs".to_string()], 100_000),
            "a diff assembled from one readable and one unreadable sub-command is INCOMPLETE; \
             returning the readable half presents it as the whole change",
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Anti-vacuity partner for the partial case: the identical
    /// staged-plus-unstaged setup, both decodable — the combined diff must
    /// come back `Known` and mention BOTH files, proving the fix keys on
    /// readability and not merely on "two sub-commands were involved".
    #[cfg(unix)]
    #[test]
    fn diff_text_with_both_subcommands_readable_mentions_both_files() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = repo_with(&[("a.rs", b"fn a() {}\n"), ("b.rs", b"fn b() {}\n")]);
        std::fs::write(root.join("a.rs"), b"fn a() { let x = 1; }\n").expect("write a");
        std::fs::write(root.join("b.rs"), GOOD_BYTES).expect("write b");
        assert!(Command::new("git")
            .current_dir(&root)
            .args(["add", "b.rs"])
            .status()
            .unwrap()
            .success());

        let d = expect_known(diff_text(
            &root,
            &["a.rs".to_string(), "b.rs".to_string()],
            100_000,
        ));
        assert!(
            d.text.contains("a.rs") && d.text.contains("b.rs"),
            "both the unstaged and the staged half must be present: {:?}",
            d.text
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// An untracked file whose contents cannot be decoded leaves the rendered
    /// diff without that file's body — the same hole, reached through the
    /// third read `diff_text` performs rather than through `git diff`.
    #[cfg(unix)]
    #[test]
    fn diff_text_with_an_undecodable_untracked_file_is_undetermined() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = repo_with(&[("keep.rs", b"fn keep() {}\n")]);
        std::fs::write(root.join("new.rs"), BAD_BYTES).expect("write untracked");

        expect_undetermined(
            diff_text(&root, &["new.rs".to_string()], 100_000),
            "an untracked file whose body could not be read leaves a hole in the diff",
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Anti-vacuity partner: a decodable untracked file still has its body
    /// inlined as a "new file" section.
    #[cfg(unix)]
    #[test]
    fn diff_text_with_a_decodable_untracked_file_inlines_its_body() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = repo_with(&[("keep.rs", b"fn keep() {}\n")]);
        std::fs::write(root.join("new.rs"), GOOD_BYTES).expect("write untracked");

        let d = expect_known(diff_text(&root, &["new.rs".to_string()], 100_000));
        assert!(
            d.text.contains("=== new file: new.rs ===") && d.text.contains("replaced"),
            "a readable untracked file must be inlined: {:?}",
            d.text
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The anti-vacuity floor for this whole family: a repo where the paths
    /// have nothing to show must still be a `Known` EMPTY diff. `Undetermined`
    /// is reserved for output that could not be read, never for output that
    /// was legitimately empty — the same contrast
    /// `git_with_readable_but_empty_stdout_is_still_empty_files` draws for the
    /// changed-file scan.
    #[cfg(unix)]
    #[test]
    fn diff_text_of_an_unchanged_file_is_known_and_empty_not_undetermined() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = repo_with(&[("keep.rs", b"fn keep() {}\n")]);

        let d = expect_known(diff_text(&root, &["keep.rs".to_string()], 100_000));
        assert!(
            d.text.trim().is_empty(),
            "an unchanged file has an empty diff, and that emptiness was OBSERVED: {:?}",
            d.text
        );
        assert!(!d.truncated);
        let _ = std::fs::remove_dir_all(&root);
    }
}
