//! Fallible input boundaries, typed so "could not observe" survives the call.
//!
//! Three things reach into the world from a gate: directory listings, file
//! reads, and subprocesses. Each has a failure mode that std reports as an
//! error and that callers routinely flatten into a permissive value — an empty
//! `Vec`, an empty `String`, an ignored exit code. Once flattened, "I looked and
//! found nothing" and "I could not look" are the same bytes, and every check
//! downstream reads the second as the first.
//!
//! These wrappers return [`Determination`], so the flattening step has to be
//! written out loud. There is no `unwrap_or` to reach for: the only extractor is
//! `require`, which yields `Result<T, Verdict>` and makes `?` — fail closed —
//! the shortest thing a caller can type.
//!
//! The distinction each wrapper draws is between *absence* and *opacity*:
//!
//! | situation | answer |
//! |---|---|
//! | the path is not there | `Known` (empty / `None`) — a real observation |
//! | the path is there but unreadable | `Undetermined` — carries why |
//! | the process ran and exited non-zero | `Known` — the code is the caller's to judge |
//! | the process could not be run, or was killed by a signal | `Undetermined` |
//!
//! A missing path is genuinely empty; an unreadable one is not. Conflating them
//! is the single most common shape of fail-open in this repo, which is why
//! [`Determination`]'s own documentation uses exactly this `read_dir` example.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use wait_timeout::ChildExt;

use crate::verdict::Determination;

/// Entries directly under `dir`, sorted by file name for a stable caller-visible
/// order.
///
/// `NotFound` yields `Known(vec![])`: a directory that does not exist really
/// does contain nothing, and a caller that walks zero entries has not been
/// misled. Every other error — `PermissionDenied`, a broken symlink chain, an
/// I/O fault — yields `Undetermined`, because the directory may well have held
/// the thing the caller was looking for.
///
/// A failure to read one entry mid-iteration is `Undetermined` for the whole
/// call rather than a skipped element. Returning the other entries would be a
/// partial listing indistinguishable from a complete one, and a check that says
/// "I examined this directory" while silently omitting a member is the exact
/// failure this module exists to prevent.
pub fn read_dir_entries(dir: &Path) -> Determination<Vec<PathBuf>> {
    let iter = match std::fs::read_dir(dir) {
        Ok(iter) => iter,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Determination::known(Vec::new()),
        Err(e) => {
            return Determination::undetermined(format!(
                "cannot list {}: {e} — treating this as an empty directory would report \
                 an unexamined path as examined",
                dir.display()
            ))
        }
    };

    let mut out = Vec::new();
    for entry in iter {
        match entry {
            Ok(entry) => out.push(entry.path()),
            Err(e) => {
                return Determination::undetermined(format!(
                    "cannot read an entry of {}: {e} — the entries read so far are a \
                     partial listing and would be indistinguishable from a complete one",
                    dir.display()
                ))
            }
        }
    }
    out.sort_by_key(|p| p.file_name().map(OsString::from));
    Determination::known(out)
}

/// The contents of `path`, or `Known(None)` if it does not exist.
///
/// The `Option` is the point: a caller must decide what an absent file means for
/// its own check, and cannot get there by way of an empty string that also
/// stands for "unreadable". Exactly one error kind is treated as absence —
/// `NotFound`; every other kind, including `PermissionDenied` and the
/// `InvalidData` that non-UTF-8 contents produce, is `Undetermined`. The list is
/// phrased that way round because it is what the match arms below guarantee
/// structurally; only the `PermissionDenied` case has a test behind it.
pub fn read_to_string(path: &Path) -> Determination<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Determination::known(Some(text)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Determination::known(None),
        Err(e) => Determination::undetermined(format!(
            "cannot read {}: {e} — treating this as empty would report an unread \
             file as read",
            path.display()
        )),
    }
}

