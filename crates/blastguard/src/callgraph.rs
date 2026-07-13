//! Deterministic caller-enumeration core — a purely-lexical, no-dependency
//! "who references this symbol" scanner in the same family as
//! [`harness_core::code_index`]: string-token scanning only (no parser, no
//! regex, no external API), pure and deterministic, and never panics on any
//! input (fail-soft floor for pathological/garbled source).
//!
//! Given a unified diff (to learn which symbol *declarations* changed) and a
//! set of source files, it answers "which sites reference each changed
//! symbol" — the raw blast-radius signal blastguard reasons over. It reuses
//! [`harness_core::code_index::extract_symbols`] to know which lines are
//! declarations, so a symbol's own declaration site is never mis-counted as a
//! caller of itself.

use std::collections::{BTreeMap, BTreeSet};

use harness_core::code_index::extract_symbols;

/// A site where a changed symbol is referenced (called / path-qualified use).
///
/// serde-serializable so downstream (e.g. a blast-radius record) can persist
/// it; `Ord`-friendly derives keep enumeration output deterministically
/// sortable.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CallSite {
    /// Source path the reference was found in (the `path` from the scanned
    /// `(path, contents)` pair).
    pub file: String,
    /// 1-indexed line number the reference occurs on.
    pub line: usize,
    /// Name of the nearest enclosing declaration (the "caller"); empty when the
    /// reference is above any recognised declaration in the file.
    pub caller: String,
}

/// Return `true` if `c` can be part of a Rust identifier (alphanumeric or `_`).
fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Extract the names of declarations introduced/removed on `+`/`-` lines of a
/// unified diff. Reuses the same declaration-recognition idiom as
/// [`extract_symbols`] (each stripped `+`/`-` line body is fed through it).
/// Deterministic, de-duplicated, and sorted. Never panics.
pub fn changed_symbol_names(diff_text: &str) -> Vec<String> {
    let mut names: BTreeSet<String> = BTreeSet::new();
    for line in diff_text.lines() {
        // Skip file headers (`+++ b/x`, `--- a/x`) — they start with `+`/`-`
        // but are diff metadata, not added/removed source content.
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
        // Feed the stripped line body through the shared declaration scanner so
        // recognition matches `code_index` exactly (fn/struct/enum/trait/impl/
        // mod/const/static/type/macro).
        for sym in extract_symbols(body, "<diff>") {
            if !sym.name.is_empty() {
                names.insert(sym.name);
            }
        }
    }
    names.into_iter().collect()
}

