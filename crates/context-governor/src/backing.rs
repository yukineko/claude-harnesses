//! Default [`BackingStore`] backed by a path-based JSON store rooted at a
//! per-session `state_dir` — the parallel-session-safe externalization substrate
//! the rest of the harness already uses (`harness_core::store`).
//!
//! Phase 2 resolves the four round-trip bodies: `open` derives the session-scoped
//! `state_dir`; `snapshot_transcript` captures a bounded transcript excerpt into a
//! single fixed slot; `put`/`recall` round-trip individual items losslessly.
//!
//! NEVER-BREAK-A-TURN: every path that runs for a real hook degrades rather than
//! panicking — no `unwrap`/`expect`/`panic`. Reads fail soft to `None`; writes are
//! best-effort via [`harness_core::store::save_json`] (atomic, fail-soft).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::handlers::BackingStore;
use crate::types::{ContextItem, ItemBody, ItemId, Lane, StoreKey};
use harness_core::store::{context_state_dir, save_json};
use harness_core::transcript::recent_turns;

/// Fixed handle for the singleton transcript snapshot slot. `snapshot_transcript`
/// always writes ONE slot (`state_dir/snapshot.json`) and returns this key, so the
/// rehydrator can recall exactly this key without enumerating anything. The value
/// is the ASCII of "snap" (`0x736e6170`) — stable and human-recognizable in dumps.
pub const SNAPSHOT_KEY: StoreKey = StoreKey(0x736e_6170);

/// Externalizes context to a path-based JSON store rooted at `state_dir`. Keyed by
/// project + session so parallel sessions never clobber each other's snapshots
/// (the same fallback discipline `harness_core::store` enforces).
pub struct TranscriptBackingStore {
    state_dir: PathBuf,
}

impl TranscriptBackingStore {
    /// Open (or lazily create) the store under `cwd`. The dispatch binary calls
    /// this once per invocation and `.expect()`s it, so this must create the state
    /// dir and return `Ok` on any normal path — an `Err` here silently kills the
    /// turn's handler. Directory-creation failures degrade (best-effort) rather
    /// than propagating.
    pub fn open(cwd: &str) -> anyhow::Result<Self> {
        // Session-scoped state dir via the shared derivation (base env +
        // canonicalize + project_key + safe_session). Centralizing this in
        // `harness_core::store::context_state_dir` keeps the backing store, the
        // ledger writer, and the session-insights reader from drifting on the
        // path — canonicalization (macOS /var→/private/var, trailing slash,
        // relative vs absolute) and the "default" unset-session fallback are
        // applied identically for all three. Repo standard env var is
        // CLAUDE_CODE_SESSION_ID; unset/empty → "default".
        let sid = std::env::var("CLAUDE_CODE_SESSION_ID").unwrap_or_default();
        let state_dir = context_state_dir(Path::new(cwd), &sid);

        // Best-effort: never Err on a normal path (the bin .expect()s open()).
        let _ = std::fs::create_dir_all(&state_dir);

        Ok(Self { state_dir })
    }

    /// Path holding the singleton transcript snapshot.
    fn snapshot_path(&self) -> PathBuf {
        self.state_dir.join("snapshot.json")
    }

    /// Path holding the externalized item for `key`.
    fn item_path(&self, key: &StoreKey) -> PathBuf {
        self.state_dir
            .join("items")
            .join(format!("{:016x}.json", key.0))
    }
}

impl BackingStore for TranscriptBackingStore {
    fn snapshot_transcript(&mut self, transcript_path: &str) -> StoreKey {
        // Bounded read — never load the whole transcript (recent_turns streams).
        let turns = recent_turns(transcript_path, 40, 24_000);
        if turns.is_empty() {
            // Missing/empty transcript: no excerpt to store. Return the fixed key
            // without panicking; recall(SNAPSHOT_KEY) stays None until a real
            // snapshot lands.
            return SNAPSHOT_KEY;
        }

        let mut excerpt = String::new();
        for t in &turns {
            excerpt.push_str(&t.role);
            excerpt.push_str(": ");
            excerpt.push_str(&t.text);
            excerpt.push_str("\n\n");
        }
        let excerpt = excerpt.trim_end().to_string();
        let tokens = (excerpt.chars().count() / 4) as u32;

        let item = ContextItem {
            id: ItemId(SNAPSHOT_KEY.0),
            lane: Lane::Verbatim,
            tokens,
            body: ItemBody::Inline(excerpt),
        };
        save_json(&self.snapshot_path(), &ContextItemDto::from(&item));
        SNAPSHOT_KEY
    }

