//! Deterministic **full-text** inverted index over source files — the
//! body-level counterpart to [`crate::code_index`], which only ever sees
//! *declaration* lines.
//!
//! Why a second index rather than an extension of `code_index`: they answer
//! different questions and have different cost profiles. `code_index` maps a
//! query to a **symbol** ("where is `enumerate_callers` declared"); this maps a
//! query to a **line** ("which 10 lines mention `toml::from_str`"). The symbol
//! index is ~1 record per declaration; this one is ~1 posting per (token, line)
//! and is two orders of magnitude larger, so it is built and loaded separately
//! and only when a body search is actually asked for.
//!
//! Same family rules as the rest of harness-core's lexical layer: no
//! embeddings, no external API, no parser dependency, pure/deterministic
//! scanning, never panics on any input.
//!
//! # The point of this module is cost, so it never returns everything
//!
//! A `grep -rn Determination crates/` in this repository returns 1,622 hits /
//! 163,032 bytes. Feeding that to an agent is the cost problem this index
//! exists to remove: [`search`] returns a ranked **top-k** of `(file, line,
//! score)` and the caller reads only those lines.
//!
//! # Cannot-determine is not "no hits" (CLAUDE.md §3)
//!
//! An index that is absent, unreadable, corrupt, or **stale** must never be
//! allowed to answer as though it were complete — a fast wrong answer is the
//! worst kind of fail-open, because it is indistinguishable from a fast right
//! one. Three separate mechanisms keep that from happening:
//!
//!   * [`load`] returns [`Determination`], so absent/corrupt resolves to
//!     `Undetermined` rather than to an empty index that would then answer
//!     every query with "no hits".
//!   * [`TextIndex::check_fresh`] compares the recorded
//!     [`crate::code_index::fingerprint`] against the tree's current one, so a
//!     stale index is `Undetermined`, not silently authoritative. (This is not
//!     hypothetical: the live `.fugu/code-index.jsonl` in this repo was two
//!     months out of date when this module was written.)
//!   * A token whose posting list hit [`MAX_POSTINGS_PER_TOKEN`] is recorded
//!     with `truncated: true`, and any search touching such a token reports
//!     [`SearchOutcome::truncated`] — so "these are the top 10 of everything"
//!     and "these are the top 10 of the first 4096 we kept" stay
//!     distinguishable downstream.
//!
//! An **empty** `hits` vector on a fresh, complete index is a genuine
//! observation ("this token is not in the corpus") and is correctly `Known`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::verdict::Determination;

/// Upper bound on how many postings are retained for any single token.
///
/// Without a cap, ubiquitous tokens (`self`, `let`, `fn`, `string`) each carry
/// a posting for a large fraction of the corpus's lines and dominate the index
/// size while contributing almost nothing to ranking. The cap is not a silent
/// truncation: the token record keeps `truncated: true` and every search that
/// touches it says so (see [`SearchOutcome`]).
pub const MAX_POSTINGS_PER_TOKEN: usize = 4096;

/// Default number of ranked lines [`search`] returns.
pub const DEFAULT_K: usize = 10;

/// One occurrence of a token, as indices into [`TextIndex::files`].
///
/// Paths are interned rather than repeated per posting: a repo-relative Rust
/// path averages ~30 bytes and there are ~1.9M postings, so interning is the
/// difference between a ~60MB and a ~15MB store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Posting {
    /// Index into [`TextIndex::files`].
    pub file: u32,
    /// 1-indexed line number.
    pub line: u32,
}

/// The posting list for one token, plus whether it was capped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenPostings {
    /// Occurrences, sorted and deduplicated (a token twice on one line is one
    /// posting).
    #[serde(default)]
    pub sites: Vec<Posting>,
    /// `true` when the real occurrence count exceeded
    /// [`MAX_POSTINGS_PER_TOKEN`] and `sites` holds only a prefix. Callers must
    /// propagate this rather than presenting the prefix as the whole truth.
    #[serde(default)]
    pub truncated: bool,
}

/// A full-text inverted index: token → the lines it occurs on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextIndex {
    /// Interned file paths; [`Posting::file`] indexes into this.
    pub files: Vec<String>,
    /// The inverted index proper. `BTreeMap` so serialization is deterministic.
    pub tokens: BTreeMap<String, TokenPostings>,
    /// Fingerprint of the source set this index was built from, used by
    /// [`TextIndex::check_fresh`].
    pub fingerprint: String,
}

