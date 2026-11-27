//! The two bounds on `detect_bash`'s recursion: the depth cap
//! ([`MAX_SHELL_DEPTH`], answered by [`depth_exhausted`]) and the per-command
//! node budget ([`MAX_ANALYSIS_NODES`] / [`ANALYSIS_BUDGET`], consumed by
//! [`take_budget`]). Moved out of `detect.rs` unchanged (file-growth cap).

use super::Decision;

/// How deep we recurse into shell-evaluation wrappers (`eval`, `sh -c`, …)
/// before giving up. Bounds work and guards against pathological nesting; no
/// realistic command line wraps a destructive payload this many layers deep.
pub(super) const MAX_SHELL_DEPTH: usize = 8;

/// Total number of `detect_bash` invocations allowed for ONE top-level command.
///
/// D4 (verified availability defect, fixed): a depth cap alone does NOT bound
/// the work, because the re-analysis arms FAN OUT. `analyze_find` loops over
/// every `-exec` position and re-analyses the whole remaining tail, which still
/// contains the other `-exec` tokens, so the branching factor is the number of
/// `-exec` tokens and the tree is O(N^MAX_SHELL_DEPTH). Measured on a single
/// invocation of `find . ` + `-exec find . `×N + `-exec echo {} ` + `+ `×(N+1):
/// N=20 → 2.2s, N=24 → 8.2s, N=28 → 30s, N=32 (a 503-byte command) → >60s,
/// killed. blastguard is a PreToolUse hook, so that hangs the user's turn.
///
/// This caps total NODE VISITS across the whole recursion tree, not just its
/// depth. Ordinary commands use fewer than a dozen visits; the cap is three
/// orders of magnitude above that, so it cannot be reached by real work.
pub(super) const MAX_ANALYSIS_NODES: u32 = 2_000;

