//! Deterministic complexity classifier for UserPromptSubmit.
//!
//! Detects complex problems that warrant `/flow` driver routing based on
//! keyword patterns: production impact, cross-crate Rust changes, test
//! impossibility, explicit "condukt で回して" requests, etc.
//! No LLM, no embeddings — pure keyword matching with fail-soft semantics.

/// Check if a prompt describes a complex problem that should be routed to /flow.
pub fn is_complex_problem(prompt: &str) -> bool {
    // Normalize for comparison: lowercase, some pattern matching.
    let lower = prompt.to_lowercase();

    // Keywords indicating production impact.
    let prod_keywords = [
        "本番", // "production" (Japanese)
        "production",
        "prod",
        "critical",
        "critical fix",
        "urgent",
        "緊急", // "urgent" (Japanese)
    ];

    // Keywords indicating complex Rust/multi-file changes.
    let rust_keywords = [
        "複数の crate", // "multiple crates"
        "multiple crate",
        "cross-crate",
        "クレート間",
        "crate",
        "struct",
        "trait",
        "macro",
        "unsafe",
        "lifetime",
        "trait object",
        "async",
        "concurrency",
    ];

    // Keywords indicating test impossibility or constraint.
    let constraint_keywords = [
        "テスト不可", // "unable to test"
        "test impossible",
        "can't test",
        "cannot test",
        "no test",
        "live env",
        "本番環境のみ", // "production env only"
        "constraint",
        "limited",
        "restricted",
    ];

    // Keywords indicating explicit "/flow" preference.
    let flow_keywords = [
        "condukt で回して", // "run through condukt" (Japanese)
        "condukt で実装",   // "implement through condukt"
        "flow",
        "parallel",
        "orchestrat",   // covers "orchestrate" / "orchestration"
        "複数のタスク", // "multiple tasks"
        "multi-task",
    ];

    // Heuristic weighting: any single strong signal is sufficient.
    let has_prod = prod_keywords.iter().any(|kw| lower.contains(kw));
    let has_rust = rust_keywords.iter().any(|kw| lower.contains(kw));
    let has_constraint = constraint_keywords.iter().any(|kw| lower.contains(kw));
    let has_flow = flow_keywords.iter().any(|kw| lower.contains(kw));

    // Composite rule: detect if complexity is high enough.
    // - Explicit flow request → always route
    // - Production + (Rust OR constraint) → route
    // - Rust + constraint → route
    // - Single strong signal alone (just prod, or just constraint) → don't route
    //   (these need to co-occur with something else to avoid false positives)

    has_flow                              // explicit request
        || (has_prod && (has_rust || has_constraint))  // prod + complexity
        || (has_rust && has_constraint) // multi-faceted complexity
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_flow_request() {
        assert!(is_complex_problem("condukt で回してください"));
        assert!(is_complex_problem("this is a flow task"));
        assert!(is_complex_problem("let's run this via /flow"));
    }

    #[test]
    fn production_plus_rust() {
        assert!(is_complex_problem("本番 struct を変更"));
        assert!(is_complex_problem(
            "production Rust refactor across 3 crates"
        ));
    }

    #[test]
    fn production_plus_constraint() {
        assert!(is_complex_problem("本番環境でテスト不可な変更"));
        assert!(is_complex_problem("production change but can't test"));
    }

    #[test]
    fn rust_plus_constraint() {
        assert!(is_complex_problem(
            "複数クレートの unsafe コード、テスト不可な状況"
        ));
        assert!(is_complex_problem("trait refactor but test impossible"));
    }

    #[test]
    fn simple_prompt_not_complex() {
        assert!(!is_complex_problem("css classをリネーム"));
        assert!(!is_complex_problem("fix a typo in the doc"));
        assert!(!is_complex_problem("update a comment"));
    }

    #[test]
    fn single_signal_weak() {
        // Prod alone or constraint alone shouldn't trigger (to avoid false positives).
        // They need accompaniment.
        assert!(!is_complex_problem("production bug fix (normal)"));
        assert!(!is_complex_problem("we can't test this alone")); // no other signal
    }

    #[test]
    fn case_insensitive() {
        assert!(is_complex_problem("CONDUKT で回して"));
        assert!(is_complex_problem("Production STRUCT改修"));
    }
}
