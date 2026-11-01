//! A bounded replacement for `spawn` + `write stdin` + `wait_with_output`.
//!
//! `wait_with_output` waits forever. A hook binary that never exits therefore
//! hangs the test that spawned it and leaves the child behind as an orphan (an
//! orphaned busy-loop was observed living 8h45m). Every test spawn of the
//! blastguard binary (and of its launcher) goes through [`run_bounded`], so a
//! hang becomes a panic naming the deadline instead.
//!
//! Include it with `#[path = "support/bounded.rs"] mod bounded;`.

use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// The deadline the hook-binary tests pass to [`run_bounded`]. One hook
/// invocation normally takes milliseconds; this is the hang bound, not a
/// performance budget.
#[allow(dead_code)] // a file that passes its own deadline does not use it
pub const HOOK_DEADLINE: Duration = Duration::from_secs(20);

/// Spawn `cmd`, feed it `stdin`, and wait for it to exit, for at most
/// `deadline` of waiting.
///
/// What it does, in order:
///
/// 1. Sets `cmd`'s stdin, stdout and stderr to piped and puts the child in its
///    own process group (`process_group(0)`), overriding whatever the caller
///    set for those. Every other setting of `cmd` (program, args, env, cwd) is
///    used as given.
/// 2. Spawns it; a spawn failure panics.
/// 3. Writes `stdin` to the child's stdin on the calling thread, then closes it.
///    A write error (e.g. the child exited without reading) is ignored. This
///    write is NOT covered by `deadline`: a child that never reads a payload
///    larger than the pipe buffer blocks here.
/// 4. Drains stdout and stderr on two threads, so a large output cannot block
///    the child while it is polled.
/// 5. Polls `try_wait` every 10ms until the child exits or `deadline` (counted
///    from after the stdin write) passes. A `try_wait` error panics.
/// 6. On timeout: runs `kill -TERM -<pgid>` (the child and every descendant in
///    its group), polls `try_wait` for up to 5s to reap it, then panics with
///    the deadline, the pgid, the `kill` result and the reap result. A child
///    that ignores SIGTERM is reported in that panic, not waited on.
/// 7. On exit: joins the two drain threads and returns the exit status with
///    the full stdout and stderr. It does not judge the status; the caller
///    asserts on it. The joins are not bounded: a descendant that outlives the
///    child and keeps a pipe open keeps the join waiting.
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
    if let Some(mut pipe) = child.stdin.take() {
        let _ = pipe.write_all(stdin);
    }
    // Drain both pipes on their own threads so a large output can never
    // block the child while we poll for its exit.
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut buf);
            }
            buf
        })
    };
    let out_reader = drain(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let err_reader = drain(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let until = Instant::now() + deadline;
    let status = loop {
        if let Some(st) = child
            .try_wait()
            .unwrap_or_else(|e| panic!("try_wait on {program}: {e}"))
        {
            break st;
        }
        if Instant::now() >= until {
            let pgid = child.id();
            let killed = Command::new("kill")
                .arg("-TERM")
                .arg(format!("-{pgid}"))
                .status();
            // Reap within a bounded window; a child that ignores SIGTERM is
            // reported, not waited on forever.
            let reap_deadline = Instant::now() + Duration::from_secs(5);
            let reaped = loop {
                match child.try_wait() {
                    Ok(Some(st)) => break Some(st),
                    Ok(None) if Instant::now() < reap_deadline => {
                        std::thread::sleep(Duration::from_millis(20))
                    }
                    _ => break None,
                }
            };
            panic!(
                "{program} did not exit within {deadline:?}; \
                 sent SIGTERM to process group {pgid} (kill: {killed:?}); reaped: {reaped:?}"
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let stdout = out_reader.join().expect("stdout reader thread");
    let stderr = err_reader.join().expect("stderr reader thread");
    Output {
        status,
        stdout,
        stderr,
    }
}