    fn put(&mut self, item: &ContextItem) -> StoreKey {
        // The handle is derived from the item's stable identity, so recall by the
        // returned key reads back exactly this item.
        let key = StoreKey(item.id.0);
        save_json(&self.item_path(&key), &ContextItemDto::from(item));
        key
    }

    fn recall(&self, key: &StoreKey) -> Option<ContextItem> {
        let path = if *key == SNAPSHOT_KEY {
            self.snapshot_path()
        } else {
            self.item_path(key)
        };
        read_item(&path)
    }
}

/// Read an externalized item back, fail-soft. Missing file / parse error → `None`
/// (never a panic), so an unknown key or a corrupt store degrades cleanly.
fn read_item(path: &Path) -> Option<ContextItem> {
    let raw = std::fs::read_to_string(path).ok()?;
    let dto: ContextItemDto = serde_json::from_str(&raw).ok()?;
    Some(ContextItem::from(dto))
}

// ── Serde DTOs ───────────────────────────────────────────────────────────────
//
// `crate::types::{ContextItem, ItemBody, Lane}` deliberately carry no serde
// derives (the frozen contract stays serialization-agnostic). These private DTOs
// mirror them 1:1 so the on-disk round-trip preserves id, lane (all 3 variants),
// tokens, and body (Inline byte-identical; Ref inner StoreKey value).

#[derive(Serialize, Deserialize)]
enum LaneDto {
    Pinned,
    Verbatim,
    Evictable,
}

#[derive(Serialize, Deserialize)]
enum ItemBodyDto {
    Inline(String),
    Ref(u64),
}

#[derive(Serialize, Deserialize)]
struct ContextItemDto {
    id: u64,
    lane: LaneDto,
    tokens: u32,
    body: ItemBodyDto,
}

impl From<&ContextItem> for ContextItemDto {
    fn from(i: &ContextItem) -> Self {
        ContextItemDto {
            id: i.id.0,
            lane: match i.lane {
                Lane::Pinned => LaneDto::Pinned,
                Lane::Verbatim => LaneDto::Verbatim,
                Lane::Evictable => LaneDto::Evictable,
            },
            tokens: i.tokens,
            body: match &i.body {
                ItemBody::Inline(s) => ItemBodyDto::Inline(s.clone()),
                ItemBody::Ref(k) => ItemBodyDto::Ref(k.0),
            },
        }
    }
}

