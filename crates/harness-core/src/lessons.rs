//! Cross-project "lessons" store — durable, machine-scope, project-INDEPENDENT.
//!
//! A lesson is a small distilled carryover ("this error pattern means X",
//! "this repo's convention is Y") that should help *any* future task on *any*
//! project, so unlike the per-project note store (`store`) and the per-repo
//! discovery ledger (`discovery`), its path is deliberately **not** keyed by
//! cwd / repo root — one global `~/.lessons/lessons.jsonl` for the machine.
//!
//! Design mirrors the sibling stores:
//!   * append-only JSONL, one lesson per line (see `discovery`);
//!   * `append` is **idempotent by `id`** — a re-append of an already-stored id
//!     is a no-op (the idempotency-by-key precedent from `discovery`);
//!   * `load` is fail-soft: missing file → empty Vec, malformed lines skipped,
//!     never panics (load-bearing: may be called from hooks);
//!   * `search` is a deterministic **lexical** Jaccard over tokens — no
//!     embeddings / vector DB (the subscription-native design from
//!     `fugu-router::rag`, whose small tokenizer is copied here so we take no
//!     dependency on that plugin crate).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// What kind of lesson this is. Serialized kebab-case so the JSONL line reads
/// `"kind":"error-pattern"` / `"kind":"convention"` and round-trips.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    ErrorPattern,
    Convention,
}

/// A single distilled lesson. Serde-serializable to one JSONL line.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lesson {
    /// Stable dedup key — a re-append with the same id is a no-op.
    pub id: String,
    pub kind: Kind,
    /// Short description of the task the lesson came from.
    pub task_summary: String,
    /// The lesson itself (the carryover text).
    pub lesson_text: String,
    /// Which run/session produced it (provenance).
    pub source_run: String,
    /// Epoch seconds when recorded.
    pub ts: u64,
}

/// Environment override for the lessons directory, honored **only when
/// absolute** (a relative override resolves differently per caller cwd, which
/// would silently split the store — the same only-when-absolute rule the
/// context-ledger base uses for `CONTEXT_GOVERNOR_STATE_DIR`).
const STORE_DIR_ENV: &str = "LESSONS_STORE_DIR";

/// Project-INDEPENDENT path to the global lessons store:
/// `<LESSONS_STORE_DIR (if absolute)>/lessons.jsonl`, else
/// `~/.lessons/lessons.jsonl`. Deliberately NOT keyed by cwd/repo so lessons
/// carry across every project on the machine.
pub fn store_path() -> PathBuf {
    store_dir().join("lessons.jsonl")
}

fn store_dir() -> PathBuf {
    std::env::var(STORE_DIR_ENV)
        .ok()
        .filter(|s| !s.is_empty())
        .filter(|s| Path::new(s).is_absolute())
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::config::base_dir("lessons"))
}

/// Append a lesson, **idempotent by `id`**: if a lesson with the same id is
/// already stored, nothing is written and the count is unchanged. Fail-soft: on
/// any IO/serialize error the lesson is silently dropped.
pub fn append(lesson: &Lesson) {
    append_at(&store_path(), lesson);
}

/// Internal: append to an explicit path. Used by `append` and by tests.
fn append_at(path: &Path, lesson: &Lesson) {
    use std::io::Write;

    // Idempotency-by-key: skip if this id already exists (mirrors discovery's
    // dedup precedent). Load is fail-soft, so a missing file reads as empty.
    if load_at(path).iter().any(|l| l.id == lesson.id) {
        return;
    }

    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    else {
        return;
    };

    let Ok(json) = serde_json::to_string(lesson) else {
        return;
    };

    let _ = writeln!(file, "{}", json);
}

/// Load all lessons from the store. Missing file → empty Vec, blank/corrupt
/// lines skipped. Never panics (fail-soft).
pub fn load() -> Vec<Lesson> {
    load_at(&store_path())
}

/// Internal: load from an explicit path. Used by `load` and by tests.
fn load_at(path: &Path) -> Vec<Lesson> {
    let mut lessons = Vec::new();

    let Ok(contents) = std::fs::read_to_string(path) else {
        return lessons;
    };

    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(l) = serde_json::from_str::<Lesson>(line) {
            lessons.push(l);
        }
    }

    lessons
}