/// What a subprocess did, with the exit code kept in front of the output.
///
/// `stdout` is deliberately not a public field and there is no accessor that
/// hands it over unconditionally. Every route to it — [`stdout_on_success`] or
/// [`stdout_allowing`] — takes the acceptable exit codes as an argument, so a
/// caller cannot read a checker's output without having said which codes mean
/// the checker actually ran. A crashed checker is not a passing checker, and
/// the type refuses to let that be a one-character omission.
///
/// [`stdout_on_success`]: CommandOutput::stdout_on_success
/// [`stdout_allowing`]: CommandOutput::stdout_allowing
#[must_use = "the exit code carries the verdict; dropping this discards it"]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    code: i32,
    stdout: String,
    stderr: String,
    display: String,
}

impl CommandOutput {
    /// The process's exit code.
    pub fn code(&self) -> i32 {
        self.code
    }

    /// Whatever the process wrote to stderr. Safe to expose unconditionally
    /// because it is diagnostic text, not a checker's answer — nobody derives a
    /// verdict from it.
    pub fn stderr(&self) -> &str {
        &self.stderr
    }

    /// stdout when the process exited 0, `Undetermined` otherwise.
    pub fn stdout_on_success(self) -> Determination<String> {
        self.stdout_allowing(&[0])
    }

    /// stdout when the exit code is one the caller listed as meaning "ran to a
    /// conclusion", `Undetermined` otherwise.
    ///
    /// Some tools answer with a non-zero code: `grep` exits 1 for no-match,
    /// `diff --no-index` exits 1 when files differ. Those are conclusions, not
    /// crashes, and folding them into `Undetermined` would block on every clean
    /// run. Passing them here is how a caller says which codes it has actually
    /// reasoned about — the ones it did not name stay undetermined.
    pub fn stdout_allowing(self, ok_codes: &[i32]) -> Determination<String> {
        if ok_codes.contains(&self.code) {
            return Determination::known(self.stdout);
        }
        let tail: String = self.stderr.chars().take(400).collect();
        Determination::undetermined(format!(
            "`{}` exited {} (expected one of {:?}); a checker that did not run to a \
             conclusion has not passed. stderr: {}",
            self.display,
            self.code,
            ok_codes,
            if tail.is_empty() { "<empty>" } else { &tail }
        ))
    }
}

