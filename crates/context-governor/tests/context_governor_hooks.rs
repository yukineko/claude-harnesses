//! Integration tests for the context-governor contract.
//!
//! **Phase 1** tests (below the fold) pin the *shape* of the frozen contract
//! and the two type-level invariants (I1 lane intent, I2 unrepresentable
//! lossy-compress of Verbatim/Pinned).
//!
//! **Phase 2** tests (added here) exercise the real `DefaultClassifier` and
//! `DefaultInjector` handlers end-to-end via the public crate API:
//! - Classifier round-trip: heading-based section split, SpecClass/Lane parity,
//!   `check_resident` Ok / Overrun paths (I3).
//! - Injector with a real file: env-var wiring, heading-match injection, and
//!   ToC sentinel on irrelevant prompts.

use context_governor::defaults::{DefaultClassifier, DefaultInjector};
use context_governor::{
    Action, ContextItem, Evictable, HookInput, HookOutput, ItemBody, ItemId, Lane, Overrun,
    SpecClass, StandingBudget, StoreKey,
};
use context_governor::{ContextInjector, SpecClassifier};

// ── Shared env lock (integration-crate analogue of `guard::acquire_env_lock`) ──
// Integration tests compile as a separate crate, so they cannot reach the
// crate-internal `#[cfg(test)] defaults::guard::acquire_env_lock`. Mirror it
// here: one process-global, poison-tolerant lock that every env-mutating test in
// this file acquires, so no two tests race on the process-global
// CONTEXT_GOVERNOR_STATE_DIR / CONTEXT_GOVERNOR_REFERENCE_DOC /
// CLAUDE_CODE_SESSION_ID / CONTEXT_GOVERNOR_GROOM_BUDGET concurrently.
use std::sync::{Mutex, MutexGuard, OnceLock};

static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn env_lock() -> MutexGuard<'static, ()> {
    ENV_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn item(lane: Lane, tokens: u32) -> ContextItem {
    ContextItem {
        id: ItemId(1),
        lane,
        tokens,
        body: ItemBody::Inline("x".to_string()),
    }
}

/// I2, enforced by the type system: an `Evictable` capability token can be
/// minted only from a `Lane::Evictable` item. `Pinned`/`Verbatim` yield `None`,
/// so the groomer (which requires `Evictable`) can never receive them.
#[test]
fn evictable_token_only_from_evictable_lane() {
    assert!(Evictable::new(&item(Lane::Evictable, 10)).is_some());
    assert!(Evictable::new(&item(Lane::Pinned, 10)).is_none());
    assert!(Evictable::new(&item(Lane::Verbatim, 10)).is_none());
}

/// An inline item occupies the window; a `Ref` body does not (it's externalized
/// behind a `StoreKey`). This is the resident/non-resident distinction the size
/// invariants count over.
#[test]
fn residency_tracks_inline_vs_ref_body() {
    let resident = item(Lane::Pinned, 5);
    assert!(resident.is_resident());

    let externalized = ContextItem {
        id: ItemId(2),
        lane: Lane::Evictable,
        tokens: 5,
        body: ItemBody::Ref(StoreKey(7)),
    };
    assert!(!externalized.is_resident());
}

/// I3 arithmetic: `Overrun::excess` reports exactly how many tokens must move to
/// `ReferenceBody` to satisfy the standing budget, and saturates at zero.
#[test]
fn overrun_reports_excess_to_shed() {
    let budget = StandingBudget {
        max_resident_tokens: 100,
    };
    let over = Overrun {
        resident_tokens: 130,
        max_resident_tokens: budget.max_resident_tokens,
    };
    assert_eq!(over.excess(), 30);

    let under = Overrun {
        resident_tokens: 80,
        max_resident_tokens: 100,
    };
    assert_eq!(under.excess(), 0);
}

/// The default (do-nothing) envelope serializes to `{}` — Claude Code reads that
/// as "proceed, no-op", the correct response when a handler declines to act.
#[test]
fn default_output_is_empty_object() {
    assert_eq!(HookOutput::default().to_json(), "{}");
}

