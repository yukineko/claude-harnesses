//! Deterministic code-symbol index — a purely-additive, subscription-native
//! (no embeddings, no external API) lexical index over Rust source, in the
//! same family as [`crate::lessons`] and [`crate::retrieval`]:
//!
//!   * `extract_symbols` is a **pure, deterministic** line-scanner: same
//!     `(contents, file)` in → identical `Vec<Symbol>` out, every call. It
//!     never panics on any input (fail-soft floor for pathological/garbled
//!     source).
//!   * `write_index`/`load_index` use the lessons/retrieval JSONL shape (one
//!     JSON object per line) but are **fail-loud**, unlike those stores:
//!     `write_index`/`write_meta` return the IO error instead of dropping it,
//!     and `load_index` is three-way — [`IndexLoadError::Absent`] (no file),
//!     [`IndexLoadError::Unreadable`] (any other IO error, invalid UTF-8, or a
//!     non-blank line that is not a symbol record), or the full symbol list.
//!     A partially corrupt index is never served as a smaller valid one, and a
//!     failed write is never reported as a build. [`load_index_checked`] adds
//!     the build-meta symbol-count cross-check that catches a body truncated at
//!     a line boundary.
//!   * `search` is a deterministic **lexical** token-overlap ranking over the
//!     symbol's name/kind/signature — the same lexical-only family as
//!     `lessons::search` (Jaccard there; a simpler overlap count here, since
//!     code identifiers are short and Jaccard's union-normalisation would
//!     over-penalise a long `signature` string against a two-word query).
//!
//! This is code-RAG **slice-1**: symbol/lexical only. No parser dependency —
//! a small, well-commented string-based scanner, matching harness-core's
//! existing "no heavy deps" convention (this module adds none).

use std::collections::hash_map::DefaultHasher;
use std::collections::BTreeSet;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One extracted Rust declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Symbol {
    pub name: String,
    /// One of: fn, struct, enum, trait, impl, mod, const, static, type, macro.
    pub kind: String,
    pub file: String,
    /// 1-indexed line number the declaration starts on.
    pub line: usize,
    /// The trimmed source line the declaration was found on (single line;
    /// multi-line signatures are not joined — kept deterministic and simple).
    pub signature: String,
}

/// Strip a token down to its leading identifier characters (alphanumeric or
/// `_`), discarding whatever punctuation follows (`(`, `<`, `:`, `{`, `;`,
/// `=`, ...). Used everywhere a declaration's name is read off a token that
/// may have trailing syntax glued to it with no whitespace (e.g. `foo()`,
/// `Foo<T>`, `FOO:`).
fn clean_ident(tok: &str) -> String {
    tok.chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect()
}

/// Recognise a top-level-ish Rust declaration on one already-trimmed,
/// non-empty, non-comment source line. Returns `(kind, name)` on a match.
///
/// Deterministic string-token scanning only (no regex/parser dependency):
/// split on whitespace, strip an optional leading `pub`/`pub(...)` and any
/// `async`/`unsafe`/`default`/`mut` qualifiers (plus `const` when it's a
/// `const fn` qualifier rather than the item keyword itself), then match the
/// remaining leading keyword. Kept intentionally simple — this is a lexical
/// scanner, not a full parser, so unusual formatting (e.g. a keyword split
/// across lines) is not recognised; that's an acceptable miss for slice-1.
fn parse_declaration(line: &str) -> Option<(&'static str, String)> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    if tokens.is_empty() {
        return None;
    }

    let mut i = 0;
    if tokens[i] == "pub" || tokens[i].starts_with("pub(") {
        i += 1;
    }

    loop {
        if i >= tokens.len() {
            return None;
        }
        match tokens[i] {
            "async" | "unsafe" | "default" | "mut" => i += 1,
            // `const fn` — `const` here is a qualifier on the fn, not the
            // `const ITEM: T = ...` keyword itself.
            "const" if tokens.get(i + 1) == Some(&"fn") => i += 1,
            _ => break,
        }
    }

    if i >= tokens.len() {
        return None;
    }

    let kind: &'static str = if tokens[i] == "fn" {
        "fn"
    } else if tokens[i] == "struct" {
        "struct"
    } else if tokens[i] == "enum" {
        "enum"
    } else if tokens[i] == "trait" {
        "trait"
    } else if tokens[i] == "impl" || tokens[i].starts_with("impl<") {
        "impl"
    } else if tokens[i] == "mod" {
        "mod"
    } else if tokens[i] == "const" {
        "const"
    } else if tokens[i] == "static" {
        "static"
    } else if tokens[i] == "type" {
        "type"
    } else if tokens[i] == "macro_rules!" {
        "macro"
    } else {
        return None;
    };

    let rest = &tokens[i + 1..];

    let name = if kind == "impl" {
        // `impl Foo` -> Foo; `impl Trait for Foo` -> Foo (the type being
        // impl'd, not the trait) — find `for` and take the token after it,
        // else the first remaining token.
        match rest.iter().position(|t| *t == "for") {
            Some(for_idx) => rest.get(for_idx + 1).map(|t| clean_ident(t)),
            None => rest.first().map(|t| clean_ident(t)),
        }
    } else {
        // `static mut FOO` — the `mut` follows the item keyword (`static`),
        // so it lands in `rest`; skip it to read the real name. No other
        // kind can legitimately have `mut` as its first post-keyword token
        // (`mut` is reserved), so this skip is safe for all of them.
        let start = usize::from(rest.first() == Some(&"mut"));
        rest.get(start).map(|t| clean_ident(t))
    }?;

    if name.is_empty() {
        return None;
    }

    Some((kind, name))
}

