//! Default [`ToolResultGroomer`] — the primary size lever (PostToolUse).
//!
//! Phase 2 (first, per force-priority): trim/summary-replace a bloated tool
//! result so the window's dominant growth term flattens (I4). The replacement
//! must be strictly smaller than the input; correctness is free here because the
//! input is an [`Evictable`] (never `Pinned`/`Verbatim`).
//!
//! The groomer is deliberately tokenizer-free and deterministic: it estimates
//! tokens at ~4 chars each, and when a result exceeds the budget it keeps the
//! head and tail and elides the middle (where tool output is least load-bearing)
//! behind a single marker. No model call, no API key — pure string surgery.

use crate::handlers::ToolResultGroomer;
use crate::io::HookOutput;
use crate::ledger::{Action, Ledger, LedgerNode};
use crate::types::{ContextItem, Evictable, ItemBody, ItemId, Lane};
use harness_core::hook::HookInput;

/// Default token budget for a single tool result before grooming engages.
/// Results at or under this pass through untouched; larger ones are head/tail
/// trimmed. Override per-environment with `CONTEXT_GOVERNOR_GROOM_BUDGET`.
const DEFAULT_GROOM_BUDGET_TOKENS: u32 = 2048;

/// Rough, deterministic token estimate (~4 chars/token, tokenizer-free). Any
/// non-empty body is at least 1 token so a tiny string never reads as free.
fn est_tokens(s: &str) -> u32 {
    let chars = s.chars().count();
    if chars == 0 {
        0
    } else {
        u32::try_from(chars.div_ceil(4).max(1)).unwrap_or(u32::MAX)
    }
}

/// Resolve the active grooming budget (env override → default). This is the
/// *default* budget at zero window pressure; [`budget_for`] shrinks it as the
/// conversation fills up.
fn groom_budget() -> u32 {
    std::env::var("CONTEXT_GOVERNOR_GROOM_BUDGET")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_GROOM_BUDGET_TOKENS)
}

/// Conversation token occupancy at or above which the groom budget is pinned to
/// its floor. Below it, the budget scales linearly from the default (at zero
/// pressure) down to the floor. ~80% of a 200k window — the band where shedding
/// tool-result bulk matters most.
const PRESSURE_FULL_SCALE_TOKENS: u64 = 160_000;

/// Window-pressure-aware budget (I6 observe→act): a pure, deterministic,
/// monotonically non-increasing function of observed `pressure` (conversation
/// token occupancy). At `pressure == 0` it returns `default_budget` unchanged
/// (backward compatible); as pressure rises it shrinks linearly toward a floor
/// of a quarter of the default (never zero for a sane default), saturating at
/// [`PRESSURE_FULL_SCALE_TOKENS`]. Higher pressure ⇒ a tighter budget ⇒ groom
/// harder. All arithmetic is saturating, so no input can panic or underflow.
fn budget_for(pressure: u64, default_budget: u32) -> u32 {
    if pressure == 0 {
        return default_budget;
    }
    // Floor: a quarter of the default, clamped into `[1, default_budget]` so a
    // tiny configured budget never floors above itself or at zero.
    let floor = (default_budget / 4).max(1).min(default_budget);
    let span = default_budget - floor; // headroom that pressure can reclaim
    let p = pressure.min(PRESSURE_FULL_SCALE_TOKENS);
    let reduction = u32::try_from(span as u64 * p / PRESSURE_FULL_SCALE_TOKENS).unwrap_or(span);
    default_budget - reduction
}

/// Keep the head and tail of `body`, eliding the middle, so the result fits
/// roughly `budget` tokens. Deterministic and split only on char boundaries. May
/// return a string no smaller than the input when the budget leaves nothing to
/// elide — the caller enforces I4 and declines in that case.
fn trim_middle(body: &str, budget: u32) -> String {
    let chars: Vec<char> = body.chars().collect();
    let total = chars.len();
    // ~chars affordable for `budget` tokens, split evenly head/tail.
    let keep = (budget as usize).saturating_mul(4);
    let head_len = keep / 2;
    let tail_len = keep / 2;
    if head_len + tail_len >= total {
        return body.to_string(); // nothing to elide; caller rechecks I4
    }
    let elided = total - head_len - tail_len;
    let head: String = chars[..head_len].iter().collect();
    let tail: String = chars[total - tail_len..].iter().collect();
    format!("{head}\n…[context-governor: elided {elided} chars of {total}]…\n{tail}")
}

pub struct DefaultGroomer;

