//! `ctxrot stop` — Stop hook auto-compact nudge (feature ⑤).
//!
//! Times the nudge off ctxrot's OWN budget-based usage estimate — the same
//! `(est_tokens / context_window)` meter that `ctxrot guard` / `ctxrot usage`
//! report bands from (the one that can read >100%) — NOT the raw
//! `context_window.used_percentage` (the true ~1M model window). The guard bands
//! are computed against ctxrot's smaller *configured budget*, so the raw model
//! percentage stays tiny while the budget meter is already "112%"; aligning here
//! makes the nudge fire when it should.
//!
//! never-break-a-turn / no turn-trap: blocking on Stop is BOUNDED. We nudge at
//! most ONCE per band crossing (mirroring the guard's "advice once per band"),
//! persisting the last-nudged band in `<state_dir>/<safe>.compact-band`. A second
//! Stop at the same already-nudged band does NOT re-block, so a persistently-high
//! context can never permanently trap the turn. The band is relaxed when usage
//! falls (e.g. after a /compact) so a later re-climb can nudge again — never a
//! one-way ratchet. `stop_hook_active` and `auto_compact_enabled` still gate it.

use harness_core::hook::HookInput;
use harness_core::transcript;

use crate::config::Config;
use crate::hooks::guard::safe_session;

/// Stop hook core: returns a JSON `{"decision":"block","reason":"..."}` string
/// when the budget-meter usage crosses into a new band at/above the threshold,
/// `None` to allow the session to end.
pub fn run(input: &HookInput, cfg: &Config) -> Option<String> {
    // A block we ourselves triggered re-enters as `stop_hook_active` → never
    // block again on that pass (the built-in guard against an infinite Stop loop).
    if input.stop_hook_active {
        return None;
    }
    if !cfg.auto_compact_enabled {
        return None;
    }
    if input.transcript_path.is_empty() {
        return None;
    }

    // ctxrot's OWN budget-based estimate — identical to the guard's meter:
    // real usage tokens over the *configured budget* (`context_window`), which is
    // the number that reads >100% while the raw model window is still tiny.
    // `src` distinguishes a real `usage` block ("usage") from the bytes-proxy
    // fallback ("bytes") — see the cannot-determine handling below.
    let (est_tokens, src) = transcript::estimate_tokens(&input.transcript_path)?;
    let frac = est_tokens as f64 / cfg.context_window as f64;
    let band = cfg.band_for(frac);

    // Bounded-nudge state: the last band we nudged at, keyed per session. Kept in
    // a dedicated `.compact-band` file so it never collides with the guard's own
    // `.band` escalation state.
    let _ = std::fs::create_dir_all(&cfg.state_dir);
    let state_file = cfg
        .state_dir
        .join(format!("{}.compact-band", safe_session(&input.session_id)));
    let last: usize = std::fs::read_to_string(&state_file)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);

    // Usage fell into a lower band (e.g. after a /compact) → relax the ratchet so
    // a later re-climb can nudge again. Never a one-way trap.
    if band < last {
        let _ = std::fs::write(&state_file, band.to_string());
    }

    // Block only when BOTH: over the budget-meter threshold, AND this is a fresh
    // upward band crossing (band > last). The second condition is the turn-trap
    // guard: a repeated Stop at the same already-nudged band takes this early
    // return and allows the session to end.
    let threshold = cfg.auto_compact_at_percentage;
    if frac < threshold || band <= last {
        // The normal reading says "don't block". But when the estimate came from
        // the bytes-proxy fallback (`src == "bytes"`, i.e. NO usage block parsed)
        // and has still reached band 1 (the same floor guard escalates from), we
        // are in a cannot-determine: the bytes proxy is cache-blind (`size/4`
        // cannot see cache-read tokens) and is only a LOWER bound on true usage,
        // so a below-`threshold` reading here is NOT trustworthy — a corrupt or
        // truncated long session reads low this way while it may already be over
        // budget. Per doctrine we surface it rather than silently allow it, but
        // ONCE (a dedicated sentinel, never a turn-trap) so a genuine early
        // session — which never reaches band 1 without a usage block — is not
        // over-alarmed.
        if src == "bytes" && band >= 1 && frac < threshold {
            return unmeasurable_nudge(cfg, &input.session_id, frac);
        }
        return None;
    }

    // Record that we nudged at this band so the next Stop here does not re-block.
    let _ = std::fs::write(&state_file, band.to_string());

    let pct = frac * 100.0;
    let threshold_pct = threshold * 100.0;
    let reason = format!(
        "Context usage is at ~{pct:.0}% of ctxrot's budget (nudge threshold {threshold_pct:.0}%). \
         Run /compact to free up context before continuing. \
         (This nudge fires once per band; to disable it set auto_compact_enabled=false in \
         ~/.ctxrot/config.toml or CTXROT_AUTO_COMPACT=0.)"
    );
    Some(serde_json::json!({ "decision": "block", "reason": reason }).to_string())
}

