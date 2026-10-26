//! `session-end-sync`: the `SessionEnd` hook, detached so it fits the hook budget.
//!
//! Measured 2026-10-25: Claude Code gives a `SessionEnd` hook about 1.5 s, and
//! a plugin hook's declared `timeout` does not raise it. The hook used to run
//! `harvest; sync` in the foreground, and `sync` does a network `git pull` and
//! `git push`, so Claude Code cancelled it mid-flight ("Hook cancelled"). A
//! killed process writes no `sync-error.json`, so the failure was dark: 9
//! local commits sat unpushed in the sync checkout and nothing said so.
//!
//! So the hook (`cmd_session_end`) only does three quick things and returns:
//!
//!   1. writes a **pending marker** under `~/.fugu-router/session-end-pending/`
//!      BEFORE any work starts, recording a pid;
//!   2. spawns a fully detached worker (`session-end-sync-worker`): its own session
//!      (`setsid`), stdin from `/dev/null`, stdout/stderr to
//!      `~/.fugu-router/session-end.log` — so the hook's pipes close at once
//!      and Claude Code's cancellation of the hook cannot reach the worker;
//!   3. exits.
//!
//! The worker first rewrites the marker with its own pid, then runs `harvest`
//! and then `sync`, in that order and sequentially (harvest appends episodes;
//! sync must push a store harvest has already appended to). Each step keeps
//! its own failure record (`harvest-error.json` / `sync-error.json`). Only when
//! the worker reaches the end does it remove its marker.
//!
//! `pending()` is read by the `UserPromptSubmit` hook. A marker whose recorded
//! process is no longer alive means a sync that started and never finished
//! (killed, crashed, machine slept and rebooted): it is reported at once,
//! judged by liveness, not by an age threshold. A marker whose process is
//! alive is a sync still in flight and is not reported — unless it has been
//! running far longer than any sync can (`STUCK_AFTER`), which also covers a
//! recycled pid. Anything that cannot be determined (unreadable directory or
//! marker, unparseable marker, liveness probe error) is reported as unfinished
//! (CLAUDE.md §3: cannot-determine resolves to the restrictive side).

use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::{config, harvest, store, syncstate};

/// A running worker older than this is reported as stuck. `sync` bounds each
/// network git call at 30 s and harvest's at 10 s, so a healthy worker is
/// done in well under a minute.
const STUCK_AFTER_SECS: u64 = 15 * 60;
/// The worker log is truncated once it grows past this.
const LOG_MAX_BYTES: u64 = 1024 * 1024;

#[derive(Serialize, Deserialize)]
struct Pending {
    /// The process responsible for the sync right now: the hook while it
    /// spawns the worker, the worker once it has started.
    pid: i64,
    /// Unix seconds when the hook started.
    at: u64,
    /// `spawning` (written by the hook) or `running` (written by the worker).
    phase: String,
}

fn fugu_dir() -> PathBuf {
    config::home_dir().join(".fugu-router")
}

fn pending_dir() -> PathBuf {
    fugu_dir().join("session-end-pending")
}

fn log_path() -> PathBuf {
    fugu_dir().join("session-end.log")
}

/// Write the marker atomically (temp file + rename), so `pending()` never
/// reads a half-written one.
fn write_marker(path: &Path, marker: &Pending) -> Result<()> {
    let body = serde_json::to_string(marker).context("serializing the pending marker")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, body).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("renaming {} to {}", tmp.display(), path.display()))
}

/// Open the worker's log for stdout/stderr. Truncated when it has grown large.
fn open_log() -> Result<std::fs::File> {
    let path = log_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let too_big = match std::fs::metadata(&path) {
        Ok(m) => m.len() > LOG_MAX_BYTES,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true);
    if too_big {
        opts.write(true).truncate(true);
    } else {
        opts.append(true);
    }
    opts.open(&path)
        .with_context(|| format!("opening {}", path.display()))
}