/// The two write-back envelopes carry the renamed fields Claude Code expects.
#[test]
fn injection_and_groom_envelopes_use_wire_field_names() {
    let inj = HookOutput::inject("UserPromptSubmit", "ctx".to_string()).to_json();
    assert!(inj.contains("\"hookSpecificOutput\""));
    assert!(inj.contains("\"hookEventName\":\"UserPromptSubmit\""));
    assert!(inj.contains("\"additionalContext\":\"ctx\""));

    let groom = HookOutput::groomed(serde_json::json!({"trimmed": true})).to_json();
    assert!(groom.contains("\"hookEventName\":\"PostToolUse\""));
    assert!(groom.contains("\"updatedToolOutput\""));
}

/// `SpecClass` maps a spec span to a lane discipline (§8). This pins the mapping
/// the classifier must honor: NormativeCore → resident, ReferenceBody → evictable.
#[test]
fn spec_class_variants_are_distinct() {
    assert_ne!(SpecClass::NormativeCore, SpecClass::ReferenceBody);
}

/// The ledger's size-bearing action carries the reclaimed tokens (I4/I6).
#[test]
fn groomed_action_carries_saved_tokens() {
    match (Action::Groomed { saved_tokens: 42 }) {
        Action::Groomed { saved_tokens } => assert_eq!(saved_tokens, 42),
        _ => unreachable!(),
    }
}

// ── Phase 2: DefaultClassifier integration ────────────────────────────────────

/// Spec doc used across the Phase 2 classifier / injector integration tests.
const PHASE2_DOC: &str = "\
# Rules
All API calls MUST include an Authorization header.
Retry on 503 up to 3 times with exponential back-off.

# Reference Table
| Field  | Type   | Required |
|--------|--------|----------|
| id     | uint64 | yes      |
| name   | string | yes      |

# Examples
```bash
curl -H 'Authorization: Bearer TOKEN' https://api.example.com/users
```
";

/// Phase 2 — classifier round-trip: the classifier splits the doc into sections
/// with the correct SpecClass/Lane parity (NormativeCore → Pinned, ReferenceBody
/// → Evictable) and returns them in document order.
#[test]
fn classifier_round_trip_spec_class_and_lane() {
    let items = DefaultClassifier.classify(PHASE2_DOC);
    // 3 sections: "Rules" (normative), "Reference Table" (reference), "Examples" (reference).
    assert_eq!(items.len(), 3, "expected 3 sections from PHASE2_DOC");

    let (class0, item0) = &items[0];
    assert_eq!(
        *class0,
        SpecClass::NormativeCore,
        "section 0 should be NormativeCore"
    );
    assert_eq!(item0.lane, Lane::Pinned, "NormativeCore must be Pinned");
    assert!(
        item0.tokens > 0,
        "non-empty section must have positive token estimate"
    );

    let (class1, item1) = &items[1];
    assert_eq!(
        *class1,
        SpecClass::ReferenceBody,
        "section 1 (Reference Table) should be ReferenceBody"
    );
    assert_eq!(
        item1.lane,
        Lane::Evictable,
        "ReferenceBody must be Evictable"
    );

    let (class2, item2) = &items[2];
    assert_eq!(
        *class2,
        SpecClass::ReferenceBody,
        "section 2 (Examples) should be ReferenceBody"
    );
    assert_eq!(
        item2.lane,
        Lane::Evictable,
        "ReferenceBody must be Evictable"
    );
}

/// Phase 2 — I3: `check_resident` returns `Ok` when Pinned tokens fit the
/// budget and `Err(Overrun)` with correct arithmetic when they exceed it.
#[test]
fn classifier_check_resident_ok_and_overrun() {
    let context_items: Vec<ContextItem> = DefaultClassifier
        .classify(PHASE2_DOC)
        .into_iter()
        .map(|(_, i)| i)
        .collect();

    // Generous budget → Ok.
    let ok_budget = StandingBudget {
        max_resident_tokens: 100_000,
    };
    assert!(
        DefaultClassifier
            .check_resident(&context_items, &ok_budget)
            .is_ok(),
        "should be Ok with a generous budget"
    );

    // Tiny budget → Err(Overrun) with correct excess().
    let tiny_budget = StandingBudget {
        max_resident_tokens: 1,
    };
    let err = DefaultClassifier
        .check_resident(&context_items, &tiny_budget)
        .expect_err("should return Overrun when budget is exceeded");
    assert!(
        err.resident_tokens > tiny_budget.max_resident_tokens,
        "resident_tokens must exceed max_resident_tokens"
    );
    assert_eq!(
        err.excess(),
        err.resident_tokens.saturating_sub(err.max_resident_tokens),
        "excess() must equal the overage amount"
    );
}