/// Extract every recognised declaration from `contents` (the source text of
/// `file`). Pure and deterministic: identical `(contents, file)` always
/// yields an identical `Vec<Symbol>`. Never panics — comments (`//...`),
/// blank lines, and any line that doesn't match a recognised declaration are
/// silently skipped.
pub fn extract_symbols(contents: &str, file: &str) -> Vec<Symbol> {
    let mut symbols = Vec::new();
    for (idx, raw_line) in contents.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        if let Some((kind, name)) = parse_declaration(line) {
            symbols.push(Symbol {
                name,
                kind: kind.to_string(),
                file: file.to_string(),
                line: idx + 1,
                signature: line.to_string(),
            });
        }
    }
    symbols
}

/// Write `symbols` to `path` as one JSON object per line, overwriting any
/// existing file (a full rebuild, not an append — matches how a code index is
/// regenerated wholesale on each `build`). Creates the parent dir if needed.
///
/// Fail-loud: any create/serialize/write/flush error is returned. A caller that
/// reports a build must not report one this function could not write.
pub fn write_index(path: &Path, symbols: &[Symbol]) -> std::io::Result<()> {
    use std::io::Write;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::io::BufWriter::new(std::fs::File::create(path)?);
    for s in symbols {
        let json = serde_json::to_string(s).map_err(std::io::Error::other)?;
        writeln!(file, "{json}")?;
    }
    file.flush()
}

/// Why a symbol index could not be loaded. Two distinct answers, neither of
/// which is "zero symbols".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexLoadError {
    /// No index file at the path: the index was never built.
    Absent(PathBuf),
    /// The index exists but cannot be trusted: an IO error other than
    /// not-found, bytes that are not UTF-8, a non-blank line that is not a
    /// symbol record, or (from [`load_index_checked`]) a body that disagrees
    /// with its build meta. Carries the detail.
    Unreadable(String),
}

impl std::fmt::Display for IndexLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IndexLoadError::Absent(p) => write!(f, "no symbol index at {}", p.display()),
            IndexLoadError::Unreadable(why) => write!(f, "symbol index unreadable: {why}"),
        }
    }
}

/// Load a symbol index body from `path`, strictly.
///
/// * file not found → [`IndexLoadError::Absent`];
/// * any other IO error (including invalid UTF-8 and a directory at `path`)
///   → [`IndexLoadError::Unreadable`];
/// * any non-blank line that does not parse as a [`Symbol`] →
///   [`IndexLoadError::Unreadable`] naming the line — a partially corrupt index
///   would otherwise be served as a smaller *valid* one, turning a lost symbol
///   into a confident "not there";
/// * otherwise every symbol, in file order. Blank lines are skipped. An empty
///   body loads as `Ok(vec![])`; use [`load_index_checked`] when an empty body
///   must be vouched for by the build meta.
///
/// Never panics.
pub fn load_index(path: &Path) -> Result<Vec<Symbol>, IndexLoadError> {
    let contents = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(IndexLoadError::Absent(path.to_path_buf()));
        }
        Err(e) => {
            return Err(IndexLoadError::Unreadable(format!(
                "cannot read {}: {e}",
                path.display()
            )));
        }
    };
    let mut symbols = Vec::new();
    for (i, line) in contents.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<Symbol>(line) {
            Ok(s) => symbols.push(s),
            Err(e) => {
                return Err(IndexLoadError::Unreadable(format!(
                    "{} line {}: not a symbol record: {e}",
                    path.display(),
                    i + 1
                )));
            }
        }
    }
    Ok(symbols)
}