/// Run `cmd` to completion.
///
/// `Undetermined` when the process could not be started (missing binary, denied
/// exec) and when it terminated without an exit code — on Unix that means a
/// signal, where the absence of a code is literally "cannot tell how it went".
/// A process that ran and exited is `Known` whatever its code, because judging
/// the code belongs to the caller; [`CommandOutput`] makes that judgement
/// unavoidable rather than making it here.
pub fn run(cmd: &mut Command) -> Determination<CommandOutput> {
    // Program *and* args: nearly every checker in this repo is invoked as
    // `python3 scripts/check-<something>.py`, so a label built from the program
    // alone renders every one of them as "python3" — in the one string whose
    // entire job is telling a human which check could not be determined.
    let display = std::iter::once(cmd.get_program())
        .chain(cmd.get_args())
        .map(|a| a.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ");
    let out = match cmd.output() {
        Ok(out) => out,
        Err(e) => {
            return Determination::undetermined(format!(
                "cannot run {display}: {e} — a checker that never started has not passed"
            ))
        }
    };
    let Some(code) = out.status.code() else {
        return Determination::undetermined(format!(
            "{display} terminated without an exit code (signal); there is no result to read"
        ));
    };
    Determination::known(CommandOutput {
        code,
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        display,
    })
}

/// Run `cmd` to completion, but never block past `timeout`.
///
/// This is [`run`] plus a bound: a checker/subprocess that hangs — a stuck
/// `git`, a shell-wrapped checker command that backgrounds a grandchild —
/// must not be able to block the caller forever. `Undetermined` covers every
/// way this can fail to produce a trustworthy result: spawn failure, the wait
/// itself erroring, a signal (no exit code), and timeout. `Known` only when
/// the process ran to completion within `timeout`, whatever its exit code —
/// judging the code is still the caller's job via [`CommandOutput`].
///
/// On Unix the child is placed in its own process group
/// (`process_group(0)`) before spawn, so a timeout can kill the whole tree
/// the child spawned (e.g. a shell exec'ing or backgrounding a real
/// long-running process) via a single negative-pid `SIGKILL`, not just the
/// direct child. `child.kill()` is also called as a fallback/supplement (a
/// no-op if the group kill already reaped it, and the only mechanism at all
/// on non-Unix platforms), and `child.wait()` reaps the zombie afterward.
///
/// stdout is read on a background thread and joined with `recv_timeout`
/// bounded by `timeout`, rather than read inline: the immediate child having
/// exited does not guarantee the write end of the stdout pipe is closed — a
/// backgrounded/detached grandchild can still hold it open, and a bare
/// `read_to_string` would then block indefinitely even after `wait_timeout`
/// reports the child gone. If the read itself cannot be joined within the
/// timeout, the call is `Undetermined` rather than silently returning a
/// partial read as if it were complete output.
pub fn run_with_timeout(cmd: &mut Command, timeout: Duration) -> Determination<CommandOutput> {
    let display = std::iter::once(cmd.get_program())
        .chain(cmd.get_args())
        .map(|a| a.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ");

    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return Determination::undetermined(format!(
                "cannot run {display}: {e} — a checker that never started has not passed"
            ))
        }
    };

    match child.wait_timeout(timeout) {
        Ok(Some(status)) => {
            let Some(code) = status.code() else {
                return Determination::undetermined(format!(
                    "{display} terminated without an exit code (signal); there is no result to \
                     read"
                ));
            };
            let stdout = read_pipe_bounded(child.stdout.take(), timeout);
            let stderr = read_pipe_bounded(child.stderr.take(), timeout);
            Determination::known(CommandOutput {
                code,
                stdout,
                stderr,
                display,
            })
        }
        Ok(None) => {
            kill_process_tree(&mut child);
            let _ = child.wait();
            Determination::undetermined(format!(
                "{display} timed out after {timeout:?} — a checker that did not finish has not \
                 passed"
            ))
        }
        Err(e) => Determination::undetermined(format!(
            "cannot wait on {display}: {e} — a checker whose status could not be observed has \
             not passed"
        )),
    }
}

/// Kill a timed-out child's whole process tree, not just the direct process.
///
/// On Unix, [`run_with_timeout`] puts the child in its own process group via
/// `process_group(0)` at spawn time, so a negative-pid `SIGKILL` targets the
/// whole group in one call — the direct child *and* anything it exec'd or
/// backgrounded. `child.kill()` is always also called: a harmless no-op if
/// the group kill already reaped it, and the only mechanism at all on
/// non-Unix platforms where there is no process group to target.
fn kill_process_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        let pid = child.id() as i32;
        // SAFETY: `kill` is a plain libc syscall; passing a negative pid
        // targets the process group whose id equals the child's pid (valid
        // because the group was created via `process_group(0)` at spawn
        // time above). Best-effort cleanup on a timeout path: any error
        // (e.g. the group already gone) is intentionally ignored, exactly
        // as the `let _ = child.kill()` fallback below already tolerates.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    let _ = child.kill();
}