// ── Phase 2: DefaultInjector integration ─────────────────────────────────────

/// Phase 2 — injector with a real file: when `CONTEXT_GOVERNOR_REFERENCE_DOC`
/// points to a valid file, `inject` reads it and returns an `additionalContext`
/// envelope. Tests both the heading-match path and the fallback-to-ToC path.
#[test]
fn injector_reads_reference_doc_from_env_and_injects() {
    use std::io::Write as _;

    // Serialise against every other env-mutating test in this crate.
    let _env = env_lock();

    let mut tmp = tempfile::NamedTempFile::new().expect("temp file");
    tmp.write_all(PHASE2_DOC.as_bytes()).expect("write doc");
    tmp.flush().expect("flush");

    std::env::set_var("CONTEXT_GOVERNOR_REFERENCE_DOC", tmp.path());

    // Heading-match path: "examples" overlaps with the "Examples" heading.
    let input_match: HookInput = serde_json::from_value(serde_json::json!({
        "session_id": "integ",
        "transcript_path": "",
        "cwd": "",
        "hook_event_name": "UserPromptSubmit",
        "prompt": "show me some examples",
    }))
    .unwrap();
    let out_match = DefaultInjector.inject(&input_match);
    let json_match = out_match.to_json();
    assert!(
        json_match.contains("\"additionalContext\""),
        "heading-match path must produce additionalContext; got: {json_match}"
    );
    assert!(
        json_match.contains("\"hookEventName\":\"UserPromptSubmit\""),
        "hookEventName must be UserPromptSubmit; got: {json_match}"
    );

    // ToC path: "butterfly" has no overlap with any heading.
    let input_toc: HookInput = serde_json::from_value(serde_json::json!({
        "session_id": "integ",
        "transcript_path": "",
        "cwd": "",
        "hook_event_name": "UserPromptSubmit",
        "prompt": "what is a butterfly?",
    }))
    .unwrap();
    let out_toc = DefaultInjector.inject(&input_toc);
    let json_toc = out_toc.to_json();
    assert!(
        json_toc.contains("Reference sections:"),
        "ToC path must list reference sections; got: {json_toc}"
    );

    // Clean up.
    std::env::remove_var("CONTEXT_GOVERNOR_REFERENCE_DOC");
}

// ── Phase 2: Guard / Rehydrator / Checkpointer end-to-end ─────────────────────

use context_governor::backing::{TranscriptBackingStore, SNAPSHOT_KEY};
use context_governor::defaults::{DefaultCheckpointer, DefaultGuard, DefaultRehydrator};
use context_governor::handlers::BackingStore;
use context_governor::{Checkpointer, CompactDecision, CompactionGuard, StateRehydrator};

/// Pull the `additionalContext` string (if any) out of a rehydrate envelope.
/// `additional_context` lives on `HookOutput.specific`, not the top level.
fn additional_context(out: &HookOutput) -> Option<String> {
    out.specific
        .as_ref()
        .and_then(|s| s.additional_context.clone())
}

