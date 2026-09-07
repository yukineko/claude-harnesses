//! Deterministic, purely-lexical call graph — "who references this symbol".
//!
//! This module is the **single implementation**. It was lifted here from
//! `blastguard::callgraph`, which now re-exports it: two copies of a
//! same-shape lexical scanner is exactly the "independent reinvention" class
//! this repository's audits keep finding, and adding a second one while
//! extending the first would be self-defeating.
//!
//! Same family rules as [`crate::code_index`] and [`crate::text_index`]: string
//! scanning only (no parser, no regex, no external API), pure, deterministic,
//! never panics on any input.
//!
//! # Two access patterns, two functions
//!
//! [`enumerate_callers`] answers "who calls these *specific* few symbols" and
//! is O(names × lines) — right for blastguard's use, where `names` is the
//! handful of declarations a diff touched.
//!
//! [`build_graph`] answers "every edge in the tree" in one pass. It inverts the
//! loop — for each line, which *known* symbols does it reference — making it
//! O(lines) with a set lookup instead of O(all_names × lines). That is what
//! makes a persisted graph affordable: [`enumerate_callers`] rescans the whole
//! corpus on every query, which is the cost this module exists to remove.
//!
//! # Cannot-determine is not "no edges" (CLAUDE.md §3)
//!
//! [`load_graph`] returns [`Determination`]. An absent or corrupt edge store
//! resolves to `Undetermined`, never to an empty `Vec<Edge>` — because an empty
//! edge list reads downstream as "nothing calls this, it is safe to change",
//! which is the most expensive wrong answer this data can give.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::code_index::extract_symbols;
use crate::verdict::Determination;

/// A site where a symbol is referenced (called / path-qualified use).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallSite {
    /// Source path the reference was found in.
    pub file: String,
    /// 1-indexed line number the reference occurs on.
    pub line: usize,
    /// Name of the nearest enclosing declaration (the "caller"); empty when the
    /// reference is above any recognised declaration in the file.
    pub caller: String,
}

/// One caller→callee edge, with the site that witnesses it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Edge {
    /// Nearest enclosing declaration at the reference site. Empty when the
    /// reference sits above any declaration (a `use` line, a module attribute).
    pub caller: String,
    /// The referenced symbol.
    pub callee: String,
    /// Where the reference is.
    pub file: String,
    /// 1-indexed line.
    pub line: u32,
}

/// Return `true` if `c` can be part of a Rust identifier.
fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Extract the names of declarations introduced/removed on `+`/`-` lines of a
/// unified diff. Deterministic, de-duplicated, sorted. Never panics.
pub fn changed_symbol_names(diff_text: &str) -> Vec<String> {
    let mut names: BTreeSet<String> = BTreeSet::new();
    for line in diff_text.lines() {
        // `+++ b/x` / `--- a/x` start with `+`/`-` but are metadata, not source.
        if line.starts_with("+++") || line.starts_with("---") {
            continue;
        }
        let body = if let Some(rest) = line.strip_prefix('+') {
            rest
        } else if let Some(rest) = line.strip_prefix('-') {
            rest
        } else {
            continue;
        };
        for sym in extract_symbols(body, "<diff>") {
            if !sym.name.is_empty() {
                names.insert(sym.name);
            }
        }
    }
    names.into_iter().collect()
}

/// Return `true` if `name` appears as a whole-word reference on `line` in a
/// call/path position: `name(`, `name::`, or `::name`.
fn line_references(line: &str, name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    for (idx, _) in line.match_indices(name) {
        let Some(before) = line.get(..idx) else {
            continue;
        };
        let Some(after) = line.get(idx + name.len()..) else {
            continue;
        };

        let prev_ok = before.chars().next_back().map(is_ident_char) != Some(true);
        let next_ok = after.chars().next().map(is_ident_char) != Some(true);
        if !prev_ok || !next_ok {
            continue;
        }

        if after.starts_with('(') || after.starts_with("::") || before.ends_with("::") {
            return true;
        }
    }
    false
}

