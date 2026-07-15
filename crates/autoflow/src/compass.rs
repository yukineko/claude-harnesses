//! Read-only compass freshness check.
//!
//! flow's invariant (`flow` SKILL.md §"盲目実行しない"): never auto-drive the
//! queue unless the compass charter is sharp. autoflow's auto-loop drove the
//! backlog (`block: /backlog`) without ever consulting compass — bypassing that
//! gate. This module asks compass for its deterministic C1/C2 freshness verdict
//! (`compass nudge --json`) so autoflow can stand down and nudge the user toward
//! `/compass` instead of blind-driving a stale charter.
//!
//! Soft dependency: if compass isn't installed (or errors / emits garbage) we
//! return `None`, and the caller preserves today's behavior — a repo that
//! doesn't use compass keeps auto-driving as before. This module only READS
//! (shells out to a hook subcommand that always exits 0); it never writes.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use harness_core::config::home;
use serde::Deserialize;
use wait_timeout::ChildExt;

/// Bound on the `compass nudge --json` shell-out below. This is a soft
/// dependency (see module docs): a hung/slow compass binary must never block
/// autoflow's caller indefinitely. Generous enough for normal usage (compass
/// nudge is a fast, local, deterministic check) while still bounding worst case.
const COMPASS_TIMEOUT: Duration = Duration::from_secs(3);

/// compass's machine-readable freshness verdict (`compass nudge --json`).
#[derive(Debug, Deserialize)]
pub struct Verdict {
    /// `true` iff the charter is sharp enough to auto-act on (C1 present +
    /// non-blurry, C2 not drift-suspect).
    pub fresh: bool,
    /// Human-readable nudge text; present iff `!fresh`.
    #[serde(default)]
    pub reason: Option<String>,
}

/// Ask compass whether the charter for the repo containing `cwd` is fresh.
///
/// `None` means "can't tell" — compass absent, errored, or emitted unparseable
/// output — and the caller should preserve its prior behavior (proceed). A
/// `Some(verdict)` carries compass's deterministic answer.
pub fn charter_freshness(cwd: &Path) -> Option<Verdict> {
    let binary = find_compass_binary()?;
    let stdout = run_compass_bounded(&binary, cwd, COMPASS_TIMEOUT)?;
    parse_verdict(&stdout)
}

/// Run `<binary> nudge --json` in `cwd` with a bounded wait, returning stdout
/// bytes on success. Returns `None` on spawn failure, non-zero exit, or a
/// timeout — matching this module's existing fail-soft convention (`None`
/// means "can't tell", caller preserves prior behavior). On timeout the child
/// (and, on Unix, its process group) is killed so no orphaned process lingers.
///
/// `timeout` is a parameter (rather than always `COMPASS_TIMEOUT`) so tests
/// can exercise the exact timeout/kill code path with a short local bound
/// instead of waiting out the production timeout.
fn run_compass_bounded(binary: &Path, cwd: &Path, timeout: Duration) -> Option<Vec<u8>> {
    let mut cmd = Command::new(binary);
    cmd.args(["nudge", "--json"])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    let mut child = cmd.spawn().ok()?;
    match child.wait_timeout(timeout) {
        Ok(Some(status)) => {
            let out = read_stdout_bounded(child.stdout.take(), timeout);
            if status.success() {
                Some(out)
            } else {
                None
            }
        }
        Ok(None) => {
            kill_tree(&mut child);
            let _ = child.wait();
            None
        }
        Err(_) => None,
    }
}

/// Kill the whole process tree of a timed-out compass call, not just the
/// direct process, so a hung subprocess it may have spawned doesn't outlive
/// the timeout either.
fn kill_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        let pid = child.id() as i32;
        // SAFETY: plain libc syscall; negative pid targets the process group
        // we created via `process_group(0)` at spawn time. Best effort: any
        // error is ignored, same as the plain `child.kill()` it supplements.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    let _ = child.kill();
}

/// Bounded stdout read: never block past `timeout` even if some lingering
/// process keeps the pipe's write end open after the immediate child exits.
fn read_stdout_bounded(stdout: Option<std::process::ChildStdout>, timeout: Duration) -> Vec<u8> {
    use std::sync::mpsc;
    let Some(mut so) = stdout else {
        return Vec::new();
    };
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut out = Vec::new();
        let _ = so.read_to_end(&mut out);
        let _ = tx.send(out);
    });
    rx.recv_timeout(timeout).unwrap_or_default()
}

/// Parse `compass nudge --json` stdout into a [`Verdict`]. Split out for unit
/// testing without a real compass binary on PATH.
fn parse_verdict(stdout: &[u8]) -> Option<Verdict> {
    serde_json::from_slice(stdout).ok()
}

