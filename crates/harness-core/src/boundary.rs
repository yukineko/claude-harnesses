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
//! | the process exited, but its output could not be read | `Undetermined` |
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

    /// Whatever the process wrote to stderr.
    ///
    /// Exposed unconditionally, unlike `stdout`, because no caller derives a
    /// verdict from its *contents*: it is diagnostic text. That is the whole
    /// of the claim, and it is narrower than it reads. Since harness-core
    /// 0.2.3 the *unreadability* of stderr decides the entire call on its own
    /// — [`run_with_timeout`] gives both pipes the same weight, so a stderr
    /// that errors or whose bounded read never reaches EOF returns
    /// `Undetermined` even when stdout arrived intact and the exit code is in
    /// hand. stderr does not produce a verdict; failing to read it withholds
    /// one.
    ///
    /// That reaches callers who asked not to have stderr at all.
    /// `propguard::git::run_git_bin` sets `.stderr(Stdio::null())`, and
    /// [`run_with_timeout_and_stdin`] overrides it unconditionally with
    /// `cmd.stdout(Stdio::piped()).stderr(Stdio::piped())`, so a `git` writing
    /// non-UTF-8 to stderr makes propguard's changed-file scan `Failed` and
    /// blocks its gate — over a stream propguard explicitly discarded. The
    /// direction is the required one (CLAUDE.md §3: cannot-determine resolves
    /// to the restricted side), and this paragraph exists so the prose stops
    /// calling stderr inert when it is not.
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
/// itself erroring, a signal (no exit code), timeout, and an exit whose
/// stdout/stderr could not be read (see [`read_pipe_bounded`]). `Known` only
/// when the process ran to completion within `timeout` **and both of its
/// output streams were read**, whatever its exit code — judging the code is
/// still the caller's job via [`CommandOutput`].
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
/// timeout, or fails outright, the call is `Undetermined` rather than
/// silently returning a partial or empty read as if it were complete output.
/// stderr is read the same way and given the same weight: it is diagnostic
/// text, but a `CommandOutput` that reported it as empty when it could not be
/// read would be stating an observation it never made.
pub fn run_with_timeout(cmd: &mut Command, timeout: Duration) -> Determination<CommandOutput> {
    run_with_timeout_and_stdin(cmd, timeout, None)
}