/// For each changed symbol name, scan every `(path, contents)` source for
/// reference sites. A symbol's own declaration site is excluded. Pure, no I/O,
/// never panics, fully deterministic.
///
/// O(names × lines). For a whole-tree graph use [`build_graph`] instead.
pub fn enumerate_callers(
    changed_symbols: &[String],
    sources: &[(String, String)],
) -> BTreeMap<String, Vec<CallSite>> {
    let mut out: BTreeMap<String, Vec<CallSite>> = BTreeMap::new();

    for name in changed_symbols {
        // An empty name would "match" every position — guard it out.
        if name.is_empty() {
            out.entry(name.clone()).or_default();
            continue;
        }

        let mut sites: Vec<CallSite> = Vec::new();

        for (path, contents) in sources {
            let symbols = extract_symbols(contents, path);
            let decl_lines: BTreeSet<usize> = symbols
                .iter()
                .filter(|s| &s.name == name)
                .map(|s| s.line)
                .collect();

            for (idx, raw_line) in contents.lines().enumerate() {
                let line_no = idx + 1;
                if decl_lines.contains(&line_no) {
                    continue;
                }
                if !line_references(raw_line, name) {
                    continue;
                }
                let caller = symbols
                    .iter()
                    .filter(|s| s.line <= line_no)
                    .max_by_key(|s| s.line)
                    .map(|s| s.name.clone())
                    .unwrap_or_default();
                sites.push(CallSite {
                    file: path.clone(),
                    line: line_no,
                    caller,
                });
            }
        }

        sites.sort_by(|a, b| {
            a.file
                .cmp(&b.file)
                .then_with(|| a.line.cmp(&b.line))
                .then_with(|| a.caller.cmp(&b.caller))
        });
        sites.dedup();

        out.insert(name.clone(), sites);
    }

    out
}

/// Every identifier on `line` that sits in a call/path position, as
/// `(identifier, is_reference)`.
///
/// One left-to-right pass over the line, so whole-tree edge building costs
/// O(line length) rather than O(known symbols × line length). The position
/// rules are identical to [`line_references`]: an identifier counts when it is
/// followed by `(` or `::`, or preceded by `::`.
fn referenced_idents(line: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let chars: Vec<(usize, char)> = line.char_indices().collect();
    let mut i = 0usize;

    while i < chars.len() {
        let Some(&(start_byte, c)) = chars.get(i) else {
            break;
        };
        if !is_ident_char(c) {
            i += 1;
            continue;
        }
        // Consume the whole identifier.
        let mut j = i;
        while let Some(&(_, cc)) = chars.get(j) {
            if is_ident_char(cc) {
                j += 1;
            } else {
                break;
            }
        }
        let end_byte = chars.get(j).map(|&(b, _)| b).unwrap_or(line.len());
        let Some(ident) = line.get(start_byte..end_byte) else {
            i = j;
            continue;
        };

        // A leading digit means this is a numeric literal, not an identifier.
        let numeric = ident.chars().next().map(|c| c.is_ascii_digit()) == Some(true);
        if !numeric {
            let after = line.get(end_byte..).unwrap_or("");
            let before = line.get(..start_byte).unwrap_or("");
            if after.starts_with('(') || after.starts_with("::") || before.ends_with("::") {
                out.push(ident.to_string());
            }
        }

        i = j.max(i + 1);
    }

    out
}

/// Build every caller→callee edge in one pass over `sources`.
///
/// Only references to symbols **declared somewhere in `sources`** become edges;
/// a call to `std::fs::read` produces no edge because `read` is not a symbol
/// this corpus declares. That keeps the graph closed over the indexed tree
/// instead of accumulating unresolvable names.
///
/// Declaration lines are not edges to themselves. Output is sorted and
/// deduplicated, so the same input always serializes byte-identically.
///
/// # Known limits (lexical, by design)
///
/// No module or type resolution: two same-named functions in different modules
/// collapse to one node, and a method call `x.foo()` is not distinguished from
/// a free function `foo()`. This is the same fidelity the rest of the lexical
/// layer offers and is stated here so a caller does not read more precision
/// into the graph than it has./// True for a line that mentions names without calling them: a comment, a doc
/// comment, or a `use` import.
///
/// Without this the graph filled up with edges that are not calls at all. A
/// `use crate::append::append_line;` at the top of a file became "this file
/// calls append_line", and a doc comment naming a function became a call from
/// whatever happened to be declared above it. Both are references — which is
/// what [`enumerate_callers`] is for, and why it keeps counting them — but a
/// CALL graph that cannot tell an import from an invocation reports the entire
/// import list of a popular helper as its callers.
///
/// Lexical, so a `//` inside a string literal is misread as a comment. That
/// direction is the safe one: it drops an edge rather than inventing one.
fn is_reference_only_line(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("//")
        || t.starts_with("/*")
        || t.starts_with('*')
        || t.starts_with("use ")
        || t.starts_with("pub use ")
}