/// The `SessionEnd` hook: mark, spawn the detached worker, return.
pub fn cmd_session_end() -> Result<()> {
    let at = store::now_secs();
    let own_pid = i64::from(std::process::id());
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let marker = pending_dir().join(format!("{at}-{own_pid}-{nanos}.json"));

    // 1. The marker comes BEFORE any work. If it cannot be written, the
    //    pending path cannot make a killed worker visible, so record a sync
    //    failure instead: only a sync that actually finishes clears it.
    let marked = match write_marker(
        &marker,
        &Pending {
            pid: own_pid,
            at,
            phase: "spawning".into(),
        },
    ) {
        Ok(()) => true,
        Err(e) => {
            let detail = format!(
                "the SessionEnd sync started without its pending marker ({e:#}); \
                 this notice clears only once a sync finishes"
            );
            eprintln!("fugu-router: {detail}");
            syncstate::record(&detail);
            false
        }
    };

    // 2. Spawn the detached worker.
    let spawned = (|| -> Result<()> {
        let exe = std::env::current_exe().context("resolving the fugu-router executable")?;
        let mut cmd = std::process::Command::new(exe);
        cmd.arg("session-end-sync-worker");
        if marked {
            cmd.arg("--marker").arg(&marker);
        }
        let (out, err) = match open_log().and_then(|f| Ok((f.try_clone()?, f))) {
            Ok((a, b)) => (Stdio::from(a), Stdio::from(b)),
            // The log is diagnostics only (failures also land in the error
            // files), so run without it, but say so on the hook's stderr.
            Err(e) => {
                eprintln!("fugu-router: session-end log unavailable, worker output dropped: {e:#}");
                (Stdio::null(), Stdio::null())
            }
        };
        cmd.stdin(Stdio::null()).stdout(out).stderr(err);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // SAFETY: `setsid` is async-signal-safe and touches no memory of
            // the parent; it only detaches the child into a new session so a
            // signal to the hook's process group cannot reach it.
            unsafe {
                cmd.pre_exec(|| {
                    if libc::setsid() == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        cmd.spawn().context("spawning the session-end worker")?;
        Ok(())
    })();

    if let Err(e) = spawned {
        let detail = format!("the SessionEnd sync could not start: {e:#}");
        syncstate::record(&detail);
        if marked {
            // sync-error.json now carries the explanation; drop the marker.
            let _ = std::fs::remove_file(&marker);
        }
        anyhow::bail!(detail);
    }
    Ok(())
}

/// The detached worker: take over the marker, harvest, then sync, then clear.
pub fn cmd_session_end_worker(
    cfg: &config::Config,
    marker: Option<PathBuf>,
    window_days: u64,
    lookback_days: u64,
) -> Result<()> {
    if let Some(path) = &marker {
        let at = std::fs::read_to_string(path)
            .ok()
            .and_then(|raw| serde_json::from_str::<Pending>(&raw).ok())
            .map(|p| p.at)
            .unwrap_or_else(store::now_secs);
        if let Err(e) = write_marker(
            path,
            &Pending {
                pid: i64::from(std::process::id()),
                at,
                phase: "running".into(),
            },
        ) {
            // The marker still names the (exiting) hook, so it will read as
            // unfinished if this worker dies: still visible, just less precise.
            eprintln!("fugu-router: could not take over the pending marker: {e:#}");
        }
    }

    println!("fugu-router session-end: step harvest");
    let harvested = harvest::cmd_harvest(cfg, None, window_days, lookback_days);
    if let Err(e) = &harvested {
        eprintln!("fugu-router: harvest: {e:#}");
    }
    println!("fugu-router session-end: step sync");
    let synced = crate::cmd_sync(cfg, false, false);
    if let Err(e) = &synced {
        eprintln!("fugu-router: sync: {e:#}");
    }
    println!("fugu-router session-end: done");

    // Both steps ran to completion; their failures, if any, are recorded in
    // their own error files. The marker's job is done.
    if let Some(path) = &marker {
        if let Err(e) = std::fs::remove_file(path) {
            if e.kind() != std::io::ErrorKind::NotFound {
                eprintln!(
                    "fugu-router: could not clear the pending marker {}: {e}",
                    path.display()
                );
            }
        }
    }
    harvested.and(synced)
}

enum Liveness {
    Alive,
    Dead,
    Unknown(String),
}

fn liveness(pid: i64) -> Liveness {
    #[cfg(unix)]
    {
        let Ok(p) = libc::pid_t::try_from(pid) else {
            return Liveness::Unknown(format!("pid {pid} is out of range"));
        };
        if p <= 0 {
            // kill(0 or negative) would probe a process group, not a process.
            return Liveness::Unknown(format!("pid {pid} is not a process id"));
        }
        // SAFETY: signal 0 performs only the existence/permission check.
        if unsafe { libc::kill(p, 0) } == 0 {
            return Liveness::Alive;
        }
        let err = std::io::Error::last_os_error();
        match err.raw_os_error() {
            Some(libc::ESRCH) => Liveness::Dead,
            // Exists, owned by someone else.
            Some(libc::EPERM) => Liveness::Alive,
            _ => Liveness::Unknown(format!("probing pid {pid}: {err}")),
        }
    }
    #[cfg(not(unix))]
    {
        Liveness::Unknown(format!("no liveness probe on this platform (pid {pid})"))
    }
}

/// The notice to surface for SessionEnd syncs that started and did not finish.
pub fn pending() -> Option<String> {
    pending_in(&pending_dir(), store::now_secs())
}

fn pending_in(dir: &Path, now: u64) -> Option<String> {
    let unfinished = |why: String| {
        format!(
            "fugu-router: a SessionEnd sync of the record store did not finish ({why}). \
             Episodes recorded on this machine may not be reaching `sync_repo`. \
             Run `fugu-router sync` to finish it and see any error."
        )
    };
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            return Some(unfinished(format!(
                "the pending-marker directory {} is unreadable: {e}",
                dir.display()
            )))
        }
    };
    let mut reasons = Vec::new();
    for entry in entries {
        let path = match entry {
            Ok(en) => en.path(),
            Err(e) => {
                reasons.push(format!("an entry of {} is unreadable: {e}", dir.display()));
                continue;
            }
        };
        if path.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let marker = match std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|raw| serde_json::from_str::<Pending>(&raw).map_err(|e| e.to_string()))
        {
            Ok(m) => m,
            Err(e) => {
                reasons.push(format!("marker {} is unreadable: {e}", path.display()));
                continue;
            }
        };
        match liveness(marker.pid) {
            Liveness::Dead => reasons.push(format!(
                "started at unix {}, its process {} is gone",
                marker.at, marker.pid
            )),
            Liveness::Unknown(why) => reasons.push(format!(
                "started at unix {}, liveness undetermined: {why}",
                marker.at
            )),
            Liveness::Alive if now.saturating_sub(marker.at) > STUCK_AFTER_SECS => {
                reasons.push(format!(
                    "started at unix {}, process {} still running after {} s",
                    marker.at,
                    marker.pid,
                    now.saturating_sub(marker.at)
                ))
            }
            Liveness::Alive => {}
        }
    }
    if reasons.is_empty() {
        None
    } else {
        Some(unfinished(reasons.join("; ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "fugu-sessionend-unit-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap_or_else(|e| panic!("{e}"));
        d
    }

    fn put(d: &Path, name: &str, pid: i64, at: u64) {
        write_marker(
            &d.join(name),
            &Pending {
                pid,
                at,
                phase: "running".into(),
            },
        )
        .unwrap_or_else(|e| panic!("{e:#}"));
    }

    #[test]
    fn missing_dir_and_empty_dir_report_nothing() {
        let d = dir("empty");
        assert!(pending_in(&d.join("absent"), 100).is_none());
        assert!(pending_in(&d, 100).is_none());
    }

    #[test]
    fn live_marker_is_not_reported_but_a_stuck_one_is() {
        let d = dir("live");
        put(&d, "a.json", i64::from(std::process::id()), 100);
        assert!(pending_in(&d, 100 + 5).is_none());
        let n = pending_in(&d, 100 + STUCK_AFTER_SECS + 1);
        assert!(n.is_some_and(|s| s.contains("still running")));
    }

    #[test]
    fn dead_marker_is_reported() {
        let d = dir("dead");
        let mut child = std::process::Command::new("true")
            .spawn()
            .unwrap_or_else(|e| panic!("{e}"));
        let pid = i64::from(child.id());
        let _ = child.wait();
        put(&d, "a.json", pid, 100);
        let n = pending_in(&d, 101);
        assert!(n.is_some_and(|s| s.contains("is gone") && s.contains("sync")));
    }

    #[test]
    fn unparseable_marker_and_bogus_pid_are_reported() {
        let d = dir("bad");
        std::fs::write(d.join("x.json"), "not json").unwrap_or_else(|e| panic!("{e}"));
        assert!(pending_in(&d, 1).is_some_and(|s| s.contains("unreadable")));
        let d = dir("pid0");
        put(&d, "a.json", 0, 1);
        assert!(pending_in(&d, 1).is_some_and(|s| s.contains("undetermined")));
    }

    #[test]
    fn temp_files_are_ignored() {
        let d = dir("tmp");
        std::fs::write(d.join("a.tmp"), "partial").unwrap_or_else(|e| panic!("{e}"));
        assert!(pending_in(&d, 1).is_none());
    }
}
