//! One in-process façade over the three deterministic code indexes —
//! [`crate::code_index`] (symbols), [`crate::text_index`] (full text), and
//! [`crate::callgraph`] (caller→callee edges).
//!
//! # Why a façade
//!
//! Before this module, `specguard` reached the symbol index by **spawning
//! `fugu-router` twice per query** (`code-index build --if-stale`, then
//! `code-index search`) and parsing its JSON back. Two process spawns and a
//! serialize/parse round trip, per shard, to read a file that is sitting on
//! disk in the same repository. This module does the same work with a function
//! call. The subprocess path was not wrong — it was how one crate borrowed
//! another binary's feature — it was just the most expensive way to do it.
//!
//! All three indexes share one directory and one staleness
//! [`crate::code_index::fingerprint`], so they cannot drift apart: a rebuild
//! rebuilds all three or none.
//!
//! # Cannot-determine is not "no results" (CLAUDE.md §3)
//!
//! Every query function returns [`Determination`]. `git ls-files` failing, an
//! index that will not load, and an index that is **stale after a rebuild was
//! attempted** all resolve to `Undetermined`. None of them resolve to an empty
//! result set, because "I found nothing" and "I could not look" are the two
//! answers a search tool must never conflate — the caller would read both as
//! "that code is not there".

use std::path::{Path, PathBuf};

use crate::callgraph::{self, Edge};
use crate::code_index::{self, Scored, Symbol};
use crate::text_index::{self, SearchOutcome, TextIndex};
use crate::verdict::Determination;

/// Directory the indexes live in, relative to the repo root.
///
/// `.fugu` rather than a new directory: the symbol index has been written there
/// since code-RAG slice-1 and `fugu-router code-index` still reads it. Pointing
/// this façade at the same file means there is exactly **one** symbol index on
/// disk, not one per consumer.
pub const INDEX_DIR: &str = ".fugu";

/// Default result count for [`search_text`], re-exported so callers do not
/// reach past this façade into the index modules for a constant.
pub use crate::text_index::DEFAULT_K as DEFAULT_TEXT_K;

/// Resolved on-disk locations for one repo's indexes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexPaths {
    pub root: PathBuf,
    pub symbols_body: PathBuf,
    pub symbols_meta: PathBuf,
    pub text_body: PathBuf,
    pub text_meta: PathBuf,
    pub graph: PathBuf,
}

impl IndexPaths {
    /// Locations under `root/.fugu`.
    pub fn new(root: &Path) -> Self {
        let dir = root.join(INDEX_DIR);
        Self {
            root: root.to_path_buf(),
            symbols_body: dir.join("code-index.jsonl"),
            symbols_meta: dir.join("code-index.meta.json"),
            text_body: dir.join("text-index.jsonl"),
            text_meta: dir.join("text-index.meta.json"),
            graph: dir.join("callgraph.jsonl"),
        }
    }
}

