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

/// Spawn `cmd`, feed it `stdin`, wait for it to exit and collect its output,
/// all within one `deadline`.
///
/// What it does, in order:
///
/// 1. Sets `cmd`'s stdin, stdout and stderr to piped and puts the child in its
///    own process group (`process_group(0)`), overriding whatever the caller
///    set for those. Every other setting of `cmd` (program, args, env, cwd) is
///    used as given.
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
///    not ignored. An empty `stdin` performs no write and cannot fail.
/// 6. On a timeout in any phase (the child still running, the stdin write
///    still blocked, or a drain still open because a descendant outlived the
///    child and holds the pipe): runs `kill -TERM -<pgid>` (the child and
///    every descendant in its group), polls `try_wait` for up to 5s to reap the
///    child if it has not exited, then panics with the phase, the deadline, the
///    pgid, the `kill` result and the reap result. A child that ignores
///    SIGTERM is reported in that panic, not waited on. A descendant that left
///    the process group is not signalled.
/// 7. Otherwise returns the exit status with the full stdout and stderr. It
///    does not judge the status; the caller asserts on it.
pub fn run_bounded(cmd: &mut Command, stdin: &[u8], deadline: Duration) -> Output {
    let program = cmd.get_program().to_string_lossy().into_owned();
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Own process group, so a timeout can signal the child and every
        // descendant it spawned (killing only a `sh` would orphan its children).
        .process_group(0)
        .spawn()
        .unwrap_or_else(|e| panic!("spawn {program}: {e}"));
    let until = Instant::now() + deadline;
    let pgid = child.id();

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
    let killed = Command::new("kill")
        .arg("-TERM")
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