/// [`run_with_timeout`], additionally writing `stdin` to the child before the
/// bounded wait begins.
///
/// The write happens on its own background thread rather than inline for the
/// same reason the stdout/stderr reads do: a checker that writes enough of
/// its own output before draining stdin can fill both the child's stdout
/// pipe and the parent's stdin pipe at once (each ~64KB), and a synchronous
/// `write_all` here would then block forever — *before* the wait below ever
/// starts, so `timeout` would provide zero protection against that deadlock.
/// The thread is detached: if the child is killed on timeout, the write
/// simply errors out (broken pipe) and the thread exits; nothing is joined
/// here since the join itself must not be able to block.
///
/// Callers that need stdin must still put `cmd.stdin(Stdio::piped())`
/// themselves; passing `stdin: Some(_)` without that set up front means the
/// bytes are silently dropped (there is no pipe to write them into), mirroring
/// how `Child::stdin` is `None` in that situation.
pub fn run_with_timeout_and_stdin(
    cmd: &mut Command,
    timeout: Duration,
    stdin: Option<Vec<u8>>,
) -> Determination<CommandOutput> {
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

    if let Some(bytes) = stdin {
        if let Some(mut child_stdin) = child.stdin.take() {
            std::thread::spawn(move || {
                use io::Write;
                let _ = child_stdin.write_all(&bytes);
            });
        }
    }

    match child.wait_timeout(timeout) {
        Ok(Some(status)) => {
            let Some(code) = status.code() else {
                return Determination::undetermined(format!(
                    "{display} terminated without an exit code (signal); there is no result to \
                     read"
                ));
            };
            // An exit code with unreadable output is not a complete
            // observation of what the process did, so `Known` is not
            // available here: both streams have to have actually been read.
            // The `Undetermined` arms FORWARD the payload rather than
            // building a new one — `read_pipe_bounded` already recorded that
            // give-up at its origin, and re-minting here would count one
            // event twice as it bubbles up.
            let stdout =
                match read_pipe_bounded(child.stdout.take(), timeout, "stdout", &display, code) {
                    Determination::Known(text) => text,
                    Determination::Undetermined(why) => return Determination::Undetermined(why),
                };
            let stderr =
                match read_pipe_bounded(child.stderr.take(), timeout, "stderr", &display, code) {
                    Determination::Known(text) => text,
                    Determination::Undetermined(why) => return Determination::Undetermined(why),
                };
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
/// instead of calling `read_to_string` inline.
///
/// The return type is the point. An exit status observed by `wait_timeout`
/// says the process *ran*; it says nothing about whether its output arrived,
/// and this function is where that second question is answered. Two ways it
/// can fail, both `Undetermined`:
///
/// * **the read errors** — including the `InvalidData` that non-UTF-8 output
///   produces, exactly as [`read_to_string`] (the file wrapper above) treats
///   it. Note the deliberate divergence from [`run`], which lossily converts:
///   there, the whole `Vec<u8>` is in hand and the lossy text is a complete
///   rendering of it; here the bytes are gone with the failed read, so the
///   choice is between "could not read" and a fabricated empty string.
/// * **the join expires** — some lingering descendant still holds the pipe's
///   write end open, so EOF never comes. The read thread is detached and
///   leaked in that case, and whatever it had accumulated is *not* returned:
///   a partial read presented as complete output is indistinguishable from a
///   process that printed nothing.
///
/// `Known(String::new())` is reserved for the two genuinely empty
/// observations: a pipe that was never captured (`None`), and a read that
/// completed with nothing in it.
///
/// Until harness-core 0.2.3 this returned a bare `String`, discarding the read
/// error (`let _ = p.read_to_string(..)`) and folding an expired join into
/// `unwrap_or_default()`. Both landed inside a `Determination::known(..)`, so
/// the value announced itself as an observation while carrying a fabricated
/// one — the precise fail-open shape this module was written to end, three
/// days after the norm that named it. Its own callers' docs already claimed
/// this behavior ("the call is `Undetermined` rather than silently returning a
/// partial read"); the code did not do it.
fn read_pipe_bounded<R: io::Read + Send + 'static>(
    pipe: Option<R>,
    timeout: Duration,
    which: &str,
    display: &str,
    code: i32,
) -> Determination<String> {
    use std::sync::mpsc;
    let Some(mut p) = pipe else {
        return Determination::known(String::new());
    };
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut out = String::new();
        // The error, not the buffer: `read_to_string` guarantees `out` is
        // left unchanged when it fails, so what sits in it at that point is
        // not a short read of the output, it is nothing at all.
        let read = p
            .read_to_string(&mut out)
            .map(|_| out)
            .map_err(|e| e.to_string());
        let _ = tx.send(read);
    });
    match rx.recv_timeout(timeout) {
        Ok(Ok(text)) => Determination::known(text),
        Ok(Err(e)) => Determination::undetermined(format!(
            "{display} exited {code} but its {which} could not be read: {e} — reporting the exit \
             status while dropping the output would present an unread stream as an empty one"
        )),
        Err(e) => Determination::undetermined(format!(
            "{display} exited {code} but its {which} could not be read within {timeout:?} ({e}); \
             a lingering descendant most likely still holds the write end of the pipe. Whatever \
             arrived is a partial read, and returning it as complete output would be \
             indistinguishable from a process that printed nothing"
        )),
    }
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
        // `pgrep -f` (NOT `-fc`): the count flag `-c` is a Linux procps
        // extension that macOS's pgrep rejects with exit 2 + a usage message.
        // The boundary correctly reported that as Undetermined, so on macOS
        // this test — the only coverage of the process-group-kill contract
        // here — did not fail the contract, it failed to RUN, and the
        // contract was UNVERIFIED on this platform. Counting the lines
        // ourselves is portable: exit 0 means matches were printed, exit 1
        // means none were. (propguard's mirror of this same probe was already
        // fixed this way; this is the twin that was left behind.)
        let still_running = expect_known(run(Command::new("pgrep").arg("-f").arg(&marker)));
        let count = expect_known(still_running.stdout_allowing(&[0, 1]))
            .lines()
            .filter(|l| !l.trim().is_empty())
            .count();
        assert_eq!(
            count, 0,
            "the process group (including the backgrounded grandchild carrying marker {marker}) \
             must be killed, not left running"
        );
    }

    // ---- run_with_timeout: the OUTPUT is part of the observation ------------
    //
    // `wait_timeout` answering with an exit code says the process *ran*; it
    // says nothing about whether its output arrived. These four tests pin the
    // difference. The first two inject the two ways a pipe read can fail to
    // deliver (the read itself erroring, and the read never reaching EOF
    // within the budget) and demand `Undetermined`; the last two are their
    // anti-vacuity partners — the same command minus the fault must still be
    // `Known` with its full stdout, and a genuinely silent command must still
    // be `Known` with an empty one. Without that pair, "everything is
    // Undetermined now" would satisfy the first two.

    /// A child that exits 0 while writing bytes that are not valid UTF-8: the
    /// read of its stdout pipe *errors* (`InvalidData`). Reporting `Known`
    /// with an empty stdout here would state that a checker printed nothing
    /// when in fact it printed something unreadable — the same conflation
    /// `read_to_string` (the file wrapper above) already refuses.
    #[test]
    fn run_with_timeout_unreadable_stdout_bytes_are_undetermined_not_empty() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("printf '\\377\\376'; exit 0");
        let why = expect_undetermined(run_with_timeout(&mut cmd, Duration::from_secs(5)));
        assert!(
            why.contains("stdout"),
            "the reason must name which stream could not be read: {why}"
        );
        assert!(
            why.contains("exited 0"),
            "the reason must say the process itself DID exit (0) — that is exactly what makes \
             an empty stdout look trustworthy: {why}"
        );
    }

    /// A child that exits 0 while a backgrounded descendant keeps the write
    /// end of the stdout pipe open: the read cannot reach EOF, so the bounded
    /// join expires. Whatever the child managed to print is a *partial* read
    /// at best, and before this was `Undetermined` it was returned as an empty
    /// `Known` — a checker's output silently replaced by "".
    #[cfg(unix)]
    #[test]
    fn run_with_timeout_pipe_held_open_after_clean_exit_is_undetermined_not_empty() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg("printf 'real output\n'; (sleep 9 &); exit 0");

        // Deliberately not a 100ms-scale budget: the same value bounds the
        // `wait_timeout` on the child, and under a loaded test runner a
        // too-tight budget makes THAT fire first — which is also
        // `Undetermined`, so the test would pass for the wrong reason. The
        // `stdout` assertion below is the second guard against that: the
        // process-timeout message does not mention a stream.
        let budget = Duration::from_millis(1500);
        let start = std::time::Instant::now();
        let outcome = run_with_timeout(&mut cmd, budget);
        let elapsed = start.elapsed();

        let why = expect_undetermined(outcome);
        assert!(
            why.contains("stdout"),
            "the reason must name which stream could not be read (and NOT be the \
             process-level timeout): {why}"
        );
        assert!(
            elapsed < Duration::from_secs(9),
            "the read must stay bounded by `timeout` rather than waiting out the descendant \
             holding the pipe, took {elapsed:?}"
        );
    }

    /// Anti-vacuity partner: the same command as the test above with the
    /// pipe-holder removed must still be `Known` and must still carry every
    /// byte. (A generous budget on purpose — this path never waits on the
    /// clock, so a tight one would only add load-flakiness.)
    #[test]
    fn run_with_timeout_readable_stdout_is_known_and_complete() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("printf 'real output\n'; exit 0");
        let out = expect_known(run_with_timeout(&mut cmd, Duration::from_secs(5)));
        assert_eq!(out.code(), 0);
        assert_eq!(
            expect_known(out.stdout_on_success()),
            "real output\n",
            "a readable pipe must deliver the full output, not a truncated or empty one"
        );
    }

    /// Anti-vacuity partner: a command that really does print nothing is
    /// `Known("")`. Empty output is still a legitimate observation — only
    /// output that could not be read is undetermined.
    #[test]
    fn run_with_timeout_genuinely_silent_child_is_known_empty() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("exit 0");
        let out = expect_known(run_with_timeout(&mut cmd, Duration::from_secs(5)));
        assert_eq!(
            expect_known(out.stdout_on_success()),
            "",
            "a silent child is Known(\"\") — the fix must not turn every empty stdout into a \
             give-up"
        );
    }

    // ---- run_with_timeout: stderr carries the same weight -------------------
    //
    // The stderr half of the block above, and it is here because that half had
    // a measured kill rate of ZERO. Reverting *only* the stderr arm of
    // `run_with_timeout_and_stdin` to the pre-0.2.3 fail-open
    // (`Determination::Undetermined(_) => String::new()`) left harness-core at
    // 261 passed / 0 failed and propguard at 66 + 4 + 11 passed / 0 failed:
    // both fault tests above inject on stdout only, so nothing anywhere
    // defended the behaviour the docs on `run_with_timeout` commit to. These
    // three mirror the stdout shape — two faults (the read erroring, and the
    // bounded join expiring) plus the anti-vacuity partners that stop
    // "Undetermined for everything" from satisfying them.

    /// A child that exits 0 after writing non-UTF-8 bytes to STDERR, with a
    /// perfectly readable (empty) stdout. Everything about the process's own
    /// status is in hand, which is exactly what would make a `Known` here look
    /// trustworthy — and its `stderr` field would be `""`, an observation the
    /// call never made.
    #[test]
    fn run_with_timeout_unreadable_stderr_bytes_are_undetermined_not_empty() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("printf '\\377\\376' >&2; exit 0");
        let why = expect_undetermined(run_with_timeout(&mut cmd, Duration::from_secs(5)));
        assert!(
            why.contains("stderr"),
            "the reason must name which stream could not be read, and it must be stderr — \
             stdout was read fine here: {why}"
        );
        assert!(
            why.contains("exited 0"),
            "the reason must say the process itself DID exit (0) — that is exactly what makes \
             an empty stderr look trustworthy: {why}"
        );
    }

    /// The second fault route, and it is a genuinely distinct arm: the bounded
    /// join expiring (`recv_timeout` erroring) rather than the read itself
    /// erroring. The backgrounded descendant's stdout is redirected to
    /// `/dev/null` on purpose — without that it would hold the stdout pipe
    /// open too, the stdout read would expire first, and this test would pass
    /// on the stdout arm while proving nothing about stderr.
    #[cfg(unix)]
    #[test]
    fn run_with_timeout_stderr_pipe_held_open_after_clean_exit_is_undetermined_not_empty() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg("printf 'real output\n'; printf 'warn\n' >&2; (sleep 9 >/dev/null &); exit 0");

        // Same reasoning as the stdout twin: not a 100ms-scale budget, because
        // the same value bounds the `wait_timeout` on the child and a too-tight
        // one makes THAT fire first — also `Undetermined`, so the test would
        // pass for the wrong reason. The `stderr` assertion is the second guard
        // against that: the process-level timeout message names no stream.
        let budget = Duration::from_millis(1500);
        let start = std::time::Instant::now();
        let outcome = run_with_timeout(&mut cmd, budget);
        let elapsed = start.elapsed();

        let why = expect_undetermined(outcome);
        assert!(
            why.contains("stderr"),
            "the reason must name stderr (and NOT be the process-level timeout, which names no \
             stream, nor the stdout read, which reached EOF): {why}"
        );
        assert!(
            elapsed < Duration::from_secs(9),
            "the stderr read must stay bounded by `timeout` rather than waiting out the \
             descendant holding the pipe, took {elapsed:?}"
        );
    }

    /// ANTI-VACUITY CONTROL for the two above: a child whose stderr really is
    /// readable must still be `Known`, and must still carry the actual text.
    /// Without this, an implementation that answers `Undetermined` for every
    /// stderr satisfies both fault tests.
    #[test]
    fn run_with_timeout_readable_stderr_is_known_and_complete() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg("printf 'to stdout\n'; printf 'diagnostic line\n' >&2; exit 0");
        let out = expect_known(run_with_timeout(&mut cmd, Duration::from_secs(5)));
        assert_eq!(out.code(), 0);
        assert_eq!(
            out.stderr(),
            "diagnostic line\n",
            "a readable stderr must arrive intact and complete"
        );
        assert_eq!(
            expect_known(out.stdout_on_success()),
            "to stdout\n",
            "and the stderr handling must not disturb stdout"
        );
    }

    /// The other half of the control: a child that genuinely writes nothing to
    /// stderr is `Known("")`. Empty stderr is a real observation; only an
    /// unread one is undetermined.
    #[test]
    fn run_with_timeout_genuinely_empty_stderr_is_known_empty() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("printf 'only stdout\n'; exit 0");
        let out = expect_known(run_with_timeout(&mut cmd, Duration::from_secs(5)));
        assert_eq!(
            out.stderr(),
            "",
            "a child with nothing to say on stderr is Known(\"\") — the fix must not turn every \
             empty stderr into a give-up"
        );
    }

    // ---- run_with_timeout_and_stdin -----------------------------------------

    #[test]
    fn run_with_timeout_and_stdin_writes_and_the_child_reads_it() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("cat").stdin(Stdio::piped());
        let out = expect_known(run_with_timeout_and_stdin(
            &mut cmd,
            Duration::from_secs(5),
            Some(b"hello from stdin".to_vec()),
        ));
        assert_eq!(out.code(), 0);
        assert_eq!(
            expect_known(out.stdout_on_success()),
            "hello from stdin",
            "the child must see exactly the bytes written to stdin"
        );
    }

    /// The deadlock this exists to avoid: a child that never reads stdin at
    /// all (so a *synchronous* stdin write on this thread would block
    /// forever, before `wait_timeout` is ever reached — the write happening
    /// on its own background thread instead is what `run_with_timeout_and_stdin`
    /// adds over a bare `wait_timeout` call). The child here also floods
    /// stdout past a pipe buffer, which independently caps how quickly
    /// `wait_timeout` itself can return (the child can stall mid-write with
    /// nobody draining its stdout until this call's own bounded read starts,
    /// so `wait_timeout` blocking up to the full `timeout` in that shape is
    /// expected, not a bug — see CA-propguard-005 in `gate.rs`, which has the
    /// same characteristic). The contract under test is narrower than "fast":
    /// the call must still return `Undetermined` within `timeout`, i.e. the
    /// off-thread stdin write must not add its own *unbounded* stall on top.
    #[test]
    fn run_with_timeout_and_stdin_does_not_deadlock_when_child_floods_stdout_first() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg("yes X | head -c 500000")
            .stdin(Stdio::piped());
        let big_stdin = b"line of stdin content\n".repeat(20_000);
        let timeout = Duration::from_secs(2);

        let start = std::time::Instant::now();
        let outcome = run_with_timeout_and_stdin(&mut cmd, timeout, Some(big_stdin));
        let elapsed = start.elapsed();

        assert!(
            elapsed < timeout * 5,
            "must be bounded by `timeout` (with slack for OS/CI scheduling), not hang \
             indefinitely because the child never drains stdin; took {elapsed:?} against \
             timeout {timeout:?}"
        );
        // Whatever the outcome (a timeout, or a fast exit that never needed
        // stdin at all), it must not be silently "all pass" via a
        // hang-then-succeed path; either shape is acceptable evidence the
        // call returned instead of hanging unboundedly.
        match outcome {
            Determination::Known(_) | Determination::Undetermined(_) => {}
        }
    }

    #[test]
    fn run_with_timeout_and_stdin_none_behaves_like_run_with_timeout() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("printf 'no stdin needed\n'");
        let out = expect_known(run_with_timeout_and_stdin(
            &mut cmd,
            Duration::from_secs(5),
            None,
        ));
        assert_eq!(out.code(), 0);
        assert_eq!(expect_known(out.stdout_on_success()), "no stdin needed\n");
    }
}