impl ToolResultGroomer for DefaultGroomer {
    fn groom(&self, tool_output: Evictable<'_>, budget: u32) -> Option<serde_json::Value> {
        // Only an inline body occupies the window; a Ref is already externalized.
        let body = match &tool_output.item().body {
            ItemBody::Inline(s) => s,
            ItemBody::Ref(_) => return None,
        };
        // Under budget → leave untouched (the common case).
        if tool_output.item().tokens <= budget {
            return None;
        }
        let trimmed = trim_middle(body, budget);
        // I4: the replacement must be *strictly* smaller than the input. If the
        // trim could not shrink it (degenerate budget, or the marker outweighs
        // what was elided), decline rather than grow the window.
        if trimmed.chars().count() >= body.chars().count() {
            return None;
        }
        Some(serde_json::Value::String(trimmed))
    }
}

impl DefaultGroomer {
    /// Bin entry point: read `input.tool_response`, wrap it as an `Evictable`,
    /// groom under the active budget, emit a ledger row on a groom, and return a
    /// PostToolUse `updatedToolOutput` envelope (or `{}` when nothing is groomed).
    /// This is the ONLY place that writes to the ledger — unit tests that call
    /// `groom` or `to_output_with_budget` remain side-effect-free.
    pub fn to_output(&self, input: &HookInput) -> HookOutput {
        // Observe the live window pressure (conversation token occupancy) and let
        // it drive how hard we groom. A missing/empty transcript reads as zero
        // pressure, so the budget is the plain default and behavior is unchanged.
        let pressure =
            harness_core::transcript::last_usage_tokens(&input.transcript_path).unwrap_or(0);
        let budget = budget_for(pressure, groom_budget());
        match self.groom_with_sizes(input, budget) {
            Some((groomed, saved_tokens, resident_tokens)) => {
                let node = LedgerNode {
                    session: input.session_id.clone(),
                    hook: "PostToolUse".to_string(),
                    item: None,
                    action: Action::Groomed { saved_tokens },
                    reason: "oversized-tool-result",
                };
                Ledger::open(&input.cwd).append(&node, resident_tokens);
                HookOutput::groomed(groomed)
            }
            None => HookOutput::default(),
        }
    }

    /// Budget-injectable core of [`Self::to_output`] — lets tests pin a budget
    /// without touching the process-global env var. Pure: no ledger side effects.
    #[cfg(test)]
    fn to_output_with_budget(&self, input: &HookInput, budget: u32) -> HookOutput {
        match self.groom_with_sizes(input, budget) {
            Some((groomed, _, _)) => HookOutput::groomed(groomed),
            None => HookOutput::default(),
        }
    }

