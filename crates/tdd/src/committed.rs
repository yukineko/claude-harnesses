//! Whether the gate's scan included the work this session committed
//! ([`Committed`]), and the block raised when that could not be determined.
//! Split out of `gate.rs` (file-growth cap) when [`Committed::Undetermined`]
//! was added; `gate` re-exports [`Committed`].

use harness_core::verdict::{Determination, Verdict};

use crate::config::Config;
use crate::gate::Report;

/// Whether `scan` includes the work this session committed.
///
/// * `Included { since, lines, test_evidence }` — added lines of non-merge
///   commits on HEAD dated at or after `since` (the session start) were
///   scanned, before attribution narrowed them. `test_evidence` is whether
///   those committed lines, narrowed to this session's files, add a test
///   ([`crate::gate::committed_test_evidence`], computed by
///   [`crate::gate::include_committed`]).
/// * `NotCredited { why }` — committed work was not scanned and there was no
///   session to scope it to (a manual run with no transcript, or the scan
///   itself was not attempted); `why` is said in the message.
/// * `Undetermined { why }` — a Stop hook run whose transcript yields NO
///   session start (unreadable, or no record carries a `timestamp`), so the
///   commits this session made could not be told apart from earlier history
///   and were not scanned. That is "cannot determine", not "nothing
///   committed": when the uncommitted scan alone would allow, the verdict is
///   `Undetermined` and the stop BLOCKS (bounded by `max_attempts`, CLAUDE.md
///   §3) — it used to allow with only a stderr note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Committed {
    Included {
        since: i64,
        lines: usize,
        test_evidence: Determination<bool>,
    },
    NotCredited {
        why: String,
    },
    Undetermined {
        why: String,
    },
}

impl Committed {
    pub(crate) fn not_attempted() -> Self {
        Committed::NotCredited {
            why: "committed work was not scanned".to_string(),
        }
    }

    /// What the scan covered, for every message that reports an absence: the
    /// uncommitted changes always, this session's own commits only when they
    /// were actually scanned. Tests committed before the session are never read.
    pub fn scope(&self) -> &'static str {
        match self {
            Committed::Included { .. } => {
                "the uncommitted changes and this session's own commits (tests already committed \
                 before the session started were not consulted)"
            }
            Committed::NotCredited { .. } | Committed::Undetermined { .. } => {
                "the uncommitted changes (already-committed tests were not consulted)"
            }
        }
    }

    /// The reason committed work could not be scoped, when that is
    /// undetermined (see [`Committed::Undetermined`]).
    pub fn undetermined(&self) -> Option<&str> {
        match self {
            Committed::Undetermined { why } => Some(why),
            _ => None,
        }
    }
}

/// True when the block comes ONLY from [`Committed::Undetermined`]: the scan
/// itself is known and would allow, but this session's commits could not be
/// scoped.
pub(crate) fn scope_blocks(v: &Report, cfg: &Config) -> bool {
    matches!(v.scan, Determination::Known(Some(_)))
        && v.committed.undetermined().is_some()
        && matches!(v.verdict(cfg), Verdict::Undetermined(_))
}

/// The model-facing block reason for [`scope_blocks`].
pub(crate) fn scope_block_reason(why: &str, attempt: u32, max: u32) -> String {
    format!(
        "🔴 tdd: couldn't determine this session's commits — the transcript gives no \
         session start ({why}), so work committed during this session could not be told \
         apart from earlier history and was NOT scanned (attempt {attempt}/{max}). The \
         uncommitted changes alone show nothing to block on, but untested implementation \
         committed in this session would be invisible, so the stop is not allowed blindly. \
         If this session committed implementation, make sure its test is committed too and \
         say so; then run `tdd skip --reason \"...\"` to skip once (THIS session only, \
         recorded), or set TDD_DISABLE=1 in the environment Claude Code itself was started \
         with (exporting it from a tool call does not reach this hook)."
    )
}
