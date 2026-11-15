//! A bounded replacement for `spawn` + `write stdin` + `wait_with_output`.
//!
//! `wait_with_output` waits forever. A hook binary that never exits therefore
//! hangs the test that spawned it and leaves the child behind as an orphan (an
//! orphaned busy-loop was observed living 8h45m). Every test spawn of the
//! blastguard binary (and of its launcher) goes through [`run_bounded`], so a
//! hang becomes a panic naming the deadline and the phase instead.
//!
//! Include it with `#[path = "support/bounded.rs"] mod bounded;`.

use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

/// The deadline the hook-binary tests pass to [`run_bounded`]. One hook
/// invocation normally takes milliseconds; this is the hang bound, not a
/// performance budget.
#[allow(dead_code)] // a file that passes its own deadline does not use it
pub const HOOK_DEADLINE: Duration = Duration::from_secs(20);

/// The deadline for the fixture tools a test runs (`git`, `sh`, ...). Like
/// [`HOOK_DEADLINE`] it is the hang bound, not a performance budget.
#[allow(dead_code)] // a file that runs no fixture tool does not use it
pub const TOOL_DEADLINE: Duration = Duration::from_secs(60);

/// What the child's stdin is connected to, for [`run_bounded_stdin`].
#[allow(dead_code)] // not every including file uses every variant
#[derive(Clone, Copy, Debug)]
pub enum ChildStdin<'a> {
    /// A pipe that receives these bytes and is then closed ([`run_bounded`]).
    Bytes(&'a [u8]),
    /// `/dev/null`: what `Command::output` sets by default.
    Null,
    /// The test process's own stdin: what `Command::status` sets by default.
    Inherit,
}

/// Spawn `cmd`, feed it `stdin`, wait for it to exit and collect its output,
/// all within one `deadline`. Same as
/// `run_bounded_stdin(cmd, ChildStdin::Bytes(stdin), deadline)`.
#[allow(dead_code)] // a file that only uses run_bounded_stdin does not use it
pub fn run_bounded(cmd: &mut Command, stdin: &[u8], deadline: Duration) -> Output {
    run_bounded_stdin(cmd, ChildStdin::Bytes(stdin), deadline)
}

/// Spawn `cmd` with its stdin connected as `stdin` says, wait for it to exit
/// and collect its output, all within one `deadline`.
///
/// What it does, in order:
///
/// 1. Sets `cmd`'s stdin as `stdin` says (a pipe for `Bytes`, `/dev/null` for
///    `Null`, the test's own stdin for `Inherit`), stdout and stderr to piped,
///    and puts the child in its own process group (`process_group(0)`),
///    overriding whatever the caller set for those. Every other setting of
///    `cmd` (program, args, env, cwd) is used as given.
/// 2. Spawns it; a spawn failure panics. The `deadline` starts right after
///    the spawn and covers every later phase (write, wait, drain).
/// 3. Writes `stdin` to the child's stdin on its own thread, then closes it.
///    Drains stdout and stderr on two more threads, so neither a payload the
///    child does not read nor a large output can block the caller.
/// 4. Polls `try_wait` every 10ms until the child exits or `deadline` passes.
///    A `try_wait` error panics.
/// 5. After the child exits, waits (for what is left of `deadline`) for the
///    stdin write result, then for the stdout drain, then for the stderr
///    drain. A write error panics naming the stdin write, including the case
///    of a child that exited without reading all of its stdin (EPIPE): the
///    caller handed the child input it did not consume, which is reported,
///    not ignored. An empty `Bytes`, `Null` and `Inherit` perform no write and
///    cannot fail.
/// 6. On a timeout in any phase (the child still running, the stdin write
///    still blocked, or a drain still open because a descendant outlived the
///    child and holds the pipe): runs `kill -TERM -- -<pgid>` (the child and
///    every descendant in its group; the `--` is required by procps-ng `kill`
///    on Linux, which otherwise exits 0 and signals nothing), polls `try_wait` for up to 5s to reap the
///    child if it has not exited, then panics with the phase, the deadline, the
///    pgid, the `kill` result and the reap result. A child that ignores
///    SIGTERM is reported in that panic, not waited on. A descendant that left
///    the process group is not signalled.
/// 7. Otherwise returns the exit status with the full stdout and stderr. It
///    does not judge the status; the caller asserts on it.
#[allow(dead_code)] // a file that only uses run_bounded does not use it
pub fn run_bounded_stdin(cmd: &mut Command, stdin: ChildStdin<'_>, deadline: Duration) -> Output {
    run_with(cmd, stdin, false, deadline)
}

/// [`run_bounded`], except that the read end of the child's stdout is closed
/// right after the spawn, before any stdin byte is written: a child that
/// waits for its stdin before printing finds its stdout reader already gone
/// (EPIPE on write). The returned `stdout` is empty. Every other step and
/// guarantee of [`run_bounded_stdin`] applies.
#[allow(dead_code)] // only a closed-stdout test uses it
pub fn run_bounded_closed_stdout(cmd: &mut Command, stdin: &[u8], deadline: Duration) -> Output {
    run_with(cmd, ChildStdin::Bytes(stdin), true, deadline)
}