/// Git-tracked `.rs` paths under `root`, repo-relative and sorted.
///
/// Fail-closed: a missing `git`, a non-zero exit, or unusable output is
/// `Undetermined`. Silently treating "git did not answer" as "this repo has no
/// Rust files" would build an empty index that then reports every query as a
/// miss.
pub fn rs_files(root: &Path) -> Determination<Vec<String>> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("ls-files")
        .output();
    let Ok(out) = out else {
        return Determination::undetermined(format!(
            "git ls-files could not be run under {}",
            root.display()
        ));
    };
    if !out.status.success() {
        return Determination::undetermined(format!(
            "git ls-files failed under {}: {}",
            root.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let mut files: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && l.ends_with(".rs"))
        .map(str::to_string)
        .collect();
    files.sort();
    Determination::known(files)
}

/// Cheap staleness fingerprint over the current `.rs` set — `(path, size,
/// mtime)` only, contents never read.
pub fn current_fingerprint(root: &Path, rel_files: &[String]) -> String {
    use std::time::UNIX_EPOCH;
    let entries: Vec<(String, u64, i64)> = rel_files
        .iter()
        .map(|rel| {
            let (size, mtime) = match std::fs::metadata(root.join(rel)) {
                Ok(md) => {
                    let mtime = md
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                        .map(|d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
                        .unwrap_or(0);
                    (md.len(), mtime)
                }
                Err(_) => (0, 0),
            };
            (rel.clone(), size, mtime)
        })
        .collect();
    code_index::fingerprint(&entries)
}

/// What a build produced.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BuildStats {
    pub files: usize,
    pub symbols: usize,
    pub tokens: usize,
    pub postings: usize,
    pub truncated_tokens: usize,
    pub edges: usize,
    /// `false` when the fingerprint matched and everything was already present.
    pub rebuilt: bool,
}

/// Read every file in `rel_files`, skipping ones that cannot be read.
///
/// A file that will not read is dropped from the corpus rather than failing the
/// build — but the count of drops is returned so the caller can decide what an
/// incomplete corpus means. (`build` treats any drop as a reason to keep going,
/// since the fingerprint already recorded the file as present-but-unreadable;
/// what must never happen is the *query* side pretending it saw everything.)
fn read_sources(root: &Path, rel_files: &[String]) -> (Vec<(String, String)>, usize) {
    let mut sources = Vec::with_capacity(rel_files.len());
    let mut unreadable = 0usize;
    for rel in rel_files {
        match std::fs::read_to_string(root.join(rel)) {
            Ok(text) => sources.push((rel.clone(), text)),
            Err(_) => unreadable += 1,
        }
    }
    (sources, unreadable)
}

/// Build all three indexes when the tree's fingerprint differs from the
/// recorded one (or when any index file is missing); otherwise do nothing.
///
/// Fail-closed on the inputs it needs: if the `.rs` set cannot be enumerated,
/// nothing is written and the result is `Undetermined`.
pub fn build_if_stale(root: &Path) -> Determination<BuildStats> {
    build_inner(root, false)
}

/// Rebuild all three indexes unconditionally.
///
/// The repair path for an index whose meta says "current" while its body is
/// damaged — the one case [`build_if_stale`]'s cheap freshness check cannot
/// see. Queries call this at most once, after a load has already failed, so a
/// corrupt index costs one rebuild rather than one silent wrong answer.
pub fn rebuild(root: &Path) -> Determination<BuildStats> {
    build_inner(root, true)
}

fn build_inner(root: &Path, force: bool) -> Determination<BuildStats> {
    let paths = IndexPaths::new(root);
    let rel_files = match rs_files(root) {
        Determination::Known(f) => f,
        Determination::Undetermined(u) => {
            return Determination::undetermined(format!(
                "cannot enumerate sources, so no index was built: {}",
                u.reason().as_str()
            ))
        }
    };
    let fp = current_fingerprint(root, &rel_files);

    // Every index must agree with the tree's current fingerprint. Checking only
    // the symbol index's would have been enough to look correct while missing
    // the case this repo actually produces: `fugu-router code-index build`
    // writes the symbol index alone, leaving a current symbol index next to a
    // stale text index and graph.
    //
    // Judged from the META SIDECARS only. Parsing the bodies to answer "is this
    // current?" cost more than the query it was guarding — a single search
    // decoded ~17MB of postings three times over. A body that is present but
    // damaged is not caught here; it is caught where it matters, at load time,
    // and `rebuild` exists so a caller can repair it in one retry rather than
    // paying for the check on every healthy query.
    let fresh = code_index::read_meta(&paths.symbols_meta)
        .map(|m| m.fingerprint == fp)
        .unwrap_or(false)
        && paths.symbols_body.exists()
        && text_index::read_meta(&paths.text_meta)
            .map(|m| m.fingerprint == fp)
            .unwrap_or(false)
        && paths.text_body.exists()
        && paths.graph.exists();

    if fresh && !force {
        return Determination::known(BuildStats {
            files: rel_files.len(),
            rebuilt: false,
            ..Default::default()
        });
    }

    let (sources, _unreadable) = read_sources(root, &rel_files);

    let mut symbols: Vec<Symbol> = Vec::new();
    for (path, contents) in &sources {
        symbols.extend(code_index::extract_symbols(contents, path));
    }
    code_index::write_index(&paths.symbols_body, &symbols);
    code_index::write_meta(
        &paths.symbols_meta,
        &code_index::IndexMeta {
            fingerprint: fp.clone(),
            files: rel_files.len(),
            symbols: symbols.len(),
        },
    );

    let text = text_index::build(&sources, &fp);
    text_index::write(&paths.text_body, &paths.text_meta, &text);

    let edges = callgraph::build_graph(&sources);
    callgraph::write_graph(&paths.graph, &edges);

    let postings: usize = text.tokens.values().map(|t| t.sites.len()).sum();
    let truncated_tokens = text.tokens.values().filter(|t| t.truncated).count();

    Determination::known(BuildStats {
        files: rel_files.len(),
        symbols: symbols.len(),
        tokens: text.tokens.len(),
        postings,
        truncated_tokens,
        edges: edges.len(),
        rebuilt: true,
    })
}

/// Load the text index and verify it describes the tree as it is now.
///
/// This runs *after* [`build_if_stale`] has already tried to make the index
/// current, and it is not redundant: index writes are deliberately fail-soft,
/// so a rebuild can leave the previous, stale index in place without saying so
/// (an unwritable `.fugu`, a read-only index file). Re-checking the fingerprint
/// here is what turns that silent failure into `Undetermined` instead of a
/// confident answer about a tree that has moved on.
fn fresh_text_index(root: &Path) -> Determination<TextIndex> {
    let paths = IndexPaths::new(root);
    let index = match text_index::load(&paths.text_body, &paths.text_meta) {
        Determination::Known(i) => i,
        Determination::Undetermined(u) => {
            return Determination::undetermined(u.reason().as_str().to_string())
        }
    };
    let rel_files = match rs_files(root) {
        Determination::Known(f) => f,
        Determination::Undetermined(u) => {
            return Determination::undetermined(u.reason().as_str().to_string())
        }
    };
    match index.check_fresh(&current_fingerprint(root, &rel_files)) {
        Determination::Known(()) => Determination::known(index),
        Determination::Undetermined(u) => {
            Determination::undetermined(u.reason().as_str().to_string())
        }
    }
}

/// Run `f` against the indexes, rebuilding first if the tree moved, and
/// repairing once if the indexes turn out to be damaged.
///
/// The retry is bounded at exactly one rebuild. A second failure is reported as
/// `Undetermined` rather than retried again — an index that will not build is a
/// condition to surface, not to spin on.
fn with_indexes<T>(root: &Path, f: impl Fn(&Path) -> Determination<T>) -> Determination<T> {
    if let Determination::Undetermined(u) = build_if_stale(root) {
        return Determination::undetermined(u.reason().as_str().to_string());
    }
    match f(root) {
        Determination::Known(v) => Determination::known(v),
        Determination::Undetermined(first) => {
            // The meta claimed current but something would not load: damaged
            // body, partial write, a file removed under us. Repair once.
            if let Determination::Undetermined(u) = rebuild(root) {
                return Determination::undetermined(format!(
                    "{} (rebuild also failed: {})",
                    first.as_str(),
                    u.as_str()
                ));
            }
            f(root)
        }
    }
}

/// Top-k full-text line search, rebuilding first if the tree moved.
pub fn search_text(root: &Path, query: &str, k: usize) -> Determination<SearchOutcome> {
    with_indexes(root, |r| match fresh_text_index(r) {
        Determination::Known(index) => Determination::known(index.search(query, k)),
        Determination::Undetermined(u) => {
            Determination::undetermined(u.reason().as_str().to_string())
        }
    })
}

/// Top-k symbol search — the in-process replacement for shelling out to
/// `fugu-router code-index search`.
pub fn search_symbols(root: &Path, query: &str, k: usize) -> Determination<Vec<Scored>> {
    with_indexes(root, |r| match fresh_symbols(r) {
        Determination::Known(symbols) => {
            Determination::known(code_index::search(&symbols, query, k))
        }
        Determination::Undetermined(u) => {
            Determination::undetermined(u.reason().as_str().to_string())
        }
    })
}

/// Load the symbol index and verify it describes the tree as it is now — the
/// same last-line staleness guard as [`fresh_text_index`], for the same reason.
fn fresh_symbols(root: &Path) -> Determination<Vec<Symbol>> {
    let paths = IndexPaths::new(root);
    let rel_files = match rs_files(root) {
        Determination::Known(f) => f,
        Determination::Undetermined(u) => {
            return Determination::undetermined(u.reason().as_str().to_string())
        }
    };
    let fp = current_fingerprint(root, &rel_files);
    match code_index::read_meta(&paths.symbols_meta) {
        Some(m) if m.fingerprint == fp => {}
        Some(m) => {
            return Determination::undetermined(format!(
                "symbol index is stale: built from {}, tree is now {}",
                m.fingerprint, fp
            ))
        }
        None => {
            return Determination::undetermined(format!(
                "symbol index meta unreadable at {}",
                paths.symbols_meta.display()
            ))
        }
    }
    if !paths.symbols_body.exists() {
        return Determination::undetermined(format!(
            "symbol index missing at {}",
            paths.symbols_body.display()
        ));
    }
    Determination::known(code_index::load_index(&paths.symbols_body))
}

/// Every symbol in the current index.
///
/// For callers that need the whole corpus rather than a top-k answer — bulk
/// enrichment, cross-referencing — where issuing one [`search_symbols`] per
/// name would reload and rescore the index every time.
pub fn all_symbols(root: &Path) -> Determination<Vec<Symbol>> {
    with_indexes(root, fresh_symbols)
}

/// Every edge in the current call graph, for the same bulk reason as
/// [`all_symbols`].
pub fn all_edges(root: &Path) -> Determination<Vec<Edge>> {
    with_indexes(root, |r| callgraph::load_graph(&IndexPaths::new(r).graph))
}

/// Who calls `name`, read from the persisted graph — no corpus rescan.
pub fn callers(root: &Path, name: &str) -> Determination<Vec<Edge>> {
    with_indexes(root, |r| {
        match callgraph::load_graph(&IndexPaths::new(r).graph) {
            Determination::Known(edges) => Determination::known(
                callgraph::callers_of(&edges, name)
                    .into_iter()
                    .cloned()
                    .collect(),
            ),
            Determination::Undetermined(u) => {
                Determination::undetermined(u.reason().as_str().to_string())
            }
        }
    })
}

/// What `name` calls, read from the persisted graph.
pub fn callees(root: &Path, name: &str) -> Determination<Vec<Edge>> {
    with_indexes(root, |r| {
        match callgraph::load_graph(&IndexPaths::new(r).graph) {
            Determination::Known(edges) => Determination::known(
                callgraph::callees_of(&edges, name)
                    .into_iter()
                    .cloned()
                    .collect(),
            ),
            Determination::Undetermined(u) => {
                Determination::undetermined(u.reason().as_str().to_string())
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway git repo with two Rust files. `git ls-files` only reports
    /// tracked paths, so the files must actually be added.
    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/alpha.rs"),
            "pub fn alpha_entry() {\n    let cfg = load_settings();\n    beta_helper(cfg);\n}\n",
        )
        .unwrap();
        std::fs::write(
            root.join("src/beta.rs"),
            "pub fn beta_helper(x: u32) {}\npub fn load_settings() {}\n",
        )
        .unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["add", "-A"],
            vec![
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-qm",
                "x",
            ],
        ] {
            let ok = std::process::Command::new("git")
                .arg("-C")
                .arg(root)
                .args(&args)
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok, "git {:?} failed", args);
        }
        dir
    }

    fn known<T>(d: Determination<T>) -> T {
        match d {
            Determination::Known(v) => v,
            Determination::Undetermined(u) => panic!("expected Known, got: {}", u.as_str()),
        }
    }

    #[test]
    fn full_text_search_finds_a_token_that_lives_only_in_a_body() {
        // `load_settings` is *called* on alpha.rs:2, a line the symbol index
        // never sees because it is not a declaration. This is the whole reason
        // the text index exists alongside code_index.
        let dir = repo();
        let out = known(search_text(dir.path(), "load_settings", DEFAULT_TEXT_K));
        assert!(
            out.hits
                .iter()
                .any(|h| h.file == "src/alpha.rs" && h.line == 2),
            "call site not found; hits: {:?}",
            out.hits
        );
        assert!(!out.truncated);
        assert!(out.missing_tokens.is_empty());
    }

    #[test]
    fn symbol_search_returns_declarations_without_spawning_fugu_router() {
        let dir = repo();
        let hits = known(search_symbols(dir.path(), "beta_helper", 10));
        assert!(
            hits.iter().any(|h| h.symbol.name == "beta_helper"),
            "got {:?}",
            hits.iter().map(|h| &h.symbol.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn callers_are_read_from_the_persisted_graph() {
        let dir = repo();
        let edges = known(callers(dir.path(), "beta_helper"));
        assert!(
            edges
                .iter()
                .any(|e| e.caller == "alpha_entry" && e.file == "src/alpha.rs"),
            "got {:?}",
            edges
        );
        let out = known(callees(dir.path(), "alpha_entry"));
        let names: Vec<&str> = out.iter().map(|e| e.callee.as_str()).collect();
        assert!(names.contains(&"load_settings"), "got {:?}", names);
    }

    #[test]
    fn a_second_build_on_an_unchanged_tree_does_no_work() {
        let dir = repo();
        let first = known(build_if_stale(dir.path()));
        assert!(first.rebuilt);
        assert!(first.files >= 2 && first.tokens > 0 && first.edges > 0);
        let second = known(build_if_stale(dir.path()));
        assert!(!second.rebuilt, "an unchanged tree must not be re-indexed");
    }

    #[test]
    fn an_edit_is_visible_to_the_next_query() {
        // The staleness contract that makes an index safe to trust: a fast
        // answer from a tree that has moved on is worse than no answer.
        let dir = repo();
        assert!(known(search_text(dir.path(), "zzz_new_token", 5))
            .hits
            .is_empty());
        std::fs::write(
            dir.path().join("src/beta.rs"),
            "pub fn beta_helper(x: u32) {}\npub fn load_settings() {}\nfn later() { zzz_new_token(); }\n",
        )
        .unwrap();
        let out = known(search_text(dir.path(), "zzz_new_token", 5));
        assert!(
            out.hits.iter().any(|h| h.file == "src/beta.rs"),
            "edit not picked up: {:?}",
            out.hits
        );
    }

    #[test]
    fn a_directory_git_cannot_answer_for_is_undetermined_not_empty() {
        // CLAUDE.md 3: "could not look" must never be delivered as "found
        // nothing", which every caller would read as "that code is not there".
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("no-repo-here");
        std::fs::create_dir_all(&outside).unwrap();
        for label in ["rs_files", "build", "text", "symbols", "callers"] {
            let undetermined = match label {
                "rs_files" => matches!(rs_files(&outside), Determination::Undetermined(_)),
                "build" => matches!(build_if_stale(&outside), Determination::Undetermined(_)),
                "text" => matches!(
                    search_text(&outside, "anything", 5),
                    Determination::Undetermined(_)
                ),
                "symbols" => matches!(
                    search_symbols(&outside, "anything", 5),
                    Determination::Undetermined(_)
                ),
                _ => matches!(
                    callers(&outside, "anything"),
                    Determination::Undetermined(_)
                ),
            };
            assert!(
                undetermined,
                "{} answered without being able to look",
                label
            );
        }
    }

    #[test]
    fn an_index_that_cannot_be_written_is_undetermined_not_zero_hits() {
        // `.fugu` occupied by a regular file: every write fails (writes are
        // fail-soft by design) and the load that follows finds nothing. The
        // query must surface that, not report an empty corpus.
        let dir = repo();
        std::fs::write(dir.path().join(INDEX_DIR), "not a directory").unwrap();
        assert!(
            matches!(
                search_text(dir.path(), "load_settings", 5),
                Determination::Undetermined(_)
            ),
            "an unwritable index must not answer queries"
        );
        assert!(matches!(
            callers(dir.path(), "beta_helper"),
            Determination::Undetermined(_)
        ));
    }

    #[test]
    fn a_genuine_miss_on_a_healthy_index_is_known_and_empty() {
        // Anti-vacuity: the tests above must not have been satisfied by making
        // every path Undetermined. A real search for a real absence is Known.
        let dir = repo();
        // `tokenize` splits snake_case, so this reports the absent *tokens*,
        // which is what tells a caller "the index looked and they are not here".
        let out = known(search_text(dir.path(), "definitely_absent_zqxj", 5));
        assert!(out.hits.is_empty(), "got {:?}", out.hits);
        assert_eq!(out.missing_tokens, vec!["absent", "definitely", "zqxj"]);
        let none = known(callers(dir.path(), "no_such_symbol"));
        assert!(none.is_empty());
    }

    #[test]
    fn all_three_indexes_share_one_directory_and_cannot_drift_apart() {
        let dir = repo();
        let _ = known(build_if_stale(dir.path()));
        let paths = IndexPaths::new(dir.path());
        for p in [
            &paths.symbols_body,
            &paths.symbols_meta,
            &paths.text_body,
            &paths.text_meta,
            &paths.graph,
        ] {
            assert!(p.exists(), "{} was not written", p.display());
            assert_eq!(p.parent().unwrap(), dir.path().join(INDEX_DIR));
        }
    }

    #[test]
    fn a_stale_index_a_rebuild_could_not_repair_is_undetermined() {
        // Index writes are fail-soft, so a rebuild can silently leave the old
        // index in place. Read-only index files reproduce that exactly: the
        // tree moves, the rebuild runs, nothing lands, and the file on disk
        // still describes the previous tree. Answering from it would be the
        // worst outcome available -- fast and wrong.
        let dir = repo();
        let _ = known(build_if_stale(dir.path()));
        let paths = IndexPaths::new(dir.path());
        std::fs::write(
            dir.path().join("src/beta.rs"),
            "pub fn beta_helper(x: u32) {}\npub fn load_settings() {}\nfn later() {}\n",
        )
        .unwrap();
        for f in [&paths.text_body, &paths.text_meta] {
            let mut perm = std::fs::metadata(f).unwrap().permissions();
            perm.set_readonly(true);
            std::fs::set_permissions(f, perm).unwrap();
        }
        let out = search_text(dir.path(), "load_settings", 5);
        // Restore permissions before asserting so the tempdir can be cleaned up.
        for f in [&paths.text_body, &paths.text_meta] {
            let mut perm = std::fs::metadata(f).unwrap().permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            perm.set_readonly(false);
            std::fs::set_permissions(f, perm).unwrap();
        }
        assert!(
            matches!(out, Determination::Undetermined(_)),
            "a stale index that could not be repaired must not answer"
        );
    }

    #[test]
    fn a_symbol_index_refreshed_alone_does_not_leave_the_others_stale() {
        // `fugu-router code-index build` writes only the symbol index. If
        // freshness were judged from that file alone, the text index and graph
        // would answer from the previous tree forever.
        let dir = repo();
        let _ = known(build_if_stale(dir.path()));
        std::fs::write(
            dir.path().join("src/beta.rs"),
            "pub fn beta_helper(x: u32) {}\npub fn load_settings() {}\nfn later() { qqq_marker(); }\n",
        )
        .unwrap();
        // Simulate the symbol-index-only refresh.
        let paths = IndexPaths::new(dir.path());
        let files = known(rs_files(dir.path()));
        let fp = current_fingerprint(dir.path(), &files);
        code_index::write_meta(
            &paths.symbols_meta,
            &code_index::IndexMeta {
                fingerprint: fp,
                files: files.len(),
                symbols: 0,
            },
        );
        let out = known(search_text(dir.path(), "qqq_marker", 5));
        assert!(
            out.hits.iter().any(|h| h.file == "src/beta.rs"),
            "text index stayed stale behind the symbol index: {:?}",
            out.hits
        );
    }

    #[test]
    fn a_damaged_body_behind_a_current_meta_is_repaired_in_one_retry() {
        // Freshness is judged from the meta sidecars, so a body corrupted in
        // place is invisible to that check. The load that follows catches it,
        // and the bounded repair turns what would be a permanently stuck index
        // into one wasted rebuild.
        let dir = repo();
        let _ = known(build_if_stale(dir.path()));
        let paths = IndexPaths::new(dir.path());
        std::fs::write(&paths.text_body, "{ not a record at all\n").unwrap();
        let out = known(search_text(dir.path(), "load_settings", 5));
        assert!(
            out.hits.iter().any(|h| h.file == "src/alpha.rs"),
            "a damaged index was not repaired: {:?}",
            out.hits
        );
    }

    #[test]
    fn freshness_does_not_parse_the_index_bodies() {
        // The performance claim, pinned as behavior: a repeat build reads the
        // meta sidecars only. Bodies replaced with garbage would make any
        // body-parsing freshness check rebuild; this one must still say fresh.
        let dir = repo();
        let _ = known(build_if_stale(dir.path()));
        let paths = IndexPaths::new(dir.path());
        std::fs::write(&paths.text_body, "garbage\n").unwrap();
        std::fs::write(&paths.graph, "garbage\n").unwrap();
        let again = known(build_if_stale(dir.path()));
        assert!(
            !again.rebuilt,
            "freshness read the bodies; it must read only the meta"
        );
    }

    #[test]
    fn the_fingerprint_moves_only_when_the_tree_moves() {
        let dir = repo();
        let files = known(rs_files(dir.path()));
        let a = current_fingerprint(dir.path(), &files);
        assert_eq!(a, current_fingerprint(dir.path(), &files));
        std::fs::write(dir.path().join("src/beta.rs"), "pub fn beta_helper() {}\n").unwrap();
        assert_ne!(a, current_fingerprint(dir.path(), &files));
    }
}