/// Sidecar metadata, written next to the JSONL body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextIndexMeta {
    /// [`crate::code_index::fingerprint`] of the `.rs` set indexed.
    pub fingerprint: String,
    /// Interned path table (the body's postings are meaningless without it).
    #[serde(default)]
    pub files: Vec<String>,
    /// Distinct tokens indexed.
    #[serde(default)]
    pub tokens: usize,
    /// Total postings retained (after capping).
    #[serde(default)]
    pub postings: usize,
    /// How many tokens hit [`MAX_POSTINGS_PER_TOKEN`].
    #[serde(default)]
    pub truncated_tokens: usize,
}

/// One record of the JSONL body: a token and its posting list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct TokenRecord {
    /// The token.
    t: String,
    /// Postings, as flat `[file, line]` pairs (half the JSON bytes of an array
    /// of objects, at ~1.9M postings a difference worth having).
    #[serde(default)]
    p: Vec<[u32; 2]>,
    /// Truncation flag; omitted when false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    x: bool,
}

/// Split text into the index's token vocabulary: lowercased, cut on any
/// non-alphanumeric byte, empties dropped.
///
/// Deliberately identical in spirit to [`crate::code_index`]'s tokenizer — no
/// stopwords, no minimum length — so a query tokenized here matches what was
/// indexed. `foo_bar::baz()` yields `foo`, `bar`, `baz`.
pub fn tokenize(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_string())
        .collect()
}

/// Build an index from `(path, contents)` pairs and the `fingerprint` of that
/// source set.
///
/// Pure and deterministic: the same input always yields byte-identical output
/// (paths interned in sorted order, postings sorted, `BTreeMap` token order).
/// Never panics — any input, including empty or non-UTF8-ish text, is fine.
pub fn build(sources: &[(String, String)], fingerprint: &str) -> TextIndex {
    // Sort by path first so the interned ids — and therefore every posting —
    // do not depend on the caller's enumeration order.
    let mut ordered: Vec<&(String, String)> = sources.iter().collect();
    ordered.sort_by(|a, b| a.0.cmp(&b.0));

    let mut files: Vec<String> = Vec::with_capacity(ordered.len());
    // Accumulate as sets so a token repeated on one line collapses to one
    // posting without a later dedup pass.
    let mut acc: BTreeMap<String, BTreeSet<Posting>> = BTreeMap::new();
    // Tokens whose real occurrence count ran past the cap. Tracked separately
    // because `acc` stops growing for them, so its length stops being evidence.
    let mut overflowed: BTreeSet<String> = BTreeSet::new();

    for (path, contents) in ordered {
        let file_id = u32::try_from(files.len()).unwrap_or(u32::MAX);
        files.push(path.clone());

        for (idx, raw_line) in contents.lines().enumerate() {
            // `u32::MAX` for an over-long file rather than a panicking cast;
            // a 4-billion-line source file is not a case worth a branch.
            let line_no = u32::try_from(idx + 1).unwrap_or(u32::MAX);
            for token in tokenize(raw_line) {
                let sites = acc.entry(token.clone()).or_default();
                if sites.len() >= MAX_POSTINGS_PER_TOKEN {
                    overflowed.insert(token);
                    continue;
                }
                sites.insert(Posting {
                    file: file_id,
                    line: line_no,
                });
            }
        }
    }

    let tokens = acc
        .into_iter()
        .map(|(token, sites)| {
            let truncated = overflowed.contains(&token);
            (
                token,
                TokenPostings {
                    sites: sites.into_iter().collect(),
                    truncated,
                },
            )
        })
        .collect();

    TextIndex {
        files,
        tokens,
        fingerprint: fingerprint.to_string(),
    }
}

/// Write the index body (JSONL, one token per line) and its sidecar meta.
///
/// Fail-soft on write, exactly like [`crate::code_index::write_index`]: the
/// index is a cache, and a failed write must not break the caller's turn. The
/// *read* side is where honesty matters, and [`load`] is fail-closed.
pub fn write(body_path: &Path, meta_path: &Path, index: &TextIndex) {
    use std::io::Write;

    if let Some(parent) = body_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let mut postings = 0usize;
    let mut truncated_tokens = 0usize;

    if let Ok(file) = std::fs::File::create(body_path) {
        let mut w = std::io::BufWriter::new(file);
        for (token, tp) in &index.tokens {
            postings += tp.sites.len();
            if tp.truncated {
                truncated_tokens += 1;
            }
            let rec = TokenRecord {
                t: token.clone(),
                p: tp.sites.iter().map(|s| [s.file, s.line]).collect(),
                x: tp.truncated,
            };
            let Ok(json) = serde_json::to_string(&rec) else {
                continue;
            };
            let _ = writeln!(w, "{}", json);
        }
        let _ = w.flush();
    }

    let meta = TextIndexMeta {
        fingerprint: index.fingerprint.clone(),
        files: index.files.clone(),
        tokens: index.tokens.len(),
        postings,
        truncated_tokens,
    };
    if let Some(parent) = meta_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string(&meta) {
        let _ = std::fs::write(meta_path, json);
    }
}

