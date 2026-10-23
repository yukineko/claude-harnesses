/// Control plane: pause, resume, reassignment.
///
/// These actions have cross-session side effects, so the binary performs the
/// deterministic action here; user approval is handled upstream by the
/// /overwatch skill, not in this file. A control action that did not happen
/// (downstream `condukt` missing or exiting non-zero, an unresolvable lease
/// root, a contended lease lock, an unheld reassign key) exits NON-ZERO and
/// says so; it is never reported as success. pause/resume record their
/// outcome (`ok=true|false`) to the overwatch ledger for auditability.
use crate::event::LifecycleEvent;
use crate::store;
use anyhow::Result;
use std::process::Command;

/// Test-only race-window widener for `reassign()`'s load->remove->save section,
/// analogous to `lease.rs`'s `artificial_mutator_delay`. Real process-spawn
/// overhead dwarfs that section's natural duration, so a racing `begin()` almost
/// never interleaves inside it by chance — the concurrency regression test needs
/// a deterministic way to force the interleave. No-op unless
/// `OVERWATCH_TEST_REASSIGN_DELAY_MS` is set (never set outside the
/// `lease_concurrency` integration test), so production behavior is unchanged.
fn artificial_reassign_delay() {
    if let Some(ms) = std::env::var("OVERWATCH_TEST_REASSIGN_DELAY_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
    {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
}

/// Resolve a session id for audit events: `--session` is not plumbed to control
/// actions, so fall back to the env session id or a pid-derived id.
fn resolve_session_id() -> String {
    if let Ok(s) = std::env::var("CLAUDE_CODE_SESSION_ID") {
        return s;
    }
    format!("pid-{}", std::process::id())
}

/// PURE: build the argv for delegating a pause to condukt.
/// Kept side-effect free so it can be unit-tested without spawning condukt.
fn pause_cmd(run: &str) -> Vec<String> {
    vec![
        "condukt".to_string(),
        "state".to_string(),
        "pause".to_string(),
        "--run".to_string(),
        run.to_string(),
    ]
}

/// PURE: build the argv for delegating a resume to condukt.
fn resume_cmd(run: &str) -> Vec<String> {
    vec![
        "condukt".to_string(),
        "state".to_string(),
        "resume".to_string(),
        "--run".to_string(),
        run.to_string(),
    ]
}

/// Run an argv without propagating an error. Returns true if the command
/// spawned AND exited zero; false if the binary was absent or exited non-zero.
/// A note is printed on failure so the operator sees what happened; the caller
/// turns `false` into a non-zero exit.
fn run_fail_soft(argv: &[String]) -> bool {
    if argv.is_empty() {
        return false;
    }
    match Command::new(&argv[0]).args(&argv[1..]).status() {
        Ok(status) if status.success() => true,
        Ok(status) => {
            println!(
                "{{\"control_note\": \"{} exited {}\"}}",
                argv[0],
                status.code().unwrap_or(-1)
            );
            false
        }
        Err(e) => {
            println!("{{\"control_note\": \"could not run {}: {}\"}}", argv[0], e);
            false
        }
    }
}

/// Record a control action to the overwatch ledger. Reuses `LifecycleEvent`
/// (consistent with event.rs, which has no dedicated control kind) as a
/// `running` event whose `status` note carries the control verb. A ledger
/// write error is noted on stdout (the control action itself already ran).
fn record_control_event(subject_key: &str, run_id: &str, action_note: String) {
    let cwd = match std::env::current_dir() {
        Ok(c) => c,
        Err(_) => return,
    };
    let now = store::now();
    let event = LifecycleEvent::running(
        subject_key.to_string(),
        format!("control:{action_note}"),
        resolve_session_id(),
        run_id.to_string(),
        now,
        Some(action_note),
    );
    if let Err(e) = store::append_event(&cwd, &event) {
        println!("{{\"control_note\": \"ledger append failed: {e}\"}}");
    }
}

/// Pause a run: delegate to condukt, then audit. Errors (non-zero exit) when
/// condukt could not be run or exited non-zero: the pause did not happen.
pub fn pause(run: &str) -> Result<()> {
    let ok = run_fail_soft(&pause_cmd(run));
    record_control_event(run, run, format!("pause ok={ok}"));
    if !ok {
        anyhow::bail!(
            "overwatch pause: `condukt state pause --run {run}` failed; run was NOT paused"
        );
    }
    Ok(())
}

/// Resume a run: delegate to condukt, then audit. Errors (non-zero exit) when
/// condukt could not be run or exited non-zero: the resume did not happen.
pub fn resume(run: &str) -> Result<()> {
    let ok = run_fail_soft(&resume_cmd(run));
    record_control_event(run, run, format!("resume ok={ok}"));
    if !ok {
        anyhow::bail!(
            "overwatch resume: `condukt state resume --run {run}` failed; run was NOT resumed"
        );
    }
    Ok(())
}

/// Reassign a lease: release the current lease for `key` (load → remove → save,
/// mirroring lease.rs mutations) and audit the new owner `to`. Errors (non-zero
/// exit, nothing recorded) if the key isn't held, the lock can't be taken, or
/// the lease root is unresolvable.
pub fn reassign(key: &str, to: &str) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let now = store::now();

    // Hold the lease-registry lock across the whole load->reap->remove->save
    // cycle, exactly as lease.rs's mutators do, so a reassign racing a
    // concurrent begin/heartbeat cannot save a stale snapshot back and
    // silently free a just-claimed key (double-grab). HARD-SKIP: if the lock
    // is not taken (contended, or the root is unresolvable) fail with a
    // non-zero exit rather than proceed unlocked.
    let _lock = crate::lease::lock_or_bail(&cwd, "reassign")?;

    let mut leases = store::load_leases(&cwd)?;
    store::reap_stale_recorded(&cwd, &mut leases, now)?;
    artificial_reassign_delay();

    // Capture the run id of the released lease (if any) for the audit event.
    let released_run = leases.get(key).map(|l| l.run_id.clone());

    if leases.remove(key).is_none() {
        println!("{{\"control_note\": \"reassign: key {key} not held; nothing reassigned\"}}");
        anyhow::bail!(
            "overwatch reassign: no lease held under key `{key}`; nothing was reassigned"
        );
    }
    // Persist the released registry.
    store::save_leases(&cwd, &leases)?;

    let run_id = released_run.unwrap_or_else(|| format!("run-{now}"));
    record_control_event(key, &run_id, format!("reassign to={to}"));

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pause_cmd_builds_expected_argv() {
        assert_eq!(
            pause_cmd("run-x"),
            vec!["condukt", "state", "pause", "--run", "run-x"]
        );
    }

    #[test]
    fn resume_cmd_builds_expected_argv() {
        assert_eq!(
            resume_cmd("run-y"),
            vec!["condukt", "state", "resume", "--run", "run-y"]
        );
    }

    #[test]
    fn cmd_builders_thread_the_run_id_through() {
        assert_eq!(pause_cmd("abc")[4], "abc");
        assert_eq!(resume_cmd("abc")[4], "abc");
        // First token is always the downstream binary name.
        assert_eq!(pause_cmd("z")[0], "condukt");
        assert_eq!(resume_cmd("z")[0], "condukt");
    }

    #[test]
    fn run_fail_soft_is_false_for_missing_binary() {
        // A binary that does not exist must be fail-soft (false), not a panic.
        let ok = run_fail_soft(&[
            "definitely-not-a-real-binary-xyz".to_string(),
            "arg".to_string(),
        ]);
        assert!(!ok);
    }

    #[test]
    fn run_fail_soft_is_false_for_empty_argv() {
        assert!(!run_fail_soft(&[]));
    }
}
