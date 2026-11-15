//! The hook's own root — the cwd — is the ONLY root donegate judges.
//!
//! User rulings 2026-11-08, verbatim: "donegateのworktreeテストは問題ありすぎるので
//! 即時に停止" and "donegateそのものが他のworktreeを見たり検証したりテストしたり
//! することは禁止とする". donegate does not resolve, list, inspect, check or
//! report any other worktree or root, on Stop or on SubagentStop. The only
//! thing it learns from a transcript is whether a written path lies INSIDE the
//! cwd ([`writes_in`] / [`wrote_in_cwd`]) — a path comparison, never a look at
//! what lies outside it.
//!
//! [`judge`] runs the cwd's checks for the SubagentStop ([`crate::subagent`]):
//! green / identical-finding waiver / max_attempts give-up / block. The caller
//! passes the attempt-counter key (`session`), a per-agent key, so a subagent
//! never shares the parent's chain.

use std::path::Path;

use harness_core::attribution::SessionWrites;
use harness_core::verdict::Determination;

use crate::config::Config;
use crate::{gate, state};

/// Whether some write in `writes` lies inside `cwd` (as given or canonical).
/// A relative path cannot be placed and counts as inside (the restrictive
/// side: the cwd is then judged). Pure path comparison: nothing outside the
/// cwd is opened.
pub(crate) fn writes_in(cwd: &Path, writes: &SessionWrites) -> bool {
    let cwd_c = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    // macOS spells `/private/var/...` and `/private/tmp/...` also without the
    // `/private` prefix; compare against that alias too (string-only, so no
    // path outside the cwd is ever touched).
    let alias = cwd_c
        .strip_prefix("/private")
        .ok()
        .map(|p| Path::new("/").join(p));
    let inside = |p: &Path| {
        p.is_relative()
            || p.starts_with(cwd)
            || p.starts_with(&cwd_c)
            || alias.as_deref().is_some_and(|a| p.starts_with(a))
    };
    writes
        .edited
        .iter()
        .chain(writes.bash.iter())
        .any(|p| inside(p))
}

/// Whether the session (parent and its subagent sidechains) wrote into the
/// cwd. No transcript (manual CLI run) or a transcript file that does not
/// exist records no writes: `Known(false)`. Any other read problem is
/// `Undetermined` and the caller blocks.
pub(crate) fn wrote_in_cwd(transcript_path: &str, cwd: &Path) -> Determination<bool> {
    if transcript_path.trim().is_empty() {
        return Determination::known(false);
    }
    if matches!(
        std::fs::metadata(transcript_path),
        Err(ref e) if e.kind() == std::io::ErrorKind::NotFound
    ) {
        return Determination::known(false);
    }
    match harness_core::attribution::session_written_paths(transcript_path) {
        Determination::Known(w) => Determination::known(writes_in(cwd, &w)),
        Determination::Undetermined(why) => Determination::undetermined(format!(
            "cannot read the session's writes from the transcript: {}",
            why.as_str()
        )),
    }
}

/// What judging one root produced. `Waived` and `GaveUp` also pushed a
/// non-blocking notice; neither blocks the stop.
pub(crate) enum Judged {
    Green,
    Waived,
    GaveUp,
    /// Blocks: the displayed reason and its repeat-ledger identity.
    Blocked {
        reason: String,
        key: String,
    },
}

/// Run `rcfg`'s checks on `r` (the hook cwd) and decide. `waiver_allowed` gates the
/// identical-finding waiver (hook mode with a real session id only).
pub(crate) fn judge(
    rcfg: &Config,
    r: &Path,
    session: &str,
    waiver_allowed: bool,
    notices: &mut Vec<String>,
) -> Judged {
    let mut report = gate::evaluate(rcfg, r);
    if report.all_green() {
        // Only failing optional checks can have a log here; keep them too.
        gate::save_records(
            &mut report,
            &rcfg.state_dir,
            session,
            0,
            rcfg.output_tail_lines,
        );
        state::forget(&rcfg.state_dir, session, r);
        return Judged::Green;
    }
    // Identical-finding waiver, checked BEFORE the counter is bumped: a
    // waived stop is not a block and does not count toward max_attempts.
    let sig = crate::red_signature(r, &report);
    if waiver_allowed {
        if let Some(sig) = &sig {
            if state::is_repeat(&rcfg.state_dir, session, r, sig, rcfg.reset_after_secs) {
                let failing: Vec<String> =
                    report.blocking().iter().map(|o| o.name.clone()).collect();
                crate::log_event(rcfg, session, "repeat-waived", &failing, 0);
                notices.push(format!(
                    "[root: {}] {}",
                    r.display(),
                    crate::repeat_message(&failing)
                ));
                return Judged::Waived;
            }
        }
    }
    let attempt = state::bump(&rcfg.state_dir, session, rcfg.reset_after_secs);
    gate::save_records(
        &mut report,
        &rcfg.state_dir,
        session,
        attempt,
        rcfg.output_tail_lines,
    );
    let failing: Vec<String> = report.blocking().iter().map(|o| o.name.clone()).collect();
    if attempt > rcfg.max_attempts {
        state::reset(&rcfg.state_dir, session);
        crate::log_event(rcfg, session, "giveup", &failing, attempt);
        eprintln!(
            "donegate: root {} still failing after {} attempts ({}); giving up on it. \
             The checks are still RED.",
            r.display(),
            rcfg.max_attempts,
            failing.join(", ")
        );
        notices.push(format!(
            "[root: {}] {}",
            r.display(),
            crate::giveup_message(&failing, rcfg.max_attempts)
        ));
        return Judged::GaveUp;
    }
    crate::log_event(rcfg, session, "blocked", &failing, attempt);
    crate::emit_violations(r, session, &failing, crate::Outcome::Blocked);
    if waiver_allowed {
        let chain = state::chain_for_block(&rcfg.state_dir, session, attempt);
        state::remember(
            &rcfg.state_dir,
            session,
            r,
            sig.as_deref(),
            chain.as_deref(),
        );
    }
    Judged::Blocked {
        reason: format!(
            "[root: {}]\n{}",
            r.display(),
            gate::block_reason(&report, attempt, rcfg.max_attempts)
        ),
        key: format!(
            "[root: {}]\n{}",
            r.display(),
            gate::block_fingerprint(&report, attempt, rcfg.max_attempts)
        ),
    }
}