/// Read a child's stdout/stderr pipe to completion, but never block past
/// `timeout`: the read happens on a background thread, joined with a bound
/// instead of calling `read_to_string` inline. If the join itself times out
/// (some lingering process still holds the pipe's write end open), whatever
/// was read so far is returned rather than hanging further — the read
/// thread is detached and leaked in that case. This is a best-effort
/// completion of an already-`Known` result (the exit status, which drives
/// the `Determination`, has already been observed by the caller); it does
/// not itself decide `Known` vs `Undetermined`.
fn read_pipe_bounded<R: io::Read + Send + 'static>(pipe: Option<R>, timeout: Duration) -> String {
    use std::sync::mpsc;
    let Some(mut p) = pipe else {
        return String::new();
    };
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut out = String::new();
        let _ = p.read_to_string(&mut out);
        let _ = tx.send(out);
    });
    rx.recv_timeout(timeout).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    /// Unwrap a `Known`, panicking loudly on `Undetermined`. Written here rather
    /// than reached for on `Determination` on purpose: the type has no such
    /// extractor, and a test that wants one must say so in the test file.
    #[track_caller]
    fn expect_known<T>(d: Determination<T>) -> T {
        match d {
            Determination::Known(v) => v,
            Determination::Undetermined(why) => {
                panic!("expected Known, got Undetermined: {why}")
            }
        }
    }

    /// Assert `Undetermined` and hand back the reason text so a test can also
    /// check that the reason names the path/cause instead of being generic.
    #[track_caller]
    fn expect_undetermined<T: std::fmt::Debug>(d: Determination<T>) -> String {
        match d {
            Determination::Undetermined(why) => why.as_str().to_string(),
            Determination::Known(v) => panic!("expected Undetermined, got Known({v:?})"),
        }
    }

    #[cfg(unix)]
    fn chmod(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
            .unwrap_or_else(|e| panic!("chmod {mode:o} {}: {e}", path.display()));
    }

    // ---- read_dir_entries -------------------------------------------------

    #[test]
    fn read_dir_entries_missing_path_is_known_empty() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("no-such-subdir");
        assert!(!missing.exists());
        let entries = expect_known(read_dir_entries(&missing));
        assert!(
            entries.is_empty(),
            "a path that does not exist really is empty: {entries:?}"
        );
    }

    #[test]
    fn read_dir_entries_lists_every_entry_sorted_by_file_name() {
        let dir = tempdir().unwrap();
        // Created out of order, and one of them is a directory, to show the
        // listing is not filtered to files.
        for name in ["zeta.txt", "alpha.txt", "middle.txt"] {
            fs::write(dir.path().join(name), b"x").unwrap();
        }
        fs::create_dir(dir.path().join("beta-dir")).unwrap();

        let entries = expect_known(read_dir_entries(dir.path()));
        let names: Vec<String> = entries
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            vec!["alpha.txt", "beta-dir", "middle.txt", "zeta.txt"],
            "entries must come back complete and sorted by file name"
        );
        // Paths are rooted at the directory that was asked about.
        for p in &entries {
            assert_eq!(p.parent(), Some(dir.path()), "{p:?} should sit under dir");
        }
    }

    /// The central claim: a directory that exists but cannot be read is NOT an
    /// empty directory. Requires a non-root uid — root bypasses the mode bits,
    /// so this test is skipped there rather than asserting something false.
    #[cfg(unix)]
    #[test]
    fn read_dir_entries_unreadable_dir_is_undetermined_not_empty() {
        let dir = tempdir().unwrap();
        let locked = dir.path().join("locked");
        fs::create_dir(&locked).unwrap();
        fs::write(locked.join("secret.txt"), b"contents").unwrap();
        chmod(&locked, 0o000);

        // If the mode bits do not actually deny us (root, or an exotic fs), the
        // premise of the test is absent — say so instead of asserting the wrong
        // thing.
        let denied = fs::read_dir(&locked).is_err();
        let result = read_dir_entries(&locked);
        chmod(&locked, 0o755); // restore before any assert so tempdir can clean up
        assert!(
            denied,
            "precondition: chmod 000 must deny this uid (running as root?)"
        );

        let why = expect_undetermined(result);
        assert!(
            why.contains("locked"),
            "the reason must name the path it could not read: {why}"
        );
    }

    // UNCOVERED BRANCH — stated, not hidden: the mid-iteration `Err(e)` arm of
    // `read_dir_entries` (the "partial listing" guard) has NO test here, because
    // no deterministic way to make `ReadDir::next()` yield an `Err` was found on
    // this platform. Two routes were measured on macOS/APFS, both with the
    // handle already open: unlinking every entry and rmdir'ing the directory
    // mid-iteration, and chmod 000 on the directory mid-iteration. Both returned
    // errs=0 / oks=199 — readdir(3) kept streaming the cached block. Rewriting
    // that arm to `continue` therefore leaves this suite fully green, so nothing
    // below defends it. Covering it needs a fault-injecting filesystem (FUSE) or
    // a seam that lets a test hand in a failing iterator.

    // ---- read_to_string ---------------------------------------------------

    #[test]
    fn read_to_string_missing_file_is_known_none() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("nope.txt");
        assert_eq!(
            expect_known(read_to_string(&missing)),
            None,
            "a missing file is Known(None) — not Undetermined, not Known(Some(..))"
        );
    }

    #[test]
    fn read_to_string_real_file_is_known_some_contents() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("f.txt");
        fs::write(&f, "line one\nline two\n").unwrap();
        assert_eq!(
            expect_known(read_to_string(&f)),
            Some("line one\nline two\n".to_string())
        );
    }

    /// An empty file and an unreadable file must not produce the same answer.
    #[test]
    fn read_to_string_empty_file_is_known_empty_string() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("empty.txt");
        fs::write(&f, "").unwrap();
        assert_eq!(expect_known(read_to_string(&f)), Some(String::new()));
    }

    #[cfg(unix)]
    #[test]
    fn read_to_string_unreadable_file_is_undetermined_not_none() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("locked.txt");
        fs::write(&f, "secret").unwrap();
        chmod(&f, 0o000);

        let denied = fs::read_to_string(&f).is_err();
        let result = read_to_string(&f);
        chmod(&f, 0o644);
        assert!(
            denied,
            "precondition: chmod 000 must deny this uid (running as root?)"
        );

        let why = expect_undetermined(result);
        assert!(
            why.contains("locked.txt"),
            "the reason must name the unreadable file: {why}"
        );
    }

    // ---- run --------------------------------------------------------------

    #[test]
    fn run_missing_binary_is_undetermined() {
        let mut cmd = Command::new("harness-core-no-such-binary-9d1f");
        let why = expect_undetermined(run(&mut cmd));
        assert!(
            why.contains("harness-core-no-such-binary-9d1f"),
            "the reason must name the program: {why}"
        );
    }

    #[test]
    fn run_exit_zero_is_known_and_yields_stdout() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("printf 'hello\n'");
        let out = expect_known(run(&mut cmd));
        assert_eq!(out.code(), 0);
        assert_eq!(expect_known(out.stdout_on_success()), "hello\n");
    }

    /// A process that ran and exited non-zero is a *conclusion* — `Known` — but
    /// its stdout is only readable by a caller that named the code.
    #[test]
    fn run_nonzero_exit_is_known_but_stdout_stays_gated_by_the_code() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg("printf 'partial'; echo 'boom' >&2; exit 1");
        let out = expect_known(run(&mut cmd));
        assert_eq!(out.code(), 1, "the exit code survives to the caller");
        assert!(out.stderr().contains("boom"));

        // Not named as acceptable -> the output is not readable.
        let why = expect_undetermined(out.clone().stdout_on_success());
        assert!(
            why.contains("exited 1"),
            "the reason must state the code it refused: {why}"
        );
        assert!(
            why.contains("boom"),
            "the reason should carry the stderr tail: {why}"
        );

        // Named as acceptable -> the output is readable.
        assert_eq!(expect_known(out.stdout_allowing(&[1])), "partial");
    }

    #[test]
    fn stdout_allowing_refuses_codes_the_caller_did_not_name() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("printf 'x'; exit 2");
        let out = expect_known(run(&mut cmd));
        assert_eq!(out.code(), 2);
        // Allowing 0 and 1 must not admit 2.
        let why = expect_undetermined(out.stdout_allowing(&[0, 1]));
        assert!(why.contains("exited 2"), "{why}");
    }

    #[cfg(unix)]
    #[test]
    fn run_killed_by_signal_is_undetermined() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("kill -9 $$");
        let why = expect_undetermined(run(&mut cmd));
        assert!(
            why.contains("without an exit code"),
            "a signalled process has no result to read: {why}"
        );
    }

    // ---- run_with_timeout ---------------------------------------------------

    #[test]
    fn run_with_timeout_missing_binary_is_undetermined() {
        let mut cmd = Command::new("harness-core-no-such-binary-9d1f");
        let why = expect_undetermined(run_with_timeout(&mut cmd, Duration::from_secs(5)));
        assert!(
            why.contains("harness-core-no-such-binary-9d1f"),
            "the reason must name the program: {why}"
        );
    }

    #[test]
    fn run_with_timeout_completes_within_budget_is_known() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("printf 'hello\n'; echo warn >&2; exit 0");
        let out = expect_known(run_with_timeout(&mut cmd, Duration::from_secs(5)));
        assert_eq!(out.code(), 0);
        assert_eq!(expect_known(out.clone().stdout_on_success()), "hello\n");
        assert!(out.stderr().contains("warn"));
    }

    #[test]
    fn run_with_timeout_nonzero_exit_within_budget_is_known() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("printf 'partial'; exit 7");
        let out = expect_known(run_with_timeout(&mut cmd, Duration::from_secs(5)));
        assert_eq!(out.code(), 7);
        assert_eq!(expect_known(out.stdout_allowing(&[7])), "partial");
    }

    #[cfg(unix)]
    #[test]
    fn run_with_timeout_killed_by_signal_is_undetermined() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("kill -9 $$");
        let why = expect_undetermined(run_with_timeout(&mut cmd, Duration::from_secs(5)));
        assert!(
            why.contains("without an exit code"),
            "a signalled process has no result to read: {why}"
        );
    }

    /// The central claim of this primitive: a hung process must not block
    /// past `timeout`, and the process (and any process-group descendants)
    /// must actually be gone afterward — not merely abandoned to linger in
    /// the background while the call returns.
    #[cfg(unix)]
    #[test]
    fn run_with_timeout_kills_hung_process_group_and_returns_promptly() {
        // A marker unique to this test run's pid, so the liveness probe below
        // cannot match anything except the actual descendant sleep — not
        // some unrelated process, and not the probe command's own argv (a
        // literal like "sleep 30" would self-match a `pgrep -f 'sleep 30'`
        // invocation, since `-f` matches the whole command line).
        let marker = format!("harness-core-boundary-test-marker-{}", std::process::id());

        let mut cmd = Command::new("sh");
        // Backgrounds a grandchild sleep inside its own subshell, standing in
        // for a shell-wrapped checker that execs/backgrounds real work: only
        // a process-group kill (not a plain `child.kill()` on the direct
        // child) reaches it. Both the direct child and the backgrounded
        // grandchild are themselves `sh -c "sleep 30"` processes, so the
        // marker rides as a harmless extra path component of an `sh -c`
        // invocation (arg0's script text) rather than a bogus argument to
        // `sleep`: it shows up in `pgrep -f`'s view of the command line
        // without perturbing what `sleep` parses.
        cmd.arg("-c").arg(format!(
            "(sh -c 'sleep 30' {marker} &) ; sh -c 'sleep 30' {marker}"
        ));

        let start = std::time::Instant::now();
        let why = expect_undetermined(run_with_timeout(&mut cmd, Duration::from_millis(300)));
        let elapsed = start.elapsed();

        assert!(
            why.contains("timed out"),
            "the reason must say this was a timeout: {why}"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "run_with_timeout must return promptly on timeout, took {elapsed:?}"
        );

        // Give the group-kill a moment to actually land, then confirm no
        // process carrying the marker survived — proving the whole tree was
        // reaped, not just the immediate `sh`. `pgrep -f marker` cannot match
        // this very probe command (its argv is `pgrep -f <marker>`, i.e. the
        // marker appears once as pgrep's own pattern argument, and pgrep
        // excludes itself from its own results by default) nor any process
        // started before this test picked its pid-derived marker.
        std::thread::sleep(Duration::from_millis(200));
        let still_running = expect_known(run(Command::new("pgrep").arg("-fc").arg(&marker)));
        // `pgrep -c` prints a count and exits 1 when the count is 0 — so read
        // stdout regardless of the exit code rather than only on success.
        let count: i64 = expect_known(still_running.stdout_allowing(&[0, 1]))
            .trim()
            .parse()
            .unwrap_or(-1);
        assert_eq!(
            count, 0,
            "the process group (including the backgrounded grandchild carrying marker {marker}) \
             must be killed, not left running"
        );
    }
}