    /// Groom `input.tool_response` under `budget` and report pre/post sizes.
    /// Returns `Some((groomed_value, saved_tokens, resident_tokens))` when the
    /// result was actually shrunk, `None` otherwise (under-budget, Ref body,
    /// degenerate trim, or no `tool_response`).
    ///
    /// * `saved_tokens`   = est_tokens(pre_body) − est_tokens(post_str) (saturating)
    /// * `resident_tokens`= est_tokens(post_str)  — post-groom window occupancy
    fn groom_with_sizes(
        &self,
        input: &HookInput,
        budget: u32,
    ) -> Option<(serde_json::Value, u32, u32)> {
        let resp = input.tool_response.as_ref()?;
        // The string that actually occupies the window: a JSON string result is
        // its own text; anything structured is rendered compactly.
        let body = match resp {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        let pre_tokens = est_tokens(&body);
        let item = ContextItem {
            id: ItemId(0),
            lane: Lane::Evictable,
            tokens: pre_tokens,
            body: ItemBody::Inline(body),
        };
        // Infallible by construction (the item is `Lane::Evictable`), but we
        // honor the capability constructor rather than asserting.
        let ev = Evictable::new(&item)?;
        let groomed = self.groom(ev, budget)?;
        // `groom` only ever constructs `Value::String` (see `DefaultGroomer::groom`
        // above), so this is infallible by construction today — but we still fall
        // back to `""` rather than `.unwrap()`/`.expect()` so a future groomer
        // implementation returning a non-string `Value` degrades to "no tokens
        // saved" instead of panicking the hook process.
        let post_str = groomed.as_str().unwrap_or("");
        let post_tokens = est_tokens(post_str);
        let saved_tokens = pre_tokens.saturating_sub(post_tokens);
        let resident_tokens = post_tokens;
        Some((groomed, saved_tokens, resident_tokens))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn evictable_item(body: &str) -> ContextItem {
        ContextItem {
            id: ItemId(0),
            lane: Lane::Evictable,
            tokens: est_tokens(body),
            body: ItemBody::Inline(body.to_string()),
        }
    }

    // ── budget_for: window-pressure-aware budget (I6 observe→act) ─────────────

    /// Zero pressure must return the default budget byte-for-byte (backward
    /// compatible — a missing transcript reads as zero pressure).
    #[test]
    fn budget_for_zero_pressure_is_default() {
        assert_eq!(budget_for(0, 2048), 2048);
        assert_eq!(budget_for(0, 777), 777);
    }

    /// High pressure must shrink the budget strictly below the default and pin it
    /// to the floor (a quarter of the default) once pressure saturates the scale.
    #[test]
    fn budget_for_high_pressure_shrinks_to_floor() {
        let floor = 2048 / 4;
        assert!(
            budget_for(PRESSURE_FULL_SCALE_TOKENS, 2048) < 2048,
            "high pressure must produce a smaller budget than the default"
        );
        assert_eq!(
            budget_for(PRESSURE_FULL_SCALE_TOKENS, 2048),
            floor,
            "budget at full-scale pressure must equal the floor"
        );
        // Beyond full scale it saturates: no further shrink below the floor.
        assert_eq!(budget_for(PRESSURE_FULL_SCALE_TOKENS * 100, 2048), floor);
    }

    /// The mapping must be monotonically non-increasing in pressure: more window
    /// occupancy never *raises* the budget.
    #[test]
    fn budget_for_is_monotonic_non_increasing() {
        let mut prev = budget_for(0, 2048);
        for step in 0..=40 {
            let pressure = step * 5_000; // 0 .. 200_000, crossing the full scale
            let b = budget_for(pressure, 2048);
            assert!(
                b <= prev,
                "budget must not rise as pressure grows: at {pressure} got {b} > prev {prev}"
            );
            prev = b;
        }
        // And the endpoint is the floor (saturated past full scale).
        assert_eq!(prev, 2048 / 4);
    }

    /// The floor must never be zero for a sane default, so grooming stays
    /// meaningful even under maximal pressure.
    #[test]
    fn budget_for_floor_is_never_zero() {
        assert!(budget_for(u64::MAX, 2048) > 0);
        assert!(budget_for(u64::MAX, 8) > 0);
        // A degenerate tiny default never floors above itself.
        assert!(budget_for(u64::MAX, 1) <= 1);
    }

    #[test]
    fn under_budget_passes_through() {
        let item = evictable_item("short result");
        let ev = Evictable::new(&item).unwrap();
        // tokens(~3) well under a generous budget → untouched.
        assert_eq!(DefaultGroomer.groom(ev, 2048), None);
    }

    #[test]
    fn over_budget_trims_strictly_smaller() {
        let big = "x".repeat(40_000); // ~10k tokens
        let item = evictable_item(&big);
        let ev = Evictable::new(&item).unwrap();
        let got = DefaultGroomer
            .groom(ev, 256)
            .expect("over budget must groom");
        let s = got.as_str().unwrap();
        // I4: strictly smaller than the input...
        assert!(s.chars().count() < big.chars().count());
        // ...and it carries the elision marker (head/tail kept, middle dropped).
        assert!(s.contains("context-governor: elided"));
    }

    #[test]
    fn ref_body_is_not_groomed() {
        // An externalized body holds no inline window cost to reclaim.
        let item = ContextItem {
            id: ItemId(0),
            lane: Lane::Evictable,
            tokens: 9999,
            body: ItemBody::Ref(crate::types::StoreKey(1)),
        };
        let ev = Evictable::new(&item).unwrap();
        assert_eq!(DefaultGroomer.groom(ev, 1), None);
    }

    fn post_tool_use(resp: Option<serde_json::Value>) -> HookInput {
        let mut obj = serde_json::json!({
            "session_id": "s",
            "transcript_path": "",
            "cwd": "",
            "hook_event_name": "PostToolUse",
            "tool_name": "Bash"
        });
        if let Some(r) = resp {
            obj["tool_response"] = r;
        }
        serde_json::from_value(obj).unwrap()
    }

    #[test]
    fn to_output_without_tool_response_is_empty() {
        let input = post_tool_use(None);
        assert_eq!(
            DefaultGroomer.to_output_with_budget(&input, 256).to_json(),
            "{}"
        );
    }

    #[test]
    fn to_output_under_budget_is_empty() {
        let input = post_tool_use(Some(serde_json::json!("a small tool result")));
        assert_eq!(
            DefaultGroomer.to_output_with_budget(&input, 2048).to_json(),
            "{}"
        );
    }

    #[test]
    fn to_output_over_budget_emits_post_tool_use_envelope() {
        let big = "y".repeat(40_000);
        let input = post_tool_use(Some(serde_json::Value::String(big)));
        let json = DefaultGroomer.to_output_with_budget(&input, 256).to_json();
        assert!(json.contains("\"hookEventName\":\"PostToolUse\""));
        assert!(json.contains("\"updatedToolOutput\""));
        assert!(json.contains("context-governor: elided"));
    }

    proptest! {
        /// I4, exhaustively: whatever the body and budget, a groom result is
        /// never larger than the input — the groomer only ever returns a
        /// strictly-smaller string, never grows the window.
        #[test]
        fn groom_never_grows_the_window(body in ".{0,5000}", budget in 0u32..4096) {
            let item = evictable_item(&body);
            let ev = Evictable::new(&item).unwrap();
            if let Some(v) = DefaultGroomer.groom(ev, budget) {
                let s = v.as_str().expect("groomed value is a JSON string");
                prop_assert!(s.chars().count() < body.chars().count());
            }
        }
    }

    /// Verify that `to_output` (the real bin entry) emits exactly ONE ledger row
    /// with event="groomed" and saved_tokens>0 when it grooms an over-budget
    /// result, and NO row when the input is under-budget.
    ///
    /// NOTE: this test sets `CONTEXT_GOVERNOR_STATE_DIR` and
    /// `CLAUDE_CODE_SESSION_ID` — process-global env vars. It is kept as a single
    /// self-contained function and uses a process-id-scoped unique cwd to reduce
    /// (but not eliminate) the race window with `backing::tests::open_is_ok…`,
    /// which also touches those vars. The ledger side-effect is isolated to this
    /// test; all other groomer tests call `groom` / `to_output_with_budget` and
    /// are therefore side-effect-free.
    #[test]
    fn to_output_emits_ledger_row_when_groomed_and_none_when_not() {
        use crate::ledger::rollup;

        // Crate-shared lock: serialise against every other test that mutates the
        // process-global CONTEXT_GOVERNOR_STATE_DIR / CLAUDE_CODE_SESSION_ID
        // (injector, snapshot guard/checkpointer, backing) so a concurrent set_var
        // cannot redirect this test's ledger path mid-run.
        let _env = crate::defaults::guard::acquire_env_lock();

        // Unique per-process tmpdir and cwd so parallel test runs don't collide.
        let tmp = tempfile::tempdir().expect("tempdir");
        let state_dir = tmp.path().to_str().expect("utf-8 state dir").to_string();
        let unique_cwd = format!("/tmp/cg-groomer-ledger-test-{}", std::process::id());
        let session_id = format!("test-groomer-{}", std::process::id());

        std::env::set_var("CONTEXT_GOVERNOR_STATE_DIR", &state_dir);
        std::env::set_var("CLAUDE_CODE_SESSION_ID", &session_id);

        // --- over-budget call: must emit exactly one groomed row ---
        let big = "z".repeat(40_000); // ~10 000 tokens, well over the 2048 default
        let input_over = {
            let mut obj = serde_json::json!({
                "session_id": &session_id,
                "transcript_path": "",
                "cwd": &unique_cwd,
                "hook_event_name": "PostToolUse",
                "tool_name": "Bash"
            });
            obj["tool_response"] = serde_json::Value::String(big);
            serde_json::from_value::<HookInput>(obj).unwrap()
        };
        let output_over = DefaultGroomer.to_output(&input_over);
        assert!(
            output_over.to_json().contains("updatedToolOutput"),
            "over-budget to_output must return a groomed envelope"
        );

        let summary = rollup(&unique_cwd);
        assert_eq!(
            summary.rows, 1,
            "exactly one ledger row after over-budget groom"
        );
        assert_eq!(
            summary.per_event.get("groomed"),
            Some(&1),
            "ledger event must be 'groomed'"
        );
        assert!(
            summary.total_saved_tokens > 0,
            "saved_tokens must be > 0 (got {})",
            summary.total_saved_tokens
        );

        // --- under-budget call: must NOT append any row ---
        let input_under = {
            let obj = serde_json::json!({
                "session_id": &session_id,
                "transcript_path": "",
                "cwd": &unique_cwd,
                "hook_event_name": "PostToolUse",
                "tool_name": "Bash",
                "tool_response": "tiny result"
            });
            serde_json::from_value::<HookInput>(obj).unwrap()
        };
        let output_under = DefaultGroomer.to_output(&input_under);
        assert_eq!(
            output_under.to_json(),
            "{}",
            "under-budget to_output must be empty (no-op)"
        );

        let summary2 = rollup(&unique_cwd);
        assert_eq!(
            summary2.rows, 1,
            "under-budget call must NOT append a new row (still 1)"
        );

        // Clean up env vars regardless of test outcome.
        std::env::remove_var("CONTEXT_GOVERNOR_STATE_DIR");
        std::env::remove_var("CLAUDE_CODE_SESSION_ID");
    }
}