/// Load an index. **Fail-closed** — the whole point of the module's contract.
///
/// Returns [`Determination::Undetermined`] when the meta sidecar is missing or
/// unparseable, or when the body cannot be read. It does **not** return an
/// empty index in those cases, because an empty index answers every query with
/// "no hits", which reads downstream as "that code does not exist".
///
/// A body line that fails to parse is skipped rather than failing the whole
/// load — but the skip is counted, and any skip makes the result
/// `Undetermined`, since a partially-loaded index silently under-reports./// Read only the sidecar meta — the fingerprint, and the counts describing the
/// body — without touching the body itself.
///
/// Freshness is a question about the meta alone, and the body is the expensive
/// part (megabytes of postings). Answering "is this index current?" by parsing
/// the whole thing made every query pay for the answer several times over.
/// `None` means the meta is absent or unparseable, which callers must treat as
/// not-fresh — never as fresh-by-default.
pub fn read_meta(meta_path: &Path) -> Option<TextIndexMeta> {
    let raw = std::fs::read_to_string(meta_path).ok()?;
    serde_json::from_str(&raw).ok()
}

pub fn load(body_path: &Path, meta_path: &Path) -> Determination<TextIndex> {
    let Ok(meta_raw) = std::fs::read_to_string(meta_path) else {
        return Determination::undetermined(format!(
            "text index meta unreadable at {}",
            meta_path.display()
        ));
    };
    let Ok(meta) = serde_json::from_str::<TextIndexMeta>(&meta_raw) else {
        return Determination::undetermined(format!(
            "text index meta unparseable at {}",
            meta_path.display()
        ));
    };
    let Ok(body) = std::fs::read_to_string(body_path) else {
        return Determination::undetermined(format!(
            "text index body unreadable at {}",
            body_path.display()
        ));
    };

    let mut tokens: BTreeMap<String, TokenPostings> = BTreeMap::new();
    let mut skipped = 0usize;
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(rec) = serde_json::from_str::<TokenRecord>(line) else {
            skipped += 1;
            continue;
        };
        tokens.insert(
            rec.t,
            TokenPostings {
                sites: rec
                    .p
                    .into_iter()
                    .map(|[file, line]| Posting { file, line })
                    .collect(),
                truncated: rec.x,
            },
        );
    }

    if skipped > 0 {
        return Determination::undetermined(format!(
            "text index body has {} unparseable record(s) at {} — a partial index \
             under-reports, so it is not usable as an authoritative answer",
            skipped,
            body_path.display()
        ));
    }

    Determination::known(TextIndex {
        files: meta.files,
        tokens,
        fingerprint: meta.fingerprint,
    })
}

/// One ranked line of a search result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextHit {
    /// Repo-relative source path.
    pub file: String,
    /// 1-indexed line number.
    pub line: u32,
    /// How many distinct query tokens occur on this line.
    pub score: i64,
}

/// The result of a search: the ranked hits plus whether the answer is complete.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchOutcome {
    /// Top-k lines, best first.
    pub hits: Vec<TextHit>,
    /// `true` when at least one query token's posting list had been capped at
    /// [`MAX_POSTINGS_PER_TOKEN`], so lines outside the retained prefix could
    /// not be considered. The hits shown are real; the ranking is not
    /// guaranteed global.
    pub truncated: bool,
    /// Query tokens that are not in the index at all. Distinct from "no hits":
    /// it says *which* term is responsible, so a caller can tell a typo from an
    /// absence.
    pub missing_tokens: Vec<String>,
}

impl TextIndex {
    /// Verify the index was built from the tree it is about to answer for.
    ///
    /// `current` is a freshly-computed [`crate::code_index::fingerprint`] over
    /// the same source set. A mismatch is [`Determination::Undetermined`]: the
    /// index may still be mostly right, but "mostly right and confidently
    /// presented" is precisely the failure this guards.
    pub fn check_fresh(&self, current: &str) -> Determination<()> {
        if self.fingerprint == current {
            Determination::known(())
        } else {
            Determination::undetermined(format!(
                "text index is stale: built from fingerprint {}, tree is now {} — \
                 rebuild before trusting a search over it",
                self.fingerprint, current
            ))
        }
    }

    /// Resolve an interned file id back to its path.
    fn path_of(&self, id: u32) -> Option<&str> {
        self.files
            .get(usize::try_from(id).ok()?)
            .map(String::as_str)
    }