/// [`load_index`] plus a cross-check against the build meta at `meta_path`.
///
/// On top of everything [`load_index`] rejects:
/// * meta present but not readable as [`IndexMeta`] → `Unreadable`;
/// * meta present and its `symbols` count differs from the number of records
///   in the body → `Unreadable`, naming both counts (a body truncated at a line
///   boundary still parses line by line, and would otherwise be served as
///   complete);
/// * an empty body is `Ok(vec![])` only when the meta records `symbols: 0`;
///   with no meta the empty body is unvouched-for and `Unreadable`;
/// * a non-empty body with **no** meta file is accepted: every line already
///   parsed and there is no count to compare against (indexes built before the
///   meta sidecar existed have none). This is the one case where truncation at
///   a line boundary cannot be detected.
///
/// The body and meta are written separately, so a failed meta write after a
/// successful body write leaves a stale meta that disagrees with the body and
/// reads as `Unreadable` until the next successful build — a body and meta that
/// disagree cannot be told apart from a truncated body.
pub fn load_index_checked(body: &Path, meta_path: &Path) -> Result<Vec<Symbol>, IndexLoadError> {
    let symbols = load_index(body)?;
    let meta = match std::fs::read_to_string(meta_path) {
        Ok(text) => match serde_json::from_str::<IndexMeta>(&text) {
            Ok(m) => Some(m),
            Err(e) => {
                return Err(IndexLoadError::Unreadable(format!(
                    "build meta {} cannot be read as index meta: {e}",
                    meta_path.display()
                )));
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            return Err(IndexLoadError::Unreadable(format!(
                "cannot read build meta {}: {e}",
                meta_path.display()
            )));
        }
    };
    match meta {
        Some(m) if m.symbols != symbols.len() => Err(IndexLoadError::Unreadable(format!(
            "{} holds {} symbols but its build meta {} records {} (truncated index or stale meta; rebuild the index)",
            body.display(),
            symbols.len(),
            meta_path.display(),
            m.symbols
        ))),
        None if symbols.is_empty() => Err(IndexLoadError::Unreadable(format!(
            "{} holds zero symbols and has no build meta recording an empty build",
            body.display()
        ))),
        _ => Ok(symbols),
    }
}

/// A cheap, deterministic **staleness fingerprint** over a set of source-file
/// stats — `(relative path, byte size, mtime)` tuples — used by code-RAG
/// slice-3 to decide whether a full rebuild is needed. Properties:
///
///   * **Order-independent**: the entries are sorted internally, so the same
///     file set yields the same fingerprint regardless of enumeration order
///     (e.g. `git ls-files` ordering changes must not flip it).
///   * **Deterministic across runs/processes**: folds the canonical byte
///     stream through [`DefaultHasher`], whose keys are fixed (`new()` is *not*
///     the randomized `RandomState` used by `HashMap`), so identical input
///     always yields the same hex string. Equality is all we need for a
///     staleness check — collision risk for "did the tree change" is
///     negligible.
///   * **Content-free (cheap)**: only `(size, mtime)` are consulted, never the
///     file bytes — this is the fast pre-check that gates the expensive
///     rebuild.
///
/// Never panics on any input (including empty).
pub fn fingerprint(entries: &[(String, u64, i64)]) -> String {
    let mut sorted: Vec<&(String, u64, i64)> = entries.iter().collect();
    sorted.sort();

    let mut hasher = DefaultHasher::new();
    // Hash the count first so `[]` and `[("", 0, 0)]` can't collide.
    sorted.len().hash(&mut hasher);
    for (path, size, mtime) in sorted {
        path.hash(&mut hasher);
        size.hash(&mut hasher);
        mtime.hash(&mut hasher);
    }
    format!("{:016x}", hasher.finish())
}

/// Sidecar metadata written next to the JSONL index (as
/// `.fugu/code-index.meta.json`), recording the staleness [`fingerprint`] plus
/// a small summary of the last build. Kept in a **separate** file so the
/// `.jsonl` body stays a pure one-[`Symbol`]-per-line store (the `--if-stale`
/// build path reads this for freshness; [`load_index_checked`] reads its
/// `symbols` count to detect a truncated body).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexMeta {
    /// The [`fingerprint`] of the source `.rs` set the index was last built
    /// from.
    pub fingerprint: String,
    /// Number of files scanned into the last build.
    pub files: usize,
    /// Number of symbols the last build produced.
    pub symbols: usize,
}