thread_local! {
    /// Remaining node budget for the top-level command currently being analysed.
    pub(super) static ANALYSIS_BUDGET: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
thread_local! {
    /// Test-only work probe: when installed, counts EVERY `detect_bash` entry
    /// (admitted or budget-rejected) on this thread. It is an atomic shared via
    /// `Arc` so a watcher thread can observe the count while the analysis is
    /// still running, and fail on an unbounded tree instead of hanging. It is
    /// deliberately independent of `ANALYSIS_BUDGET`: removing the budget must
    /// make this count explode, not leave it untouched.
    pub(super) static VISIT_PROBE: std::cell::RefCell<Option<std::sync::Arc<std::sync::atomic::AtomicU64>>> =
        const { std::cell::RefCell::new(None) };
}

/// Total number of NESTED (`depth >= 1`) `detect_bash` visits one user command
/// may cost across ALL its readings (b37bc26c).
///
/// [`ANALYSIS_BUDGET`] is refilled by every reading and every re-entry (each
/// payload layer, each inline Python program the S5 pass judges), so it bounds
/// one reading's tree, not the command: k sequential programs each carrying a
/// D4 fan-out cost ~k x [`MAX_ANALYSIS_NODES`] nodes (measured: k=4 took 13-29 s
/// against the 10 s hook timeout, and a timed-out hook does not block the
/// call). This caps the command as a whole. Exhausting it is the same
/// fail-closed answer as exhausting the per-reading budget: the nested visit
/// is an Ask (a Deny for a subagent). Depth-0 entries are exempt: they run
/// their rules without recursing into the nested visits this counts, so a
/// harmful tail (`rm -rf /etc`) after an exhausted total is still judged and
/// still a Deny.
pub(super) const MAX_COMMAND_NODES: u32 = 3_000;

thread_local! {
    /// Nested visits left for the command being judged; `None` outside a
    /// top-level detection (direct `detect_bash` callers, tests).
    static COMMAND_NODES: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
}

pub(super) fn command_nodes_left() -> Option<u32> {
    COMMAND_NODES.with(std::cell::Cell::get)
}

pub(super) fn set_command_nodes(n: Option<u32>) {
    COMMAND_NODES.with(|c| c.set(n));
}

/// Install the per-command total for the outermost detection, if none is
/// installed yet. The returned guard clears it when that detection ends.
pub(super) struct CommandNodesGuard(bool);

impl CommandNodesGuard {
    pub(super) fn install() -> Self {
        let fresh = command_nodes_left().is_none();
        if fresh {
            set_command_nodes(Some(MAX_COMMAND_NODES));
        }
        CommandNodesGuard(fresh)
    }
}

impl Drop for CommandNodesGuard {
    fn drop(&mut self) {
        if self.0 {
            set_command_nodes(None);
            clear_shallow();
        }
    }
}

/// Charge one nested visit to the command total. False when it is spent.
/// Depth 0 is never charged (see [`MAX_COMMAND_NODES`]).
pub(super) fn take_command_node(depth: usize) -> bool {
    if depth == 0 {
        return true;
    }
    COMMAND_NODES.with(|c| match c.get() {
        None => true,
        Some(0) => false,
        Some(n) => {
            c.set(Some(n - 1));
            true
        }
    })
}

/// The budget gate of `detect_bash`: runs `judge` (the visit proper) when both
/// the per-reading budget and the per-command nested-visit total admit it.
///
/// When the per-command total of nested visits is spent (b37bc26c), a bare Ask
/// would WEAKEN a verdict the unbounded analysis reached (a system-harm Deny in
/// a tail after k fan-out programs came back Ask in main mode). So that text is
/// still judged by its own rules, SHALLOWLY (see [`shallow_visit`]): never an
/// Allow, a Deny only if the rules found one.
pub(super) fn admit(cmd: &str, depth: usize, judge: impl FnOnce() -> Decision) -> Decision {
    const TOO_COMPLEX: &str = "command is too complex to analyse within the safety budget — blastguard cannot vouch for it either way";
    // 687ece3c: an exhausted per-reading budget, a spent command total and a
    // visit nested inside a shallow visit all get the same shallow judgement,
    // not a bare Ask: a nested system-harm target must stay a Deny. Only the
    // shallow depth/size bounds below fall back to the bare Ask.
    if depth > 0 && in_shallow_visit() {
        return shallow_visit(cmd, depth, TOO_COMPLEX, judge);
    }
    if !take_budget() || !take_command_node(depth) {
        return shallow_visit(cmd, depth, TOO_COMPLEX, judge);
    }
    judge()
}

/// Most distinct texts judged shallowly per command (see [`shallow_visit`]).
const MAX_SHALLOW_VISITS: usize = 600;

/// Extra distinct texts that may still be judged once the memo is full, if
/// they spell a system directory ([`may_name_system_path`]).
const MAX_PRIORITY_SHALLOW_VISITS: usize = 400;

/// Deepest nesting of shallow visits (a shallow visit's own nested payloads are
/// shallow visits too).
const MAX_SHALLOW_DEPTH: usize = 8;

#[derive(Default)]
struct Shallow {
    depth: usize,
    priority: usize,
    memo: std::collections::HashMap<(String, usize), Decision>,
}

thread_local! {
    static SHALLOW: std::cell::RefCell<Shallow> = std::cell::RefCell::new(Shallow::default());
}

/// True while a [`shallow_visit`] is running.
pub(super) fn in_shallow_visit() -> bool {
    SHALLOW.with(|s| s.borrow().depth > 0)
}

/// Does `cmd` contain the spelling of a system directory? A cheap literal
/// filter, never a verdict: it only decides whether a text may still be judged
/// shallowly after the memo is full, where the judge itself says Deny or not.
fn may_name_system_path(cmd: &str) -> bool {
    crate::deletion::SYSTEM_PREFIXES
        .iter()
        .any(|p| cmd.contains(p))
}

/// Judge `cmd` by its own rules when the per-command nested-visit total is
/// spent: `judge` runs with every nested visit answering Ask, so the cost is
/// that of the rules alone. A Deny is returned as is; anything else becomes
/// the budget Ask `why` (never Allow: the nested part was not examined).
/// Results are memoised per `(text, depth)` and the memo is bounded by
/// [`MAX_SHALLOW_VISITS`]; past that only a text spelling a system directory
/// ([`may_name_system_path`], at most [`MAX_PRIORITY_SHALLOW_VISITS`] of them)
/// is still judged, and the rest get the bare Ask. Nested shallow visits go
/// down to [`MAX_SHALLOW_DEPTH`]. Entries into `detect_bash` for the D4 find
/// fan-out input: 5073 with this (2199 when rejected entries were bare leaves);
/// `d4_exponential_find_exec_is_bounded_and_denies` bounds it at 8250.
pub(super) fn shallow_visit(
    cmd: &str,
    depth: usize,
    why: &str,
    judge: impl FnOnce() -> Decision,
) -> Decision {
    let key = (cmd.to_string(), depth);
    let known = SHALLOW.with(|s| {
        let mut s = s.borrow_mut();
        if s.depth >= MAX_SHALLOW_DEPTH {
            return Some(Decision::ask(why));
        }
        if let Some(d) = s.memo.get(&key) {
            return Some(d.clone());
        }
        if s.memo.len() >= MAX_SHALLOW_VISITS {
            if s.priority >= MAX_PRIORITY_SHALLOW_VISITS || !may_name_system_path(cmd) {
                return Some(Decision::ask(why));
            }
            s.priority += 1;
        }
        s.depth += 1;
        None
    });
    if let Some(d) = known {
        return d;
    }
    let judged = judge();
    let verdict = if judged.is_deny() {
        judged
    } else {
        Decision::ask(why)
    };
    SHALLOW.with(|s| {
        let mut s = s.borrow_mut();
        s.depth -= 1;
        s.memo.insert(key, verdict.clone());
    });
    verdict
}

/// Forget the shallow memo (end of the outermost detection).
fn clear_shallow() {
    SHALLOW.with(|s| *s.borrow_mut() = Shallow::default());
}

/// Consume one unit of the analysis budget. Returns false when it is exhausted.
pub(super) fn take_budget() -> bool {
    ANALYSIS_BUDGET.with(|b| {
        let left = b.get();
        if left == 0 {
            false
        } else {
            b.set(left - 1);
            true
        }
    })
}

/// The recursion cap was reached, so analysis DID NOT FINISH. That is the same
/// condition as budget exhaustion (`detect_bash`'s D4 guard, [`take_budget`]) —
/// "we did not finish looking, so we have no verdict" — and it gets the same
/// answer: an Ask, which `Decision::hardened` turns into the pre-existing Deny
/// whenever no human is available to answer.
///
/// Every one of the five depth-capped sites used to resolve to `Allow`, either
/// by returning it outright or by skipping the recursion and falling through to
/// `acc.finish()`. A destructive payload nested past the cap was therefore
/// waved through *unanalysed* — the exact fail-open this module exists to
/// prevent, sitting one line away from the sibling condition that handles it
/// correctly.
///
/// The reason deliberately differs from the budget ask so the two unfinished-
/// analysis causes stay distinguishable in the logs.
pub(super) fn depth_exhausted() -> Decision {
    Decision::ask(
        "command nests shell/wrapper invocations deeper than blastguard analyses \
         (recursion depth limit) — analysis did not finish, so blastguard cannot \
         vouch for it either way",
    )
}