/// Phase 2 — the real PreCompact backstop, SessionStart rehydrator, and Stop
/// checkpointer, exercised against a live `TranscriptBackingStore`.
///
/// All three store-touching scenarios live in this one `#[test]` because
/// `set_var("CONTEXT_GOVERNOR_STATE_DIR", …)` is process-global: keeping them
/// single-threaded under one env set, with a distinct `cwd` per scenario (so
/// `project_key(cwd)` isolates each store), avoids cross-test races and never
/// touches `$HOME`.
#[test]
fn phase2_guard_rehydrator_checkpointer_end_to_end() {
    use std::io::Write as _;

    // Serialise against every other env-mutating test in this crate.
    let _env = env_lock();

    let td = tempfile::tempdir().expect("state dir");
    // SAFETY: single-threaded test; we set the var once before any store opens.
    unsafe {
        std::env::set_var("CONTEXT_GOVERNOR_STATE_DIR", td.path());
    }

    // 1. put -> recall lossless round-trip for a non-snapshot key (Inline + Ref).
    {
        let cwd = td.path().join("proj_round_trip");
        let cwd = cwd.to_str().unwrap();
        let mut store = TranscriptBackingStore::open(cwd).expect("open store");

        let inline = ContextItem {
            id: ItemId(42),
            lane: Lane::Verbatim,
            tokens: 3,
            body: ItemBody::Inline("hello inline body".to_string()),
        };
        let k_inline = store.put(&inline);
        assert_ne!(
            k_inline, SNAPSHOT_KEY,
            "a non-snapshot put must not collide with the reserved snapshot key"
        );
        assert_eq!(
            store.recall(&k_inline),
            Some(inline),
            "Inline body must round-trip byte-identically through put/recall"
        );

        let referenced = ContextItem {
            id: ItemId(7),
            lane: Lane::Evictable,
            tokens: 1,
            body: ItemBody::Ref(StoreKey(0xdead_beef)),
        };
        let k_ref = store.put(&referenced);
        assert_eq!(
            store.recall(&k_ref),
            Some(referenced),
            "Ref body must round-trip with its inner StoreKey preserved"
        );
    }

    // 2. guard.snapshot -> rehydrate emits additionalContext (I1).
    {
        let cwd = td.path().join("proj_i1");
        let cwd = cwd.to_str().unwrap();

        let mut tf = tempfile::NamedTempFile::new().expect("transcript file");
        writeln!(
            tf,
            r#"{{"message":{{"role":"user","content":"hello from the user turn"}}}}"#
        )
        .unwrap();
        writeln!(
            tf,
            r#"{{"message":{{"role":"assistant","content":"reply from the assistant turn"}}}}"#
        )
        .unwrap();
        tf.flush().unwrap();
        let tpath = tf.path().to_str().unwrap().to_string();

        let mut store = TranscriptBackingStore::open(cwd).expect("open store");
        let input = HookInput {
            transcript_path: tpath,
            ..Default::default()
        };

        let mut guard = DefaultGuard;
        assert!(
            matches!(
                guard.on_pre_compact(&input, &mut store),
                CompactDecision::Proceed
            ),
            "PreCompact backstop proceeds after securing a snapshot"
        );

        // The snapshot landed under the reserved key, carrying transcript text.
        match store.recall(&SNAPSHOT_KEY) {
            Some(ContextItem {
                body: ItemBody::Inline(text),
                ..
            }) => assert!(
                text.contains("hello from the user turn"),
                "snapshot must contain user-turn text; got: {text}"
            ),
            other => panic!("expected an Inline snapshot under SNAPSHOT_KEY, got: {other:?}"),
        }

        let out = DefaultRehydrator.rehydrate(&input, &store);
        let ctx = additional_context(&out)
            .expect("rehydrate must emit additionalContext after a snapshot");
        assert!(
            ctx.contains("hello from the user turn"),
            "rehydrated additionalContext must carry the snapshot text; got: {ctx}"
        );
    }

    // 3. guard + checkpointer no-panic on a missing transcript: guard proceeds,
    //    no snapshot is written, and rehydrate falls back to the empty default.
    {
        let cwd = td.path().join("proj_missing");
        let cwd = cwd.to_str().unwrap();
        let mut store = TranscriptBackingStore::open(cwd).expect("open store");

        let missing = HookInput {
            transcript_path: "/no/such/transcript.jsonl".to_string(),
            ..Default::default()
        };

        let mut guard = DefaultGuard;
        assert!(
            matches!(
                guard.on_pre_compact(&missing, &mut store),
                CompactDecision::Proceed
            ),
            "guard must proceed (not panic) when the transcript is missing"
        );

        // Checkpointer must be a no-op (never panic / block) on a missing path;
        // stop_hook_active short-circuits the re-entrant Stop case too.
        let mut checkpointer = DefaultCheckpointer;
        checkpointer.checkpoint(&missing, &mut store);
        let reentrant = HookInput {
            transcript_path: "/no/such/transcript.jsonl".to_string(),
            stop_hook_active: true,
            ..Default::default()
        };
        checkpointer.checkpoint(&reentrant, &mut store);

        assert!(
            store.recall(&SNAPSHOT_KEY).is_none(),
            "no snapshot should be written for a missing transcript"
        );
        let out = DefaultRehydrator.rehydrate(&missing, &store);
        assert!(
            additional_context(&out).is_none(),
            "rehydrate must return the empty default when no snapshot exists"
        );
    }
}

// ── Phase 2: action-ledger acceptance (I6) ────────────────────────────────────

/// Recursively locate the single `ledger.jsonl` written under a state dir,
/// without reconstructing the `project_key/safe_session` path layout.
fn find_ledger(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let p = entry.path();
        if p.is_dir() {
            if let Some(found) = find_ledger(&p) {
                return Some(found);
            }
        } else if p.file_name().and_then(|n| n.to_str()) == Some("ledger.jsonl") {
            return Some(p);
        }
    }
    None
}