    /// Deterministic top-k line search.
    ///
    /// Scores each line by how many **distinct** query tokens occur on it, then
    /// ranks by score descending with ties broken by file then line, so the
    /// output is stable across runs and independent of index iteration order.
    ///
    /// A zero-token query or `k == 0` yields no hits — and says so honestly via
    /// `missing_tokens`/an empty `hits`, rather than pretending to have
    /// searched.
    pub fn search(&self, query: &str, k: usize) -> SearchOutcome {
        let q: BTreeSet<String> = tokenize(query).into_iter().collect();
        let mut missing_tokens: Vec<String> = Vec::new();
        let mut truncated = false;

        if k == 0 || q.is_empty() {
            return SearchOutcome {
                hits: Vec::new(),
                truncated: false,
                missing_tokens: q.into_iter().collect(),
            };
        }

        // (file_id, line) -> count of distinct query tokens present.
        let mut tally: BTreeMap<Posting, i64> = BTreeMap::new();
        for token in &q {
            let Some(tp) = self.tokens.get(token) else {
                missing_tokens.push(token.clone());
                continue;
            };
            if tp.truncated {
                truncated = true;
            }
            for site in &tp.sites {
                *tally.entry(*site).or_insert(0) += 1;
            }
        }

        let mut hits: Vec<TextHit> = tally
            .into_iter()
            .filter_map(|(site, score)| {
                self.path_of(site.file).map(|path| TextHit {
                    file: path.to_string(),
                    line: site.line,
                    score,
                })
            })
            .collect();

        hits.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| a.file.cmp(&b.file))
                .then_with(|| a.line.cmp(&b.line))
        });
        hits.truncate(k);

        SearchOutcome {
            hits,
            truncated,
            missing_tokens,
        }
    }
}