/// Write `meta` to `path` as a single JSON object, creating the parent dir if
/// needed. Fail-loud like [`write_index`]: any IO/serialize error is returned.
pub fn write_meta(path: &Path, meta: &IndexMeta) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string(meta).map_err(std::io::Error::other)?;
    std::fs::write(path, json)
}

/// Read the sidecar metadata from `path`. A missing or corrupt/unparseable
/// file returns `None`, never panics. `None` is only safe as "not provably
/// fresh" (every freshness caller rebuilds or reports undetermined on it); it
/// must not be read as "no meta" when vouching for a body — that is what
/// [`load_index_checked`] distinguishes.
pub fn read_meta(path: &Path) -> Option<IndexMeta> {
    let contents = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&contents).ok()
}

/// Default number of matches [`search`] callers reach for absent a reason to
/// pick a different `k` (mirrors `lessons::DEFAULT_K`'s role for its store).
pub const DEFAULT_K: usize = 10;

/// Normalise text into a token set: lowercased, split on any non-alphanumeric
/// byte (so `extract_symbols` and `foo_bar` both split into their constituent
/// words), empty tokens dropped. No stopword list and no minimum length
/// (unlike `lessons::tokenize`) — code identifiers/keywords are frequently
/// short (`fn`, `Rc`, `id`) and are exactly the tokens a code search needs to
/// match on.
fn tokenize(s: &str) -> BTreeSet<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_string())
        .collect()
}

/// The lexical token set of a symbol: its name + kind + signature combined,
/// so a query can match on any of "what it's called", "what kind of thing it
/// is", or "what its declaration line contains".
fn symbol_tokens(s: &Symbol) -> BTreeSet<String> {
    let mut t = tokenize(&s.name);
    t.extend(tokenize(&s.kind));
    t.extend(tokenize(&s.signature));
    t
}

/// A symbol paired with its lexical overlap score against a query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scored {
    pub symbol: Symbol,
    /// Count of query tokens also present in the symbol's token set.
    pub score: i64,
}

/// Deterministic **lexical** top-K search over `symbols` for `query` (token
/// overlap count — no embeddings/vector DB, same lexical-only family as
/// [`crate::lessons::search`]). Returns at most `k` symbols with a non-zero
/// score, sorted by score descending, ties broken deterministically by name
/// then line (a stable order independent of input order). An empty
/// `symbols` slice, an empty query (after tokenizing), or `k == 0` all yield
/// an empty Vec — never an error.
pub fn search(symbols: &[Symbol], query: &str, k: usize) -> Vec<Scored> {
    if k == 0 || symbols.is_empty() {
        return Vec::new();
    }
    let q = tokenize(query);
    if q.is_empty() {
        return Vec::new();
    }
    let mut scored: Vec<Scored> = symbols
        .iter()
        .map(|s| Scored {
            symbol: s.clone(),
            score: q.intersection(&symbol_tokens(s)).count() as i64,
        })
        .filter(|sc| sc.score > 0)
        .collect();
    scored.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.symbol.name.cmp(&b.symbol.name))
            .then_with(|| a.symbol.line.cmp(&b.symbol.line))
    });
    scored.truncate(k);
    scored
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = r#"pub fn hello_world() -> i32 {
    42
}

struct Point {
    x: i32,
}

pub enum Color {
    Red,
}

trait Greet {
    fn hi(&self);
}

impl Point {
    fn origin() -> Self {
        Point { x: 0 }
    }
}

impl Greet for Point {
    fn hi(&self) {}
}

mod sub {
}

pub const MAX: i32 = 100;