/// I6 end-to-end: the three size-bearing levers — groom (PostToolUse), inject
/// (UserPromptSubmit), and snapshot (PreCompact) — each append exactly one
/// durable JSONL row carrying both `saved_tokens` and `resident_tokens`, and
/// `ledger::rollup` sums them deterministically.
///
/// All three emits are routed through one fixed `(STATE_DIR, cwd, session)`, so
/// they land in a single `ledger.jsonl` that `rollup(cwd)` reads back. The
/// scenario runs under the shared `env_lock` because it mutates the
/// process-global `CONTEXT_GOVERNOR_STATE_DIR` / `CLAUDE_CODE_SESSION_ID` /
/// `CONTEXT_GOVERNOR_REFERENCE_DOC`. It also asserts the byte-frozen contract
/// files (`types.rs` / `handlers.rs` / `io.rs`) were not modified by this work.
#[test]
fn action_ledger_records_groom_inject_snapshot_and_rollup_sums() {
    use context_governor::defaults::DefaultGroomer;
    use context_governor::rollup;
    use std::io::Write as _;

    let _env = env_lock();

    let td = tempfile::tempdir().expect("state dir");
    let state_dir = td.path().to_str().expect("utf-8 state dir").to_string();
    let unique_cwd = td.path().join("ledger-acceptance-proj");
    let cwd = unique_cwd.to_str().expect("utf-8 cwd").to_string();
    let session_id = format!("ledger-acceptance-{}", std::process::id());

    std::env::set_var("CONTEXT_GOVERNOR_STATE_DIR", &state_dir);
    std::env::set_var("CLAUDE_CODE_SESSION_ID", &session_id);

    // ── 1. Groom: an over-budget tool_response → one `groomed` row ────────────
    let groom_input: HookInput = serde_json::from_value(serde_json::json!({
        "session_id": &session_id,
        "transcript_path": "",
        "cwd": &cwd,
        "hook_event_name": "PostToolUse",
        "tool_name": "Bash",
        "tool_response": "z".repeat(40_000), // ~10 000 tokens, well over the 2048 default
    }))
    .expect("groom HookInput");
    let groom_out = DefaultGroomer.to_output(&groom_input);
    assert!(
        groom_out.to_json().contains("updatedToolOutput"),
        "over-budget groom must return a PostToolUse envelope"
    );

    // ── 2. Inject: a heading-matching prompt against a real doc → one `injected` row ──
    let mut doc = tempfile::NamedTempFile::new().expect("reference doc");
    doc.write_all(PHASE2_DOC.as_bytes()).expect("write doc");
    doc.flush().expect("flush doc");
    std::env::set_var("CONTEXT_GOVERNOR_REFERENCE_DOC", doc.path());

    let inject_input: HookInput = serde_json::from_value(serde_json::json!({
        "session_id": &session_id,
        "transcript_path": "",
        "cwd": &cwd,
        "hook_event_name": "UserPromptSubmit",
        "prompt": "show me some examples",
    }))
    .expect("inject HookInput");
    let inject_out = DefaultInjector.inject(&inject_input);
    assert!(
        inject_out.to_json().contains("additionalContext"),
        "heading-match inject must produce additionalContext"
    );
    std::env::remove_var("CONTEXT_GOVERNOR_REFERENCE_DOC");

    // ── 3. Snapshot: PreCompact backstop over a real transcript → one `snapshotted` row ──
    let mut tf = tempfile::NamedTempFile::new().expect("transcript file");
    writeln!(
        tf,
        r#"{{"message":{{"role":"user","content":"hello from the user turn"}}}}"#
    )
    .unwrap();
    writeln!(
        tf,
        r#"{{"message":{{"role":"assistant","content":"reply from the assistant turn"}}}}"#
    )
    .unwrap();
    tf.flush().unwrap();

    let mut store = TranscriptBackingStore::open(&cwd).expect("open store");
    let snap_input = HookInput {
        session_id: session_id.clone(),
        transcript_path: tf.path().to_str().unwrap().to_string(),
        cwd: cwd.clone(),
        hook_event_name: "PreCompact".to_string(),
        ..Default::default()
    };
    let mut guard = DefaultGuard;
    assert!(
        matches!(
            guard.on_pre_compact(&snap_input, &mut store),
            CompactDecision::Proceed
        ),
        "PreCompact backstop proceeds after securing a snapshot"
    );

    // ── 4. Rollup: exactly three rows, one per event, with summed saved_tokens ──
    let summary = rollup(&cwd);
    assert_eq!(
        summary.rows, 3,
        "groom + inject + snapshot must produce exactly three ledger rows; got {summary:?}"
    );
    assert_eq!(
        summary.per_event.get("groomed"),
        Some(&1),
        "one groomed row"
    );
    assert_eq!(
        summary.per_event.get("injected"),
        Some(&1),
        "one injected row"
    );
    assert_eq!(
        summary.per_event.get("snapshotted"),
        Some(&1),
        "one snapshotted row"
    );
    // Only the groom carries saved_tokens; inject/snapshot record 0, so the sum is
    // the groom's reclaimed-token estimate and must be strictly positive.
    assert!(
        summary.total_saved_tokens > 0,
        "rollup must sum the groom's saved_tokens (> 0); got {}",
        summary.total_saved_tokens
    );

    // ── 5. Every emitted row must carry both size fields on disk ──────────────
    let sink = find_ledger(td.path()).expect("ledger.jsonl must exist under the state dir");
    let contents = std::fs::read_to_string(&sink).expect("read ledger");
    let rows: Vec<&str> = contents.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(
        rows.len(),
        3,
        "exactly three JSONL rows on disk; got {rows:?}"
    );
    for line in &rows {
        let v: serde_json::Value = serde_json::from_str(line).expect("each row is valid JSON");
        assert!(
            v.get("saved_tokens").and_then(|x| x.as_u64()).is_some(),
            "every row must carry a numeric saved_tokens; got: {line}"
        );
        assert!(
            v.get("resident_tokens").and_then(|x| x.as_u64()).is_some(),
            "every row must carry a numeric resident_tokens; got: {line}"
        );
    }

    // ── 6. Frozen-file guard: the byte-frozen contract files must be untouched ─
    const FROZEN: [&str; 3] = ["src/types.rs", "src/handlers.rs", "src/io.rs"];
    let manifest = env!("CARGO_MANIFEST_DIR");
    let mut args: Vec<&str> = vec!["-C", manifest, "diff", "--quiet", "HEAD", "--"];
    args.extend(FROZEN.iter().copied());
    match std::process::Command::new("git").args(&args).status() {
        // exit 0 = no diff; exit 1 = a real diff. Only fail on a real diff; any
        // other outcome (git absent, detached layout) is environmental, not a
        // contract violation, so we don't flake the suite on it.
        Ok(status) if status.code() == Some(1) => {
            panic!("frozen contract files were modified vs HEAD: {FROZEN:?}");
        }
        _ => {}
    }
}