/// Read exactly the lines named by `hits` from disk, under `root`.
///
/// This is the other half of the cost story: [`TextIndex::search`] narrows
/// 163KB of grep output to ten `(file, line)` pairs, and this turns those pairs
/// into ten lines of text — so the caller never reads a whole file. Fail-soft
/// per hit: a file that cannot be read yields `None` for that hit rather than
/// failing the batch (the hit's location is still useful on its own).
pub fn snippets(root: &Path, hits: &[TextHit]) -> Vec<Option<String>> {
    let mut cache: BTreeMap<String, Option<Vec<String>>> = BTreeMap::new();
    hits.iter()
        .map(|hit| {
            let lines = cache.entry(hit.file.clone()).or_insert_with(|| {
                std::fs::read_to_string(root.join(&hit.file))
                    .ok()
                    .map(|text| text.lines().map(str::to_string).collect())
            });
            let lines = lines.as_ref()?;
            let idx = usize::try_from(hit.line).ok()?.checked_sub(1)?;
            lines.get(idx).map(|l| l.trim().to_string())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src() -> Vec<(String, String)> {
        vec![
            (
                "a.rs".to_string(),
                "fn alpha() {\n    let cfg = toml::from_str(text);\n    beta();\n}\n".to_string(),
            ),
            (
                "b.rs".to_string(),
                "fn beta() {\n    alpha();\n}\n".to_string(),
            ),
        ]
    }

    #[test]
    fn build_is_deterministic_regardless_of_source_order() {
        let mut reversed = src();
        reversed.reverse();
        assert_eq!(build(&src(), "fp"), build(&reversed, "fp"));
    }

    #[test]
    fn search_ranks_lines_containing_more_query_tokens_first() {
        let idx = build(&src(), "fp");
        let out = idx.search("toml from_str", 10);
        let top = out.hits.first().expect("a hit");
        assert_eq!(top.file, "a.rs");
        assert_eq!(top.line, 2);
        // "toml", "from", "str" all occur on that line.
        assert_eq!(top.score, 3);
    }

    #[test]
    fn search_honours_k_and_never_returns_the_whole_corpus() {
        let idx = build(&src(), "fp");
        assert!(idx.search("alpha beta", 1).hits.len() <= 1);
        assert!(idx.search("alpha beta", 0).hits.is_empty());
    }

    #[test]
    fn a_token_absent_from_the_corpus_is_named_not_silently_dropped() {
        let idx = build(&src(), "fp");
        let out = idx.search("alpha nonexistenttoken", 10);
        assert_eq!(out.missing_tokens, vec!["nonexistenttoken".to_string()]);
        // Still a determined answer: the index is complete, so "not present"
        // is an observation, not a failure.
        assert!(!out.truncated);
    }

    #[test]
    fn capped_token_is_marked_truncated_and_the_flag_reaches_the_search() {
        // One token repeated on more lines than the cap allows.
        let body: String = (0..MAX_POSTINGS_PER_TOKEN + 50)
            .map(|_| "floodtoken\n")
            .collect();
        let idx = build(&[("big.rs".to_string(), body)], "fp");
        let tp = idx.tokens.get("floodtoken").expect("token present");
        assert!(tp.truncated, "posting list past the cap must be marked");
        assert_eq!(tp.sites.len(), MAX_POSTINGS_PER_TOKEN);
        assert!(
            idx.search("floodtoken", 5).truncated,
            "a search touching a capped token must report truncated"
        );
    }

    #[test]
    fn roundtrip_through_disk_preserves_the_index() {
        let dir = tempfile::tempdir().unwrap();
        let body = dir.path().join("t.jsonl");
        let meta = dir.path().join("t.meta.json");
        let idx = build(&src(), "fp-1");
        write(&body, &meta, &idx);
        match load(&body, &meta) {
            Determination::Known(loaded) => assert_eq!(loaded, idx),
            Determination::Undetermined(_) => panic!("clean roundtrip must be Known"),
        }
    }

    #[test]
    fn missing_index_is_undetermined_not_an_empty_index() {
        let dir = tempfile::tempdir().unwrap();
        let got = load(
            &dir.path().join("nope.jsonl"),
            &dir.path().join("nope.meta.json"),
        );
        assert!(
            matches!(got, Determination::Undetermined(_)),
            "an absent index must not answer 'no hits' for every query"
        );
    }

    #[test]
    fn corrupt_body_record_is_undetermined_not_a_partial_answer() {
        let dir = tempfile::tempdir().unwrap();
        let body = dir.path().join("t.jsonl");
        let meta = dir.path().join("t.meta.json");
        write(&body, &meta, &build(&src(), "fp-1"));
        let mut text = std::fs::read_to_string(&body).unwrap();
        text.push_str("{ this is not json\n");
        std::fs::write(&body, text).unwrap();
        assert!(
            matches!(load(&body, &meta), Determination::Undetermined(_)),
            "a partially-parseable index under-reports and must not be Known"
        );
    }

    #[test]
    fn corrupt_meta_is_undetermined() {
        let dir = tempfile::tempdir().unwrap();
        let body = dir.path().join("t.jsonl");
        let meta = dir.path().join("t.meta.json");
        write(&body, &meta, &build(&src(), "fp-1"));
        std::fs::write(&meta, "{ nope").unwrap();
        assert!(matches!(load(&body, &meta), Determination::Undetermined(_)));
    }

    #[test]
    fn stale_index_is_undetermined_even_though_it_is_readable() {
        let idx = build(&src(), "fingerprint-at-build-time");
        assert!(matches!(
            idx.check_fresh("fingerprint-now-that-the-tree-moved"),
            Determination::Undetermined(_)
        ));
        assert!(matches!(
            idx.check_fresh("fingerprint-at-build-time"),
            Determination::Known(())
        ));
    }

    #[test]
    fn empty_hits_on_a_fresh_complete_index_is_a_real_observation() {
        // Anti-vacuity for the fail-closed tests above: they must not have been
        // satisfied by making *everything* Undetermined.
        let idx = build(&src(), "fp");
        let out = idx.search("zzzznotpresent", 10);
        assert!(out.hits.is_empty());
        assert!(!out.truncated);
        assert!(matches!(idx.check_fresh("fp"), Determination::Known(())));
    }

    #[test]
    fn snippets_read_only_the_named_lines() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "one\ntwo\nthree\n").unwrap();
        let hits = vec![TextHit {
            file: "a.rs".to_string(),
            line: 2,
            score: 1,
        }];
        assert_eq!(snippets(dir.path(), &hits), vec![Some("two".to_string())]);
    }

    #[test]
    fn snippets_fail_soft_per_hit_on_an_unreadable_file() {
        let dir = tempfile::tempdir().unwrap();
        let hits = vec![TextHit {
            file: "absent.rs".to_string(),
            line: 1,
            score: 1,
        }];
        assert_eq!(snippets(dir.path(), &hits), vec![None]);
    }

    #[test]
    fn never_panics_on_pathological_input() {
        let weird = "\u{1F389} fn \u{4F60}\u{597D}(( {{ [[[\n\0\n";
        let idx = build(&[("w.rs".to_string(), weird.to_string())], "fp");
        let _ = idx.search(weird, 5);
        let _ = idx.search("", 5);
        let _ = build(&[], "fp").search("anything", 5);
    }
}