/// One-shot compact nudge for an UNMEASURABLE session: the transcript had no
/// parseable `usage` block (the estimate fell back to the cache-blind bytes
/// proxy) yet that proxy — a LOWER bound on true usage — has already reached
/// band 1. We cannot confirm we are below the block threshold, so we surface a
/// nudge instead of silently allowing a possibly-over-budget session to keep
/// growing. Fires at most ONCE per session via a dedicated `.compact-unmeasured`
/// sentinel (independent of the `.compact-band` ratchet) so it never re-blocks
/// and never traps the turn; returns `None` if it already fired. The eventual
/// genuine high reading is still caught by the normal `frac >= threshold` block
/// above, so this sentinel never suppresses that.
fn unmeasurable_nudge(cfg: &Config, session_id: &str, frac: f64) -> Option<String> {
    let flag = cfg
        .state_dir
        .join(format!("{}.compact-unmeasured", safe_session(session_id)));
    if flag.exists() {
        return None;
    }
    let _ = std::fs::write(&flag, "1");

    let at_least_pct = frac * 100.0;
    let reason = format!(
        "Context usage could not be measured precisely — the transcript has no usage block, \
         so this is only a rough size estimate. That estimate ALONE is already ~{at_least_pct:.0}% \
         of ctxrot's budget, and it is a LOWER bound (cache-read tokens are invisible to it), so \
         true usage may be higher. Run /compact before continuing. (This unmeasured nudge fires \
         at most once per session; to disable it set auto_compact_enabled=false in \
         ~/.ctxrot/config.toml or CTXROT_AUTO_COMPACT=0.)"
    );
    Some(serde_json::json!({ "decision": "block", "reason": reason }).to_string())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use harness_core::hook::HookInput;

    use super::*;

    /// Write a one-line transcript whose LAST usage block totals `tokens`, so
    /// `estimate_tokens` returns exactly `tokens` (the "usage" source path).
    fn write_transcript(dir: &Path, tokens: u64) -> String {
        let p = dir.join("transcript.jsonl");
        let line = serde_json::json!({
            "type": "assistant",
            "message": { "role": "assistant", "usage": { "input_tokens": tokens } }
        });
        std::fs::write(&p, format!("{line}\n")).unwrap();
        p.to_string_lossy().into_owned()
    }

    /// Write a transcript with NO `usage` block, of roughly `bytes` size, so
    /// `estimate_tokens` takes the bytes-proxy fallback path (src `"bytes"`,
    /// tokens ≈ `bytes/4`). Uses user-role lines that carry no usage field.
    fn write_bytes_transcript(dir: &Path, bytes: usize) -> String {
        let p = dir.join("proxy.jsonl");
        let line = serde_json::json!({
            "type": "user",
            "message": { "role": "user", "content": "x".repeat(120) }
        })
        .to_string();
        let mut s = String::with_capacity(bytes + line.len());
        while s.len() < bytes {
            s.push_str(&line);
            s.push('\n');
        }
        std::fs::write(&p, s).unwrap();
        p.to_string_lossy().into_owned()
    }

    fn cfg_at(base: &Path, enabled: bool, threshold: f64) -> Config {
        Config {
            state_dir: base.join("state"),
            store_dir: base.join("store"),
            auto_compact_enabled: enabled,
            auto_compact_at_percentage: threshold,
            ..Config::default()
        }
    }

    fn cfg_window(base: &Path, threshold: f64, window: u64) -> Config {
        Config {
            context_window: window,
            ..cfg_at(base, true, threshold)
        }
    }

    fn input_for(session: &str, transcript: &str, stop_hook_active: bool) -> HookInput {
        HookInput {
            session_id: session.into(),
            transcript_path: transcript.into(),
            stop_hook_active,
            ..Default::default()
        }
    }

    #[test]
    fn over_budget_threshold_blocks_with_compact_nudge() {
        // 184000 / 200000 = 0.92 → over the default 0.90 budget threshold (band 3).
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        let t = write_transcript(base, 184_000);
        let cfg = cfg_at(base, true, 0.90);
        let out = run(&input_for("s-over", &t, false), &cfg).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["decision"], "block");
        let reason = v["reason"].as_str().unwrap();
        assert!(
            reason.contains("/compact"),
            "must tell the user to compact: {reason}"
        );
        assert!(
            reason.contains("92%"),
            "must report the budget-meter %: {reason}"
        );
    }

    #[test]
    fn below_budget_threshold_allows() {
        // 100000 / 200000 = 0.50 → below the 0.90 budget threshold, no nudge.
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        let t = write_transcript(base, 100_000);
        let cfg = cfg_at(base, true, 0.90);
        assert!(run(&input_for("s-below", &t, false), &cfg).is_none());
    }

    #[test]
    fn second_stop_same_band_does_not_trap() {
        // Bounded: the first over-threshold Stop blocks, but a second Stop at the
        // SAME already-nudged band allows — the turn is never permanently trapped.
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        let t = write_transcript(base, 184_000);
        let cfg = cfg_at(base, true, 0.90);
        let input = input_for("s-bound", &t, false);
        assert!(run(&input, &cfg).is_some(), "first Stop nudges");
        assert!(
            run(&input, &cfg).is_none(),
            "second Stop at the same band must NOT re-block"
        );
    }

    #[test]
    fn disabled_always_allows() {
        // Even at ~100% of budget, the master switch off means never block.
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        let t = write_transcript(base, 199_000);
        let cfg = cfg_at(base, false, 0.90);
        assert!(run(&input_for("s-dis", &t, false), &cfg).is_none());
    }

    #[test]
    fn stop_hook_active_allows() {
        // Our own re-entrant Stop pass must never block again (loop guard).
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        let t = write_transcript(base, 199_000);
        let cfg = cfg_at(base, true, 0.90);
        assert!(run(&input_for("s-active", &t, true), &cfg).is_none());
    }

    #[test]
    fn no_transcript_allows() {
        let cfg = cfg_at(Path::new("/nonexistent-base"), true, 0.90);
        assert!(run(&input_for("s-none", "", false), &cfg).is_none());
    }

    #[test]
    fn unmeasurable_bytes_proxy_at_band1_nudges_once() {
        // A transcript with NO usage block whose bytes-proxy estimate has reached
        // band 1 (≥50% of the budget) but is BELOW the 0.90 block threshold. The
        // old code returned None here (silent under-nudge = the fail-open); now it
        // must surface a one-shot "unmeasured" nudge, then never re-block.
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        // window 1500 → floor for band 1 is 750 tokens ≈ 3000 bytes; 0.90 threshold
        // is 1350 tokens ≈ 5400 bytes. ~4000 bytes → ~1000 tokens → frac ~0.67 →
        // band 1, below threshold.
        let t = write_bytes_transcript(base, 4000);
        let cfg = cfg_window(base, 0.90, 1500);

        let out = run(&input_for("s-unmeasured", &t, false), &cfg)
            .expect("bytes-proxy at band 1 below threshold must surface a nudge, not stay silent");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["decision"], "block");
        let reason = v["reason"].as_str().unwrap();
        assert!(
            reason.contains("could not be measured"),
            "must name the cannot-determine cause: {reason}"
        );
        assert!(
            reason.contains("/compact"),
            "must tell the user to compact: {reason}"
        );

        // One-shot: a second Stop at the same unmeasurable state must NOT re-block.
        assert!(
            run(&input_for("s-unmeasured", &t, false), &cfg).is_none(),
            "the unmeasured nudge must fire at most once (never trap the turn)"
        );
    }

    #[test]
    fn genuine_early_session_below_band1_stays_silent() {
        // A tiny transcript with no usage block (a real early session) is BELOW
        // band 1 → it must NOT be over-alarmed by the unmeasured branch.
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        // ~400 bytes → ~100 tokens → frac ~0.067 of a 1500 window → band 0.
        let t = write_bytes_transcript(base, 400);
        let cfg = cfg_window(base, 0.90, 1500);
        assert!(
            run(&input_for("s-early", &t, false), &cfg).is_none(),
            "a genuine sub-band-1 early session must stay silent (no over-nudge)"
        );
    }

    #[test]
    fn unmeasurable_does_not_suppress_a_later_genuine_high_block() {
        // After the one-shot unmeasured nudge fires, a bytes-proxy reading that
        // later climbs OVER the 0.90 threshold must still hit the normal block —
        // the sentinel only bounds the unmeasured variant, not the real high one.
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        let cfg = cfg_window(base, 0.90, 1500);
        // First: band-1 unmeasurable → fires once.
        let t_lo = write_bytes_transcript(base, 4000); // ~1000 tokens, frac ~0.67
        assert!(run(&input_for("s-climb2", &t_lo, false), &cfg).is_some());
        // Then: a bigger no-usage-block transcript over threshold (~1400 tokens).
        let t_hi = write_bytes_transcript(base, 5800); // ~1450 tokens, frac ~0.97
        let out = run(&input_for("s-climb2", &t_hi, false), &cfg)
            .expect("a genuine over-threshold reading must still block");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["decision"], "block");
    }

    #[test]
    fn higher_band_crossing_refires() {
        // Not a one-way ratchet: after nudging at one band, crossing into a HIGHER
        // band nudges again. (Bands default [0.50, 0.75, 0.90].)
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        let cfg = cfg_at(base, true, 0.50);
        // 0.60 → band 1, first crossing at/above threshold → blocks.
        let t1 = write_transcript(base, 120_000);
        assert!(run(&input_for("s-climb", &t1, false), &cfg).is_some());
        // Same band again → allow.
        assert!(run(&input_for("s-climb", &t1, false), &cfg).is_none());
        // 0.80 → band 2 (higher) → re-fires.
        let t2 = write_transcript(base, 160_000);
        assert!(run(&input_for("s-climb", &t2, false), &cfg).is_some());
    }
}