macro_rules! my_macro {
    () => {};
}
"#;

    fn sym(name: &str, kind: &str, file: &str, line: usize, sig: &str) -> Symbol {
        Symbol {
            name: name.to_string(),
            kind: kind.to_string(),
            file: file.to_string(),
            line,
            signature: sig.to_string(),
        }
    }

    #[test]
    fn extract_symbols_detects_all_kinds_with_correct_name_and_line() {
        let symbols = extract_symbols(SRC, "src/example.rs");
        let got: Vec<(&str, &str, usize)> = symbols
            .iter()
            .map(|s| (s.name.as_str(), s.kind.as_str(), s.line))
            .collect();
        assert_eq!(
            got,
            vec![
                ("hello_world", "fn", 1),
                ("Point", "struct", 5),
                ("Color", "enum", 9),
                ("Greet", "trait", 13),
                ("hi", "fn", 14),
                ("Point", "impl", 17),
                ("origin", "fn", 18),
                ("Point", "impl", 23),
                ("hi", "fn", 24),
                ("sub", "mod", 27),
                ("MAX", "const", 30),
                ("my_macro", "macro", 32),
            ]
        );
        for s in &symbols {
            assert_eq!(s.file, "src/example.rs");
            assert!(!s.signature.is_empty());
        }
    }

    #[test]
    fn extract_symbols_reads_name_past_static_mut() {
        // `static mut` puts `mut` in the post-keyword position; the name must
        // still be the identifier, not the `mut` qualifier.
        let symbols = extract_symbols("static mut COUNTER: i32 = 0;\n", "f.rs");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "COUNTER");
        assert_eq!(symbols[0].kind, "static");
    }

    #[test]
    fn extract_symbols_is_deterministic_across_calls() {
        let a = extract_symbols(SRC, "f.rs");
        let b = extract_symbols(SRC, "f.rs");
        assert_eq!(a, b);
    }

    #[test]
    fn extract_symbols_skips_comments_and_blank_lines() {
        let src = "// fn not_a_symbol() {}\n\n   \n/// also not a symbol\nfn real_one() {}\n";
        let symbols = extract_symbols(src, "f.rs");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "real_one");
        assert_eq!(symbols[0].line, 5);
    }

    #[test]
    fn extract_symbols_never_panics_on_pathological_input() {
        let inputs = [
            "",
            "\n\n\n",
            "fn",
            "pub",
            "impl",
            "macro_rules!",
            "🎉 fn 你好() {}",
            &"x".repeat(100_000),
        ];
        for src in inputs {
            let _ = extract_symbols(src, "f.rs");
        }
    }

    #[test]
    fn search_ranks_matching_name_above_unrelated() {
        let symbols = vec![
            sym(
                "extract_symbols",
                "fn",
                "a.rs",
                10,
                "pub fn extract_symbols(contents: &str) -> Vec<Symbol> {",
            ),
            sym(
                "unrelated_thing",
                "fn",
                "b.rs",
                3,
                "fn unrelated_thing() {}",
            ),
        ];
        let hits = search(&symbols, "extract symbols", 5);
        assert!(!hits.is_empty(), "a matching query must find a hit");
        assert_eq!(hits[0].symbol.name, "extract_symbols");
        assert!(
            hits.iter().all(|h| h.symbol.name != "unrelated_thing"),
            "an unrelated symbol must be dropped: {hits:?}"
        );
    }

    #[test]
    fn search_tiebreak_is_deterministic_by_name_then_line() {
        let symbols = vec![
            sym("zeta", "fn", "a.rs", 5, "fn zeta() {}"),
            sym("alpha", "fn", "a.rs", 1, "fn alpha() {}"),
        ];
        // Both symbols share the same score (their signature/kind both
        // contain "fn"), so the tiebreak (name, then line) decides order.
        let hits = search(&symbols, "fn", 5);
        assert_eq!(hits.len(), 2);
        assert_eq!(
            hits[0].score, hits[1].score,
            "scores must tie for this test"
        );
        assert_eq!(hits[0].symbol.name, "alpha");
        assert_eq!(hits[1].symbol.name, "zeta");
    }

    #[test]
    fn search_empty_query_or_k0_or_empty_store_is_empty() {
        let symbols = vec![sym("foo", "fn", "a.rs", 1, "fn foo() {}")];
        assert!(search(&symbols, "", 5).is_empty());
        assert!(search(&symbols, "foo", 0).is_empty());
        assert!(search(&[], "foo", 5).is_empty());
    }

    #[test]
    fn load_index_distinguishes_missing_from_corrupt() {
        assert!(
            matches!(
                load_index(Path::new("/nonexistent/code-index.jsonl")),
                Err(IndexLoadError::Absent(_))
            ),
            "a missing index is Absent, not an empty index"
        );

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("code-index.jsonl");
        let valid = serde_json::to_string(&sym("foo", "fn", "a.rs", 1, "fn foo() {}")).unwrap();
        let content = format!("{valid}\n{{ not json\n\n{valid}\n");
        std::fs::write(&path, content).unwrap();
        assert!(
            matches!(load_index(&path), Err(IndexLoadError::Unreadable(_))),
            "a corrupt line makes the index Unreadable, not a smaller valid index"
        );
    }

    #[test]
    fn write_then_load_round_trips_symbols() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("code-index.jsonl");
        let symbols = extract_symbols(SRC, "src/example.rs");
        write_index(&path, &symbols).expect("write");
        let loaded = load_index(&path).expect("load");
        assert_eq!(loaded, symbols);
    }

    #[test]
    fn write_index_rebuilds_rather_than_appends() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("code-index.jsonl");
        write_index(&path, &[sym("a", "fn", "x.rs", 1, "fn a() {}")]).expect("write a");
        write_index(&path, &[sym("b", "fn", "x.rs", 2, "fn b() {}")]).expect("write b");
        let loaded = load_index(&path).expect("load");
        assert_eq!(loaded.len(), 1, "a second write must overwrite, not append");
        assert_eq!(loaded[0].name, "b");
    }

    // ---- code-RAG slice-3: staleness fingerprint + meta sidecar ----

    #[test]
    fn fingerprint_is_deterministic_and_order_independent() {
        let a = vec![
            ("src/a.rs".to_string(), 100, 111),
            ("src/b.rs".to_string(), 200, 222),
            ("src/c.rs".to_string(), 300, 333),
        ];
        // Same entries, shuffled order.
        let b = vec![
            ("src/c.rs".to_string(), 300, 333),
            ("src/a.rs".to_string(), 100, 111),
            ("src/b.rs".to_string(), 200, 222),
        ];
        assert_eq!(
            fingerprint(&a),
            fingerprint(&a),
            "same input, same call twice"
        );
        assert_eq!(
            fingerprint(&a),
            fingerprint(&b),
            "fingerprint must not depend on enumeration order"
        );
    }

    #[test]
    fn fingerprint_changes_on_path_size_or_mtime_change() {
        let base = vec![("src/a.rs".to_string(), 100, 111)];
        let fp0 = fingerprint(&base);

        // Path changed (rename / add / remove of the set member).
        assert_ne!(
            fp0,
            fingerprint(&[("src/renamed.rs".to_string(), 100, 111)]),
            "a path change must shift the fingerprint"
        );
        // Size changed (edit that changed length).
        assert_ne!(
            fp0,
            fingerprint(&[("src/a.rs".to_string(), 101, 111)]),
            "a size change must shift the fingerprint"
        );
        // mtime changed (edit re-saved).
        assert_ne!(
            fp0,
            fingerprint(&[("src/a.rs".to_string(), 100, 999)]),
            "an mtime change must shift the fingerprint"
        );
        // Adding a file changes the set.
        assert_ne!(
            fp0,
            fingerprint(&[
                ("src/a.rs".to_string(), 100, 111),
                ("src/b.rs".to_string(), 0, 0),
            ]),
            "adding a file must shift the fingerprint"
        );
    }

    #[test]
    fn fingerprint_never_panics_on_edge_inputs() {
        let _ = fingerprint(&[]);
        let _ = fingerprint(&[(String::new(), 0, 0)]);
        let _ = fingerprint(&[("x".repeat(100_000), u64::MAX, i64::MIN)]);
        // Empty vs one zero-entry must differ (count is hashed).
        assert_ne!(fingerprint(&[]), fingerprint(&[(String::new(), 0, 0)]));
    }

    #[test]
    fn meta_write_then_read_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(".fugu").join("code-index.meta.json");
        let meta = IndexMeta {
            fingerprint: "deadbeef".to_string(),
            files: 42,
            symbols: 314,
        };
        write_meta(&path, &meta).expect("write meta");
        assert_eq!(read_meta(&path), Some(meta));
    }

    #[test]
    fn read_meta_is_fail_soft_missing_and_corrupt() {
        assert_eq!(
            read_meta(Path::new("/nonexistent/code-index.meta.json")),
            None
        );

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("code-index.meta.json");
        std::fs::write(&path, "{ not valid json").unwrap();
        assert_eq!(
            read_meta(&path),
            None,
            "corrupt meta returns None, not panic"
        );
    }
}
