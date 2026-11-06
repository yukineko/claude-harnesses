//! Judging one extra root (a root other than the hook cwd) that declares
//! checks. Shared by the parent's Stop loop (`gate_run` in `main.rs`) and the
//! SubagentStop judgment ([`crate::subagent`]), so the two apply the SAME
//! green / identical-finding waiver / max_attempts give-up / block logic. The
//! caller passes the attempt-counter key (`session`): the parent's session key,
//! or a per-agent key for a subagent, so the two never share a chain.

use std::path::Path;

use crate::config::Config;
use crate::{gate, state};

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

/// Run `rcfg`'s checks on `r` and decide. `waiver_allowed` gates the
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