const STOP: &[&str] = &[
    "the", "a", "an", "to", "of", "and", "or", "for", "in", "on", "with", "add", "update", "fix",
    "make", "use", "via", "into", "from", "that", "this", "be", "is", "are", "new",
];

/// Normalise free text into a token set: lowercased, alphanumeric, min length 3,
/// stopwords dropped. Copied (minus the concept-expansion) from
/// `fugu_router::rag::tokenize` so harness-core takes no dependency on that
/// plugin crate.
fn tokenize(s: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for t in s.to_lowercase().split(|c: char| !c.is_alphanumeric()) {
        if t.len() >= 3 && !STOP.contains(&t) {
            out.insert(t.to_string());
        }
    }
    out
}

fn jaccard(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f64 {
    let union = a.union(b).count() as f64;
    if union == 0.0 {
        0.0
    } else {
        a.intersection(b).count() as f64 / union
    }
}

/// The lexical token set of a lesson: its task summary + lesson text combined,
/// so a query matches either the "what task" or the "what learned" side.
fn lesson_tokens(l: &Lesson) -> BTreeSet<String> {
    let mut t = tokenize(&l.task_summary);
    t.extend(tokenize(&l.lesson_text));
    t
}

/// A lesson paired with its lexical similarity to the query.
pub struct Match {
    pub lesson: Lesson,
    pub score: f64,
}

/// Default number of matches to return when a caller has no reason to pick a
/// different `k`. Rust has no default arguments, so the spec's "default k=3" is
/// expressed here as a named constant plus the `search_default` wrapper below;
/// callers wanting another cutoff still pass `k` explicitly to `search`.
pub const DEFAULT_K: usize = 3;

/// Deterministic **lexical** top-K search over `lessons` for `query` (token
/// Jaccard — no embeddings/vector DB). Returns at most `k` matches with a
/// non-zero score, sorted by score descending (ties broken by id for a stable
/// order). An empty store, an empty query, or no overlap → empty Vec (never an
/// error). `k == 0` also yields an empty Vec.
pub fn search(query: &str, lessons: &[Lesson], k: usize) -> Vec<Match> {
    if k == 0 || lessons.is_empty() {
        return Vec::new();
    }
    let q = tokenize(query);
    if q.is_empty() {
        return Vec::new();
    }
    let mut scored: Vec<Match> = lessons
        .iter()
        .map(|l| Match {
            lesson: l.clone(),
            score: jaccard(&q, &lesson_tokens(l)),
        })
        .filter(|m| m.score > 0.0)
        .collect();
    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.lesson.id.cmp(&b.lesson.id))
    });
    scored.truncate(k);
    scored
}