/// For each changed symbol name, scan every `(path, contents)` source for
/// reference/call sites and return a [`BTreeMap`] keyed by symbol name →
/// sorted `Vec<CallSite>`. A symbol's own declaration site is excluded (a
/// declaration is not a caller of itself). Pure, no I/O, never panics, fully
/// deterministic.
pub fn enumerate_callers(
    changed_symbols: &[String],
    sources: &[(String, String)],
) -> BTreeMap<String, Vec<CallSite>> {
    let mut out: BTreeMap<String, Vec<CallSite>> = BTreeMap::new();

    for name in changed_symbols {
        // An empty symbol name would "match" every position — guard it out so
        // it never floods the result (and never panics on slicing).
        if name.is_empty() {
            out.entry(name.clone()).or_default();
            continue;
        }

        let mut sites: Vec<CallSite> = Vec::new();

        for (path, contents) in sources {
            // Lines that *declare* this symbol are excluded — a declaration is
            // not a caller of itself. Also precompute all declarations so each
            // reference can be attributed to its nearest enclosing one.
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

        // Deterministic order (file, line, caller) and de-duplicate identical
        // sites (e.g. two matches on one line collapse to one record).
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

/// Return `true` if `name` appears as a whole-word reference on `line` in a
/// call/path position: `name(` (call), `name::` or `::name` (path-qualified).
/// Whole-word means bounded by non-identifier chars, so `helper` does not
/// match inside `helperx` or `xhelper`. Never panics on non-ASCII / unbalanced
/// input (all slicing is on `match_indices` char boundaries).
fn line_references(line: &str, name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    for (idx, _) in line.match_indices(name) {
        let before = &line[..idx];
        let after = &line[idx + name.len()..];

        // Whole-word boundary check.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changed_symbol_names_reads_declarations_off_plus_minus_lines() {
        let diff = "\
--- a/src/foo.rs
+++ b/src/foo.rs
@@ -1,3 +1,4 @@
-fn old_helper() {}
+fn new_helper() -> i32 { 0 }
+pub struct Widget { x: i32 }
 fn untouched() {}
";
        let got = changed_symbol_names(diff);
        assert_eq!(got, vec!["Widget", "new_helper", "old_helper"]);
    }

    #[test]
    fn changed_symbol_names_is_sorted_deduped_and_ignores_file_headers() {
        // `+++`/`---` file headers must not be read as `+`/`-` content lines,
        // and duplicate declarations collapse.
        let diff = "\
--- a/a.rs
+++ b/a.rs
+fn dup() {}
+fn dup() {}
+fn alpha() {}
";
        let got = changed_symbol_names(diff);
        assert_eq!(got, vec!["alpha", "dup"]);
    }

    #[test]
    fn enumerate_callers_finds_a_caller_across_multiple_files() {
        let a = (
            "src/a.rs".to_string(),
            "fn helper() {}\nfn uses_a() {\n    helper();\n}\n".to_string(),
        );
        let b = (
            "src/b.rs".to_string(),
            "fn uses_b() {\n    let x = helper();\n    let _ = x;\n}\n".to_string(),
        );
        let map = enumerate_callers(&["helper".to_string()], &[a, b]);
        let sites = map.get("helper").expect("helper key present");
        assert_eq!(
            sites,
            &vec![
                CallSite {
                    file: "src/a.rs".to_string(),
                    line: 3,
                    caller: "uses_a".to_string(),
                },
                CallSite {
                    file: "src/b.rs".to_string(),
                    line: 2,
                    caller: "uses_b".to_string(),
                },
            ]
        );
    }

    #[test]
    fn enumerate_callers_excludes_the_declaration_site_itself() {
        // `helper` is declared on line 1 (also matches `helper(`), but a
        // declaration is not a caller of itself, so line 1 must not appear.
        let src = (
            "src/only.rs".to_string(),
            "fn helper() {}\nfn caller() {\n    helper();\n}\n".to_string(),
        );
        let map = enumerate_callers(&["helper".to_string()], &[src]);
        let sites = map.get("helper").expect("helper key present");
        assert_eq!(
            sites.len(),
            1,
            "only the real call site, not the decl: {sites:?}"
        );
        assert_eq!(sites[0].line, 3);
        assert_eq!(sites[0].caller, "caller");
    }

    #[test]
    fn enumerate_callers_is_deterministic_byte_identical() {
        let sources = vec![
            ("z.rs".to_string(), "fn z() { thing(); }\n".to_string()),
            ("a.rs".to_string(), "fn a() { thing(); }\n".to_string()),
        ];
        let one = enumerate_callers(&["thing".to_string()], &sources);
        let two = enumerate_callers(&["thing".to_string()], &sources);
        assert_eq!(one, two);
        // Byte-identical serialisation (BTreeMap + sorted vecs).
        assert_eq!(
            serde_json::to_string(&one).unwrap(),
            serde_json::to_string(&two).unwrap()
        );
    }

    #[test]
    fn enumerate_callers_path_qualified_reference_counts() {
        let src = (
            "src/p.rs".to_string(),
            "fn user() {\n    let _ = mymod::target();\n    Type::target();\n}\n".to_string(),
        );
        let map = enumerate_callers(&["target".to_string()], &[src]);
        let sites = map.get("target").expect("target key present");
        assert_eq!(sites.len(), 2, "both path-qualified uses: {sites:?}");
        assert!(sites.iter().all(|s| s.caller == "user"));
    }

    #[test]
    fn never_panics_on_pathological_input() {
        // Empty everything.
        let _ = changed_symbol_names("");
        let _ = enumerate_callers(&[], &[]);
        // Empty changed_symbols with real sources.
        let _ = enumerate_callers(&[], &[("f.rs".to_string(), "fn f() {}".to_string())]);
        // Real changed_symbols with empty sources.
        let _ = enumerate_callers(&["x".to_string()], &[]);
        // Empty symbol name must not match everything / panic.
        let _ = enumerate_callers(
            &[String::new()],
            &[("f.rs".to_string(), "a b c".to_string())],
        );
        // Garbage / non-ASCII / unbalanced delimiters.
        let garbage = "\
+🎉 fn 你好(( {{ [[[
-)))) ]]] target(
+++ not/a/real header target::
target(((🎉
";
        let _ = changed_symbol_names(garbage);
        let _ = enumerate_callers(
            &["target".to_string(), "你好".to_string()],
            &[
                ("junk.rs".to_string(), garbage.to_string()),
                (String::new(), "x".repeat(50_000)),
            ],
        );
    }
}