pub fn build_graph(sources: &[(String, String)]) -> Vec<Edge> {
    // Every declared name in the corpus, and where each file's declarations sit.
    let mut known: BTreeSet<String> = BTreeSet::new();
    let mut per_file: Vec<(&String, &String, Vec<crate::code_index::Symbol>)> =
        Vec::with_capacity(sources.len());

    for (path, contents) in sources {
        let symbols = extract_symbols(contents, path);
        for s in &symbols {
            if !s.name.is_empty() {
                known.insert(s.name.clone());
            }
        }
        per_file.push((path, contents, symbols));
    }

    let mut edges: BTreeSet<Edge> = BTreeSet::new();

    for (path, contents, symbols) in &per_file {
        // Declaration lines, so a declaration is not an edge to itself.
        let decl_lines: BTreeMap<usize, &str> =
            symbols.iter().map(|s| (s.line, s.name.as_str())).collect();

        for (idx, raw_line) in contents.lines().enumerate() {
            if is_reference_only_line(raw_line) {
                continue;
            }
            let line_no = idx + 1;
            let enclosing = symbols
                .iter()
                .filter(|s| s.line <= line_no)
                .max_by_key(|s| s.line)
                .map(|s| s.name.clone())
                .unwrap_or_default();

            for ident in referenced_idents(raw_line) {
                if !known.contains(&ident) {
                    continue;
                }
                // Skip the symbol's own declaration line.
                if decl_lines.get(&line_no) == Some(&ident.as_str()) {
                    continue;
                }
                edges.insert(Edge {
                    caller: enclosing.clone(),
                    callee: ident,
                    file: (*path).clone(),
                    line: u32::try_from(line_no).unwrap_or(u32::MAX),
                });
            }
        }
    }

    edges.into_iter().collect()
}

/// Write edges as JSONL, one per line. Fail-soft (a cache write must not break
/// a turn); the honesty lives in [`load_graph`].
pub fn write_graph(path: &Path, edges: &[Edge]) {
    use std::io::Write;

    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(file) = std::fs::File::create(path) else {
        return;
    };
    let mut w = std::io::BufWriter::new(file);
    for e in edges {
        let Ok(json) = serde_json::to_string(e) else {
            continue;
        };
        let _ = writeln!(w, "{}", json);
    }
    let _ = w.flush();
}

/// Load edges. **Fail-closed**: absent, unreadable, or partially-corrupt stores
/// resolve to [`Determination::Undetermined`], never to an empty edge list.
///
/// An empty *file* is `Known(vec![])` — a corpus with no edges is a legitimate
/// observation. A file with unparseable records is not: it under-reports, and
/// under-reported callers read as "safe to change".
pub fn load_graph(path: &Path) -> Determination<Vec<Edge>> {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return Determination::undetermined(format!("call graph unreadable at {}", path.display()));
    };

    let mut edges: Vec<Edge> = Vec::new();
    let mut skipped = 0usize;
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<Edge>(line) {
            Ok(e) => edges.push(e),
            Err(_) => skipped += 1,
        }
    }

    if skipped > 0 {
        return Determination::undetermined(format!(
            "call graph at {} has {} unparseable record(s) — a partial graph \
             under-reports callers, which reads as 'safe to change'",
            path.display(),
            skipped
        ));
    }

    Determination::known(edges)
}

/// Every edge whose callee is `name` — "who calls this".
///
/// Pure slice over an already-loaded graph: no rescan, which is the entire
/// point of persisting it.
pub fn callers_of<'a>(edges: &'a [Edge], name: &str) -> Vec<&'a Edge> {
    edges.iter().filter(|e| e.callee == name).collect()
}