fn run_with(
    cmd: &mut Command,
    stdin: ChildStdin<'_>,
    close_stdout: bool,
    deadline: Duration,
) -> Output {
    let program = cmd.get_program().to_string_lossy().into_owned();
    let (stdio, stdin): (Stdio, &[u8]) = match stdin {
        ChildStdin::Bytes(b) => (Stdio::piped(), b),
        ChildStdin::Null => (Stdio::null(), &[]),
        ChildStdin::Inherit => (Stdio::inherit(), &[]),
    };
    let mut child = cmd
        .stdin(stdio)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Own process group, so a timeout can signal the child and every
        // descendant it spawned (killing only a `sh` would orphan its children).
        .process_group(0)
        .spawn()
        .unwrap_or_else(|e| panic!("spawn {program}: {e}"));
    let until = Instant::now() + deadline;
    let pgid = child.id();
    if close_stdout {
        // Before the stdin writer starts, so the child cannot print first.
        drop(child.stdout.take());
    }

    // stdin on its own thread: a child that never reads a payload larger than
    // the pipe buffer blocks this write, and the deadline must still apply.
    let (write_tx, write_rx) = mpsc::channel::<std::io::Result<()>>();
    let pipe = child.stdin.take();
    let payload = stdin.to_vec();
    std::thread::spawn(move || {
        let r = match pipe {
            Some(mut p) => p.write_all(&payload),
            None => Ok(()),
        };
        // `p` is dropped here, closing the child's stdin.
        let _ = write_tx.send(r);
    });

    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        let (tx, rx) = mpsc::channel::<std::io::Result<Vec<u8>>>();
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let r = match pipe {
                Some(mut p) => p.read_to_end(&mut buf).map(|_| buf),
                None => Ok(buf),
            };
            let _ = tx.send(r);
        });
        rx
    };
    let out_rx = drain(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let err_rx = drain(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );

    let status = loop {
        if let Some(st) = child
            .try_wait()
            .unwrap_or_else(|e| panic!("try_wait on {program}: {e}"))
        {
            break st;
        }
        if Instant::now() >= until {
            let phase = match write_rx.try_recv() {
                Err(mpsc::TryRecvError::Empty) => {
                    "waiting for exit (stdin write still blocked: the child is not reading it)"
                }
                _ => "waiting for exit",
            };
            timed_out(&mut child, None, &program, phase, deadline, pgid);
        }
        std::thread::sleep(Duration::from_millis(10));
    };

    match recv_until(&write_rx, until) {
        Ok(Ok(())) => {}
        Ok(Err(e)) => panic!(
            "stdin write of {} bytes to {program} failed: {e} (exit status {status})",
            stdin.len()
        ),
        Err(RecvTimeoutError::Timeout) => timed_out(
            &mut child,
            Some(status),
            &program,
            "writing stdin",
            deadline,
            pgid,
        ),
        Err(RecvTimeoutError::Disconnected) => {
            panic!("stdin writer thread for {program} died without a result")
        }
    }
    let mut collect = |rx: &Receiver<std::io::Result<Vec<u8>>>, name: &str| -> Vec<u8> {
        match recv_until(rx, until) {
            Ok(Ok(buf)) => buf,
            Ok(Err(e)) => panic!("reading {name} of {program} failed: {e}"),
            Err(RecvTimeoutError::Timeout) => timed_out(
                &mut child,
                Some(status),
                &program,
                &format!("draining {name} (a descendant may hold it open)"),
                deadline,
                pgid,
            ),
            Err(RecvTimeoutError::Disconnected) => {
                panic!("{name} reader thread for {program} died without a result")
            }
        }
    };
    let stdout = collect(&out_rx, "stdout");
    let stderr = collect(&err_rx, "stderr");
    Output {
        status,
        stdout,
        stderr,
    }
}

/// Receive on `rx` until the absolute instant `until`.
fn recv_until<T>(rx: &Receiver<T>, until: Instant) -> Result<T, RecvTimeoutError> {
    rx.recv_timeout(until.saturating_duration_since(Instant::now()))
}

/// SIGTERM the whole process group, reap the child within a bounded window
/// (unless it already exited with `exited`), then panic naming `phase`.
fn timed_out(
    child: &mut Child,
    exited: Option<ExitStatus>,
    program: &str,
    phase: &str,
    deadline: Duration,
    pgid: u32,
) -> ! {
    // `--` before the negative pgid: procps-ng `kill` (Linux) reads `-<pgid>`
    // without it as an option, exits 0 and signals nothing. POSIX `kill`
    // (BSD/macOS too) accepts `--` as the end of options.
    let killed = Command::new("kill")
        .arg("-TERM")
        .arg("--")
        .arg(format!("-{pgid}"))
        .status();
    let reaped = match exited {
        Some(st) => Some(st),
        None => {
            // A child that ignores SIGTERM is reported, not waited on forever.
            let reap_deadline = Instant::now() + Duration::from_secs(5);
            loop {
                match child.try_wait() {
                    Ok(Some(st)) => break Some(st),
                    Ok(None) if Instant::now() < reap_deadline => {
                        std::thread::sleep(Duration::from_millis(20))
                    }
                    _ => break None,
                }
            }
        }
    };
    panic!(
        "{program} timed out {phase}: not done within {deadline:?}; \
         sent SIGTERM to process group {pgid} (kill: {killed:?}); reaped: {reaped:?}"
    );
}