impl From<ContextItemDto> for ContextItem {
    fn from(d: ContextItemDto) -> Self {
        ContextItem {
            id: ItemId(d.id),
            lane: match d.lane {
                LaneDto::Pinned => Lane::Pinned,
                LaneDto::Verbatim => Lane::Verbatim,
                LaneDto::Evictable => Lane::Evictable,
            },
            tokens: d.tokens,
            body: match d.body {
                ItemBodyDto::Inline(s) => ItemBody::Inline(s),
                ItemBodyDto::Ref(k) => ItemBody::Ref(StoreKey(k)),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A store rooted under a private temp dir (no env / no shared state), so
    /// put/recall/snapshot tests never race the process-global environment.
    fn temp_store(name: &str) -> TranscriptBackingStore {
        let dir = std::env::temp_dir().join(format!(
            "cg-backing-{}-{}-{:p}",
            name,
            std::process::id(),
            &name
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp state dir");
        TranscriptBackingStore { state_dir: dir }
    }

    fn item(id: u64, lane: Lane, body: ItemBody) -> ContextItem {
        ContextItem {
            id: ItemId(id),
            lane,
            tokens: 42,
            body,
        }
    }

    #[test]
    fn open_is_ok_and_creates_state_dir_for_any_cwd() {
        // Serialise against the defaults tests that also mutate these process-global
        // env vars (groomer / injector / snapshot), via the crate-shared lock.
        let _env = crate::defaults::guard::acquire_env_lock();
        // Isolate the base so the test never touches a real $HOME/.context-governor.
        let base = std::env::temp_dir().join(format!("cg-open-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::env::set_var("CONTEXT_GOVERNOR_STATE_DIR", &base);
        std::env::set_var("CLAUDE_CODE_SESSION_ID", "test-session-A");

        for cwd in ["", "/any/path"] {
            let store = TranscriptBackingStore::open(cwd).expect("open must not Err");
            assert!(
                store.state_dir.is_dir(),
                "open({cwd:?}) must create state_dir: {:?}",
                store.state_dir
            );
            assert!(store.state_dir.starts_with(&base));
        }

        std::env::remove_var("CONTEXT_GOVERNOR_STATE_DIR");
        std::env::remove_var("CLAUDE_CODE_SESSION_ID");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn put_recall_roundtrips_all_lanes_inline() {
        let mut store = temp_store("inline");
        for (id, lane) in [
            (1u64, Lane::Pinned),
            (2, Lane::Verbatim),
            (3, Lane::Evictable),
        ] {
            let body = ItemBody::Inline(format!("body for {id} — café 日本語 \u{1F600}"));
            let original = item(id, lane, body);
            let key = store.put(&original);
            let back = store.recall(&key).expect("recall after put");
            assert_eq!(back, original, "lossless round-trip for {lane:?}");
            // Body text must be byte-identical. Assert on the shape via a bool
            // flag rather than `panic!`-ing in the unexpected-variant arm, so a
            // regression here yields a normal assertion failure (clean, message
            // carries context) instead of an ad hoc panic in test code.
            let both_inline = matches!(
                (&back.body, &original.body),
                (ItemBody::Inline(_), ItemBody::Inline(_))
            );
            assert!(
                both_inline,
                "expected inline bodies, got {back:?} / {original:?}"
            );
            match (&back.body, &original.body) {
                (ItemBody::Inline(a), ItemBody::Inline(b)) => assert_eq!(a, b),
                _ => unreachable!("shape already asserted above"),
            }
        }
    }

    #[test]
    fn put_recall_roundtrips_ref_body() {
        let mut store = temp_store("ref");
        let original = item(7, Lane::Evictable, ItemBody::Ref(StoreKey(0xdead_beef)));
        let key = store.put(&original);
        let back = store.recall(&key).expect("recall after put");
        assert_eq!(back, original);
        // Assert the shape via a bool flag rather than `panic!`-ing in the
        // unexpected-variant arm, so a regression yields a normal assertion
        // failure instead of an ad hoc panic in test code.
        let is_ref = matches!(back.body, ItemBody::Ref(_));
        assert!(is_ref, "expected Ref body, got {:?}", back.body);
        match back.body {
            ItemBody::Ref(k) => assert_eq!(k, StoreKey(0xdead_beef)),
            _ => unreachable!("shape already asserted above"),
        }
    }

    #[test]
    fn recall_unknown_key_is_none() {
        let store = temp_store("unknown");
        assert!(store.recall(&StoreKey(0x1234)).is_none());
        // Before any snapshot, the snapshot slot is also empty.
        assert!(store.recall(&SNAPSHOT_KEY).is_none());
    }

    #[test]
    fn snapshot_captures_transcript_and_recalls_excerpt() {
        let mut store = temp_store("snapshot");
        // Minimal JSONL transcript with two real turns.
        let tpath = store.state_dir.join("transcript.jsonl");
        let jsonl = concat!(
            "{\"message\":{\"role\":\"user\",\"content\":\"please refactor the parser\"}}\n",
            "{\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"done, parser refactored\"}]}}\n",
        );
        std::fs::write(&tpath, jsonl).unwrap();

        let key = store.snapshot_transcript(tpath.to_str().unwrap());
        assert_eq!(key, SNAPSHOT_KEY, "snapshot must return the fixed key");

        let snap = store.recall(&SNAPSHOT_KEY).expect("recall snapshot");
        assert_eq!(snap.lane, Lane::Verbatim);
        // Assert the shape via a bool flag rather than `panic!`-ing in the
        // unexpected-variant arm, so a regression yields a normal assertion
        // failure instead of an ad hoc panic in test code.
        let is_inline = matches!(snap.body, ItemBody::Inline(_));
        assert!(
            is_inline,
            "snapshot body must be Inline, got {:?}",
            snap.body
        );
        match snap.body {
            ItemBody::Inline(s) => {
                assert!(!s.is_empty(), "excerpt must be non-empty");
                assert!(
                    s.contains("refactor"),
                    "excerpt should carry the turn text: {s}"
                );
            }
            _ => unreachable!("shape already asserted above"),
        }
    }

    #[test]
    fn snapshot_missing_path_does_not_panic() {
        let mut store = temp_store("missing");
        let key = store.snapshot_transcript("/no/such/transcript.jsonl");
        assert_eq!(key, SNAPSHOT_KEY);
        // No excerpt was stored, so the slot stays empty.
        assert!(store.recall(&SNAPSHOT_KEY).is_none());
    }

    #[test]
    fn snapshot_key_is_public_const() {
        // Compile-time guarantee the rehydrator can name the same key.
        const _K: StoreKey = SNAPSHOT_KEY;
        assert_eq!(SNAPSHOT_KEY.0, 0x736e_6170);
    }
}