/// Bounded existence probe: does `<name> --version` run without hanging or
/// erroring? Same fail-soft/timeout-bounded shape as `run_compass_bounded`,
/// but discards output — this is only used to decide whether `binary` on PATH
/// resolves to a runnable compass at all.
fn probe_binary_bounded(name: &str) -> bool {
    let mut cmd = Command::new(name);
    cmd.arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    let Ok(mut child) = cmd.spawn() else {
        return false;
    };
    match child.wait_timeout(COMPASS_TIMEOUT) {
        Ok(Some(status)) => status.success(),
        Ok(None) => {
            kill_tree(&mut child);
            let _ = child.wait();
            false
        }
        Err(_) => false,
    }
}

/// Locate the compass binary: PATH first, then the plugin cache (newest version).
fn find_compass_binary() -> Option<PathBuf> {
    if probe_binary_bounded("compass") {
        return Some(PathBuf::from("compass"));
    }

    // ~/.claude/plugins/cache/yukineko/compass/<version>/bin/compass
    let base = home()
        .join(".claude")
        .join("plugins")
        .join("cache")
        .join("yukineko")
        .join("compass");

    let mut candidates: Vec<PathBuf> = std::fs::read_dir(&base)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path().join("bin").join("compass"))
        .filter(|p| p.exists())
        .collect();

    candidates.sort();
    candidates.pop()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_stale_verdict() {
        let v =
            parse_verdict(br#"{"fresh":false,"reason":"charter may be stale (x) - run /compass"}"#)
                .expect("parse");
        assert!(!v.fresh);
        assert!(v.reason.as_deref().unwrap_or("").contains("stale"));
    }

    #[test]
    fn parses_fresh_verdict_with_null_reason() {
        let v = parse_verdict(br#"{"fresh":true,"reason":null}"#).expect("parse");
        assert!(v.fresh);
        assert!(v.reason.is_none());
    }

    #[test]
    fn unparseable_output_is_none() {
        assert!(parse_verdict(b"compass: some human line, not json").is_none());
        assert!(parse_verdict(b"").is_none());
    }

    // ── shell-out timeout: a hung `compass nudge --json` must fail soft ────
    //
    // `charter_freshness` shells out to a compass binary that is, by this
    // module's own contract, only a soft dependency (see module docs): if it's
    // slow or hangs, callers must get a graceful `None` back promptly, never
    // an indefinite block. These tests simulate a hung binary with a small
    // fake executable script that just sleeps, and drive it through the exact
    // `run_compass_bounded` code path `charter_freshness` uses.

    #[cfg(unix)]
    fn write_hanging_fake_binary(dir: &Path, sleep_secs: u64) -> PathBuf {
        let path = dir.join("fake-compass-hang.sh");
        std::fs::write(
            &path,
            format!("#!/bin/sh\nsleep {sleep_secs}\necho '{{\"fresh\":true}}'\n"),
        )
        .expect("write fake binary");
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path).expect("metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).expect("chmod");
        path
    }

    #[cfg(unix)]
    #[test]
    fn hung_compass_invocation_returns_promptly_with_graceful_none() {
        let tmp = std::env::temp_dir().join(format!(
            "autoflow-compass-timeout-test-{}-{}",
            std::process::id(),
            line!()
        ));
        std::fs::create_dir_all(&tmp).expect("create tmp dir");
        let fake_binary = write_hanging_fake_binary(&tmp, 30);
        let short_timeout = Duration::from_millis(300);

        let start = std::time::Instant::now();
        let out = run_compass_bounded(&fake_binary, &tmp, short_timeout);
        let elapsed = start.elapsed();

        assert!(
            out.is_none(),
            "a timed-out compass invocation must fall back gracefully (None), not fabricate output"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "run_compass_bounded must return promptly on timeout, took {elapsed:?} (production \
             COMPASS_TIMEOUT is 3s; this test drives the same code path with a short local \
             override to stay fast)"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[cfg(unix)]
    #[test]
    fn compass_invocation_faster_than_timeout_still_returns_output() {
        let tmp = std::env::temp_dir().join(format!(
            "autoflow-compass-fast-test-{}-{}",
            std::process::id(),
            line!()
        ));
        std::fs::create_dir_all(&tmp).expect("create tmp dir");
        let fake_binary = write_hanging_fake_binary(&tmp, 0);

        let out = run_compass_bounded(&fake_binary, &tmp, Duration::from_secs(3))
            .expect("fast binary must not be treated as timed out");
        let v = parse_verdict(&out).expect("fast binary output must parse");
        assert!(v.fresh);

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