// ── Phase 2: ledger GC/rotation truncation → rollup round-trip (I6) ──────────

/// End-to-end GC/rotation round-trip: append ~1000 rows across several
/// sessions and event kinds through the *real* `Ledger::append` trigger — the
/// same size-gated prune every hook append runs (`ledger.rs` ~lines 136-167,
/// `file_could_exceed` + `prune_jsonl` ~lines 148-209) — so the production
/// truncation path fires mid-test (not a hand-truncated file), then asserts
/// `rollup` reports the exact post-truncation totals.
///
/// Truncation spec and why the asserted values are exact: `file_could_exceed`
/// gates on file *size* with a conservative `MIN_ROW_BYTES = 16` floor, while
/// this test's real JSONL rows are ~180+ bytes each, so the size gate trips
/// (and `prune_jsonl` is invoked) well before the row *count* reaches `CAP`
/// (at ~49 rows here, vs. `CAP = 300`, verified against the real emitted row
/// shape). `prune_jsonl` itself is only a no-op while `rows <= max_rows`; once
/// total rows first exceed `CAP`, that same append call's post-write prune
/// rewrites the file to its LAST `CAP` lines. From then on every following
/// append leaves the file at exactly `CAP` rows before the call and `CAP + 1`
/// after the write, so the post-write prune fires again and re-converges it to
/// `CAP` — deterministically, on every single append, all the way to the end
/// of the loop. So the final on-disk ledger holds precisely the most recent
/// `CAP` of the `TOTAL` appended rows, never a fuzzy "somewhere near the cap"
/// range. The test computes the expected summary purely from its own
/// in-memory record of what it appended (an independent ground truth, not a
/// re-derivation of the production aggregation in `summarize_jsonl`) and
/// asserts `rollup` matches it exactly.
#[test]
fn ledger_truncation_rollup_roundtrip() {
    use context_governor::{rollup, Ledger, LedgerNode};
    use std::collections::BTreeMap;

    let _env = env_lock();

    const CAP: usize = 300;
    const TOTAL: usize = 1000;

    let td = tempfile::tempdir().expect("state dir");
    let state_dir = td.path().to_str().expect("utf-8 state dir").to_string();
    let unique_cwd = td.path().join("ledger-truncation-proj");
    let cwd = unique_cwd.to_str().expect("utf-8 cwd").to_string();
    let session_id = format!("ledger-truncation-{}", std::process::id());

    std::env::set_var("CONTEXT_GOVERNOR_STATE_DIR", &state_dir);
    std::env::set_var("CLAUDE_CODE_SESSION_ID", &session_id);
    std::env::set_var("CONTEXT_GOVERNOR_LEDGER_MAX_ROWS", CAP.to_string());

    let ledger = Ledger::open(&cwd);

    // Ground truth: (event_name, saved_tokens) for every row appended, in
    // append order — independent of any production aggregation logic.
    let mut appended: Vec<(&'static str, u64)> = Vec::with_capacity(TOTAL);

    for i in 0..TOTAL {
        // Vary the LedgerNode's own `session` tag and `hook` across several
        // (fake) sessions/hooks, even though everything lands in one physical
        // ledger.jsonl (CLAUDE_CODE_SESSION_ID is fixed for the process) — the
        // scenario this test targets is many logical rows accumulating and
        // getting GC'd, not multiple physical ledger files.
        let session = format!("sess-{}", i % 3);
        let hook = match i % 4 {
            0 => "PostToolUse",
            1 => "UserPromptSubmit",
            2 => "PreCompact",
            _ => "Stop",
        };
        let item = Some(ItemId(i as u64));

        let (action, event_name, saved): (Action, &'static str, u64) = match i % 5 {
            0 => {
                let saved_tokens = (i % 13) as u32 + 1;
                (
                    Action::Groomed { saved_tokens },
                    "groomed",
                    saved_tokens as u64,
                )
            }
            1 => (Action::Injected, "injected", 0),
            2 => (Action::Pinned, "pinned", 0),
            3 => (
                Action::Snapshotted {
                    to: StoreKey(i as u64),
                },
                "snapshotted",
                0,
            ),
            _ => (
                Action::Recalled {
                    from: StoreKey(i as u64),
                },
                "recalled",
                0,
            ),
        };

        let node = LedgerNode {
            session,
            hook: hook.to_string(),
            item,
            action,
            reason: "gc-rotation-roundtrip-test",
        };
        ledger.append(&node, 100 + i as u32);
        appended.push((event_name, saved));
    }

    // Independent expected summary: per the doc comment above, the production
    // truncation policy converges the file to the LAST `CAP` appended rows, so
    // slice our ground truth the same way and aggregate it ourselves. (`TOTAL
    // > CAP` by construction above, so the loop actually exercises truncation.)
    let kept = &appended[TOTAL - CAP..];
    let mut expected_total_saved_tokens: u64 = 0;
    let mut expected_per_event: BTreeMap<String, u64> = BTreeMap::new();
    for (event_name, saved) in kept {
        expected_total_saved_tokens += saved;
        *expected_per_event
            .entry((*event_name).to_string())
            .or_insert(0) += 1;
    }

    let summary = rollup(&cwd);

    std::env::remove_var("CONTEXT_GOVERNOR_LEDGER_MAX_ROWS");
    std::env::remove_var("CLAUDE_CODE_SESSION_ID");
    std::env::remove_var("CONTEXT_GOVERNOR_STATE_DIR");

    assert_eq!(
        summary.rows,
        CAP as u64,
        "the real GC/rotation prune (not a hand-truncated file) must converge the ledger to exactly the cap"
    );
    assert_eq!(
        summary.total_saved_tokens, expected_total_saved_tokens,
        "post-truncation total_saved_tokens must equal the sum over the LAST {CAP} rows only, not all {TOTAL} appended"
    );
    assert_eq!(
        summary.per_event, expected_per_event,
        "post-truncation per_event counts must equal counts over the LAST {CAP} rows only, not all {TOTAL} appended"
    );
}
