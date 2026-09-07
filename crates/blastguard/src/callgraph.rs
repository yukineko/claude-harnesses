//! Deterministic caller-enumeration — **a thin re-export** of
//! [`harness_core::callgraph`].
//!
//! The implementation used to live here. It moved to `harness-core` when the
//! call graph gained a persisted, whole-tree form
//! ([`harness_core::callgraph::build_graph`]), because keeping a second copy of
//! the same lexical scanner in a gate crate is the "independent reinvention"
//! failure this repository's audits repeatedly find — and duplicating it *while*
//! fixing that class would have been self-defeating.
//!
//! Nothing about blastguard's contract changes: `blastguard::callgraph::{CallSite,
//! changed_symbol_names, enumerate_callers}` resolve exactly as before, and the
//! tests below are unchanged, so they now prove the moved implementation still
//! satisfies blastguard's expectations rather than merely re-testing a copy.

pub use harness_core::callgraph::{changed_symbol_names, enumerate_callers, CallSite};

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