/// Every edge whose caller is `name` — "what this calls".
pub fn callees_of<'a>(edges: &'a [Edge], name: &str) -> Vec<&'a Edge> {
    edges.iter().filter(|e| e.caller == name).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corpus() -> Vec<(String, String)> {
        vec![
            (
                "a.rs".to_string(),
                "fn helper() {}\nfn uses_a() {\n    helper();\n}\n".to_string(),
            ),
            (
                "b.rs".to_string(),
                "fn uses_b() {\n    let x = helper();\n    mymod::helper();\n}\n".to_string(),
            ),
        ]
    }

    #[test]
    fn build_graph_finds_caller_to_callee_edges_across_files() {
        let edges = build_graph(&corpus());
        let callers: Vec<&str> = callers_of(&edges, "helper")
            .iter()
            .map(|e| e.caller.as_str())
            .collect();
        assert!(callers.contains(&"uses_a"), "got {:?}", callers);
        assert!(callers.contains(&"uses_b"), "got {:?}", callers);
    }

    #[test]
    fn build_graph_excludes_a_declarations_own_line() {
        let edges = build_graph(&corpus());
        // `fn helper() {}` is a.rs:1 — it must not be an edge helper->helper.
        assert!(
            !edges
                .iter()
                .any(|e| e.callee == "helper" && e.file == "a.rs" && e.line == 1),
            "a declaration is not a caller of itself"
        );
    }

    #[test]
    fn build_graph_is_closed_over_the_indexed_corpus() {
        // `println` is referenced but declared nowhere in the corpus, so it must
        // not become a node — otherwise the graph accumulates unresolvable names.
        let src = vec![(
            "c.rs".to_string(),
            "fn f() {\n    println!(\"hi\");\n}\n".to_string(),
        )];
        let edges = build_graph(&src);
        assert!(edges.iter().all(|e| e.callee != "println"));
    }

    #[test]
    fn build_graph_is_deterministic_regardless_of_source_order() {
        let mut reversed = corpus();
        reversed.reverse();
        assert_eq!(build_graph(&corpus()), build_graph(&reversed));
    }

    #[test]
    fn build_graph_agrees_with_enumerate_callers_on_who_calls_helper() {
        // The two access patterns must not disagree — they are one implementation
        // seen from two directions, and a divergence here is the reinvention this
        // module was consolidated to prevent.
        let sources = corpus();
        let via_graph: std::collections::BTreeSet<(String, u32)> =
            callers_of(&build_graph(&sources), "helper")
                .iter()
                .map(|e| (e.file.clone(), e.line))
                .collect();
        let via_scan: std::collections::BTreeSet<(String, u32)> =
            enumerate_callers(&["helper".to_string()], &sources)
                .get("helper")
                .map(|sites| {
                    sites
                        .iter()
                        .map(|s| (s.file.clone(), s.line as u32))
                        .collect()
                })
                .unwrap_or_default();
        assert_eq!(via_graph, via_scan);
    }

    #[test]
    fn callees_of_is_the_inverse_direction() {
        let edges = build_graph(&corpus());
        let callees: Vec<&str> = callees_of(&edges, "uses_a")
            .iter()
            .map(|e| e.callee.as_str())
            .collect();
        assert!(callees.contains(&"helper"));
    }

    #[test]
    fn roundtrip_through_disk_preserves_edges() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("edges.jsonl");
        let edges = build_graph(&corpus());
        write_graph(&path, &edges);
        match load_graph(&path) {
            Determination::Known(loaded) => assert_eq!(loaded, edges),
            Determination::Undetermined(_) => panic!("clean roundtrip must be Known"),
        }
    }

    #[test]
    fn missing_graph_is_undetermined_not_an_empty_edge_list() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            matches!(
                load_graph(&dir.path().join("absent.jsonl")),
                Determination::Undetermined(_)
            ),
            "an absent graph must not read as 'nothing calls this'"
        );
    }

    #[test]
    fn corrupt_graph_record_is_undetermined_not_a_partial_answer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("edges.jsonl");
        write_graph(&path, &build_graph(&corpus()));
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("{ not json\n");
        std::fs::write(&path, text).unwrap();
        assert!(matches!(load_graph(&path), Determination::Undetermined(_)));
    }

    #[test]
    fn a_genuinely_edgeless_corpus_is_known_empty_not_undetermined() {
        // Anti-vacuity: the fail-closed tests must not have been satisfied by
        // making every load Undetermined.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("edges.jsonl");
        write_graph(&path, &[]);
        match load_graph(&path) {
            Determination::Known(e) => assert!(e.is_empty()),
            Determination::Undetermined(_) => {
                panic!("an empty graph file is a real observation, not a failure")
            }
        }
    }

    #[test]
    fn imports_and_comments_are_not_calls() {
        let sources = vec![
            ("a.rs".to_string(), "fn helper() {}\n".to_string()),
            (
                "b.rs".to_string(),
                concat!(
                    "use crate::a::helper;\n",
                    "/// Calls [`helper`] eventually.\n",
                    "// helper();\n",
                    "fn real() {\n    helper();\n}\n",
                )
                .to_string(),
            ),
        ];
        let edges = build_graph(&sources);
        let lines: Vec<u32> = callers_of(&edges, "helper")
            .iter()
            .filter(|e| e.file == "b.rs")
            .map(|e| e.line)
            .collect();
        assert_eq!(
            lines,
            vec![5],
            "only the real call site is an edge: {lines:?}"
        );
    }

    #[test]
    fn a_call_with_no_enclosing_declaration_is_still_recorded() {
        // Anti-vacuity for the filter above: dropping comment and import lines
        // must not have been achieved by dropping lines with no known caller.
        let sources = vec![
            ("a.rs".to_string(), "fn helper() {}\n".to_string()),
            (
                "b.rs".to_string(),
                "static X: u32 = helper();\nfn after() {}\n".to_string(),
            ),
        ];
        let edges = build_graph(&sources);
        assert!(
            callers_of(&edges, "helper")
                .iter()
                .any(|e| e.file == "b.rs" && e.line == 1),
            "got {edges:?}"
        );
    }

    #[test]
    fn never_panics_on_pathological_input() {
        let weird = "\u{1F389} fn \u{4F60}\u{597D}(( {{ [[[::\n\0\n";
        let _ = build_graph(&[("w.rs".to_string(), weird.to_string())]);
        let _ = build_graph(&[]);
        let _ = referenced_idents(weird);
        let _ = referenced_idents("");
    }
}