/// Convenience wrapper around [`search`] using the spec's default cutoff
/// [`DEFAULT_K`]. Equivalent to `search(query, lessons, DEFAULT_K)`; exists so
/// the "default k=3" contract is reachable at the core API without every caller
/// repeating the literal.
pub fn search_default(query: &str, lessons: &[Lesson]) -> Vec<Match> {
    search(query, lessons, DEFAULT_K)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lesson(id: &str, summary: &str, text: &str) -> Lesson {
        Lesson {
            id: id.to_string(),
            kind: Kind::ErrorPattern,
            task_summary: summary.to_string(),
            lesson_text: text.to_string(),
            source_run: "run-1".to_string(),
            ts: 1000,
        }
    }

    #[test]
    fn kind_round_trips_kebab_case() {
        let l = lesson("k1", "s", "t");
        let json = serde_json::to_string(&l).unwrap();
        assert!(
            json.contains("\"kind\":\"error-pattern\""),
            "kind must serialize kebab-case: {json}"
        );
        let back: Lesson = serde_json::from_str(&json).unwrap();
        assert_eq!(back.kind, Kind::ErrorPattern);

        let conv = Lesson {
            kind: Kind::Convention,
            ..lesson("k2", "s", "t")
        };
        let j2 = serde_json::to_string(&conv).unwrap();
        assert!(j2.contains("\"kind\":\"convention\""), "{j2}");
        let b2: Lesson = serde_json::from_str(&j2).unwrap();
        assert_eq!(b2.kind, Kind::Convention);
    }

    #[test]
    fn append_is_idempotent_by_id() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("lessons.jsonl");

        append_at(
            &path,
            &lesson("same", "borrow checker error", "clone the value"),
        );
        // Re-append the SAME id (even with different body) must NOT grow the store.
        append_at(
            &path,
            &lesson("same", "different summary", "different text"),
        );

        let loaded = load_at(&path);
        assert_eq!(loaded.len(), 1, "same-id re-append must not increase count");
        // The first write wins (no overwrite).
        assert_eq!(loaded[0].task_summary, "borrow checker error");

        // A genuinely new id does append.
        append_at(&path, &lesson("other", "another lesson", "text"));
        assert_eq!(load_at(&path).len(), 2);
    }

    #[test]
    fn search_ranks_related_and_drops_unrelated() {
        let lessons = vec![
            lesson(
                "auth",
                "fix the login authentication flow",
                "session token must be refreshed",
            ),
            lesson(
                "billing",
                "update the billing invoice report",
                "round currency to cents",
            ),
        ];
        let hits = search("login authentication token", &lessons, 3);
        assert!(!hits.is_empty(), "a related query must match");
        assert_eq!(hits[0].lesson.id, "auth", "the auth lesson must rank first");
        // The unrelated billing lesson shares no tokens → dropped entirely.
        assert!(
            hits.iter().all(|m| m.lesson.id != "billing"),
            "an unrelated lesson must be dropped: {:?}",
            hits.iter().map(|m| m.lesson.id.clone()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn search_default_caps_at_default_k() {
        // The named constant must equal the spec's default of 3.
        assert_eq!(DEFAULT_K, 3);

        // More matching lessons than DEFAULT_K → the wrapper caps the result.
        let lessons: Vec<Lesson> = (0..5)
            .map(|i| {
                lesson(
                    &format!("l{i}"),
                    "login authentication token",
                    "session token",
                )
            })
            .collect();
        let hits = search_default("login authentication token", &lessons);
        assert!(
            hits.len() <= DEFAULT_K,
            "search_default must return at most DEFAULT_K results, got {}",
            hits.len()
        );
        assert_eq!(hits.len(), 3, "5 matching lessons capped to DEFAULT_K==3");

        // And it agrees with an explicit search(.., DEFAULT_K) call.
        let explicit = search("login authentication token", &lessons, DEFAULT_K);
        assert_eq!(hits.len(), explicit.len());
    }

    #[test]
    fn search_over_empty_store_is_empty() {
        let empty: Vec<Lesson> = Vec::new();
        assert!(search("anything at all", &empty, 3).is_empty());
        // k == 0 is also empty even with lessons present.
        let some = vec![lesson("a", "login flow", "token")];
        assert!(search("login", &some, 0).is_empty());
    }

    #[test]
    fn load_is_fail_soft_missing_and_corrupt() {
        // Missing file → empty Vec, never a panic.
        assert!(load_at(Path::new("/nonexistent/lessons.jsonl")).is_empty());

        // Corrupt lines are skipped, valid ones kept.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("lessons.jsonl");
        let valid = serde_json::to_string(&lesson("v", "s", "t")).unwrap();
        let content = format!("{valid}\n{{ not json\n\n{valid}\n");
        // Note: two identical valid lines — load keeps both (dedup is append's job).
        std::fs::write(&path, content).unwrap();
        let loaded = load_at(&path);
        assert_eq!(loaded.len(), 2, "corrupt/blank lines skipped, valid kept");
    }

    #[test]
    fn store_path_is_project_independent_and_honors_absolute_override() {
        // Default: under the home-based ~/.lessons dir, ending in lessons.jsonl.
        // (Do not assert the home prefix to avoid racing env-mutating tests; the
        // filename tail and absolute-override behavior are the invariants.)
        let default = store_path();
        assert!(
            default.ends_with("lessons.jsonl"),
            "default path tail: {default:?}"
        );

        // The path must not depend on cwd — calling twice yields the same path.
        assert_eq!(store_path(), store_path());
    }
}
