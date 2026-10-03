//! Default [`StateRehydrator`] — SessionStart restore. Re-injects normative core
//! / verbatim from the backing store so pins survive compaction (I1) and resume
//! reseeds durably. Most relevant on `source == "compact"`, which the shipped hooks.json SessionStart
//! matcher (`startup|resume|clear|compact`) routes to this handler.
//!
//! Lane-aware re-injection (backlog b9ab97a7): the raw snapshot is no longer
//! re-injected verbatim and whole. Instead the rehydrator recomputes the
//! [`SpecClassifier`](crate::handlers::SpecClassifier) decision for the snapshot
//! body, applies a `SpecClass → Lane` rule, and re-injects only the sections that
//! land in a *resident* lane (`Pinned`/`Verbatim`), each tagged with a pinning
//! marker so it survives as a resident norm (I1). `Evictable` (`ReferenceBody`)
//! sections are dropped from the resident re-injection — they are situational and
//! belong to retrieval, not the standing window.

use crate::backing::SNAPSHOT_KEY;
use crate::defaults::classifier::DefaultClassifier;
use crate::handlers::{BackingStore, SpecClassifier, StateRehydrator};
use crate::io::HookOutput;
use crate::types::{ItemBody, Lane};
use harness_core::hook::HookInput;

/// Marker prefix that tags a re-injected section as a resident pin. Its presence
/// is what lets a downstream consumer (or a test) tell a pinned/verbatim norm
/// apart from ordinary injected reference text — these survive as resident norms.
pub(crate) const PIN_MARKER: &str = "[pinned]";

/// `true` for the lanes that are *resident*: they must always be present in the
/// final context (`Pinned`, I1) or round-trip losslessly (`Verbatim`, I2). These
/// are the lanes the rehydrator re-injects with a pin marker. `Evictable` is not
/// resident — it is dropped from the SessionStart re-injection.
fn is_resident_lane(lane: Lane) -> bool {
    matches!(lane, Lane::Pinned | Lane::Verbatim)
}

/// Apply lane decisions to the snapshot body: classify it into sections, keep only
/// the resident-lane sections, and prefix each with [`PIN_MARKER`] so it is marked
/// to survive as a resident norm. Returns the joined re-injection text, or an empty
/// string when nothing classifies as resident.
fn lane_aware_reinjection(snapshot: &str) -> String {
    let mut pinned: Vec<String> = Vec::new();
    for (_class, item) in DefaultClassifier.classify(snapshot) {
        if !is_resident_lane(item.lane) {
            continue;
        }
        if let ItemBody::Inline(text) = item.body {
            if !text.trim().is_empty() {
                pinned.push(format!("{PIN_MARKER} {text}"));
            }
        }
    }
    pinned.join("\n\n")
}

pub struct DefaultRehydrator;

impl StateRehydrator for DefaultRehydrator {
    fn rehydrate(&self, _i: &HookInput, s: &dyn BackingStore) -> HookOutput {
        match s.recall(&SNAPSHOT_KEY) {
            Some(item) => match item.body {
                ItemBody::Inline(text) if !text.is_empty() => {
                    let reinjected = lane_aware_reinjection(&text);
                    if reinjected.is_empty() {
                        HookOutput::default()
                    } else {
                        HookOutput::inject("SessionStart", reinjected)
                    }
                }
                _ => HookOutput::default(),
            },
            None => HookOutput::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backing::SNAPSHOT_KEY;
    use crate::handlers::BackingStore;
    use crate::types::{ContextItem, ItemBody, ItemId, Lane, StoreKey};
    use harness_core::hook::HookInput;

    /// Minimal in-memory store: serves exactly one snapshot body on
    /// `recall(SNAPSHOT_KEY)`. The other methods are unused by the rehydrator.
    struct OneSnapshotStore {
        snapshot: Option<String>,
    }

    impl BackingStore for OneSnapshotStore {
        fn snapshot_transcript(&mut self, _transcript_path: &str) -> StoreKey {
            SNAPSHOT_KEY
        }
        fn put(&mut self, item: &ContextItem) -> StoreKey {
            StoreKey(item.id.0)
        }
        fn recall(&self, key: &StoreKey) -> Option<ContextItem> {
            if *key == SNAPSHOT_KEY {
                self.snapshot.as_ref().map(|text| ContextItem {
                    id: ItemId(SNAPSHOT_KEY.0),
                    lane: Lane::Verbatim,
                    tokens: 0,
                    body: ItemBody::Inline(text.clone()),
                })
            } else {
                None
            }
        }
    }

    fn dummy_input() -> HookInput {
        // Empty JSON parses to an all-None HookInput; the rehydrator ignores it.
        HookInput::default()
    }

    fn injected_text(out: &HookOutput) -> Option<String> {
        out.specific
            .as_ref()
            .and_then(|h| h.additional_context.clone())
    }

    /// A snapshot mixing a normative section (→ Pinned, resident) and a reference
    /// section (→ Evictable, not resident). Lane application must re-inject the
    /// normative one as a resident pin and must NOT re-inject the reference one.
    const MIXED: &str = "\
# Acceptance Criteria
Every request MUST be authenticated.

# Examples
```
GET /api/users
```
";

    #[test]
    fn pinned_and_verbatim_sections_reinjected_as_resident() {
        let store = OneSnapshotStore {
            snapshot: Some(MIXED.to_string()),
        };
        let out = DefaultRehydrator.rehydrate(&dummy_input(), &store);
        let text = injected_text(&out).expect("resident content must be re-injected");

        // The normative (Pinned) section is present and tagged as a resident pin.
        assert!(
            text.contains(PIN_MARKER),
            "resident sections must carry the pin marker: {text}"
        );
        assert!(
            text.contains("Acceptance Criteria") && text.contains("authenticated"),
            "the normative section must survive re-injection: {text}"
        );
    }

    #[test]
    fn evictable_sections_not_pinned() {
        let store = OneSnapshotStore {
            snapshot: Some(MIXED.to_string()),
        };
        let out = DefaultRehydrator.rehydrate(&dummy_input(), &store);
        let text = injected_text(&out).expect("resident content must be re-injected");

        // The reference (Evictable) section is dropped — not re-injected as resident.
        assert!(
            !text.contains("Examples") && !text.contains("GET /api/users"),
            "Evictable reference body must NOT be re-injected as resident: {text}"
        );
        // And every re-injected block is pin-marked (no un-pinned resident leaks).
        for block in text.split("\n\n") {
            assert!(
                block.trim().is_empty() || block.starts_with(PIN_MARKER),
                "every resident block must be pin-marked: {block}"
            );
        }
    }

    #[test]
    fn reference_only_snapshot_injects_nothing() {
        // A snapshot whose every section is Evictable yields no resident content,
        // so the rehydrator emits the default (no-op) output.
        let store = OneSnapshotStore {
            snapshot: Some("# Examples\nsample body\n".to_string()),
        };
        let out = DefaultRehydrator.rehydrate(&dummy_input(), &store);
        assert_eq!(out, HookOutput::default(), "no resident content → no-op");
    }

    #[test]
    fn absent_snapshot_is_noop() {
        let store = OneSnapshotStore { snapshot: None };
        let out = DefaultRehydrator.rehydrate(&dummy_input(), &store);
        assert_eq!(out, HookOutput::default());
    }
}
