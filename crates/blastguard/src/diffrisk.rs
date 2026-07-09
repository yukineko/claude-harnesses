//! Diff-level semantic risk signals: configurable sensitive-path globs and
//! public-API surface changes. These feed the SAME [`crate::classify::RiskAssessment`]
//! that command classification produces, so callers (e.g. condukt's
//! `gate check` / force-gate escalation) reason over one graded axis rather
//! than a parallel path. Pure, deterministic, no I/O, no LLM.

use globset::{Glob, GlobSet, GlobSetBuilder};

use crate::classify::{Risk, RiskAssessment};

/// Built-in sensitive-path globs: directories/files that typically hold
/// authn/authz, payment, or PII-handling logic. Deliberately conservative
/// (name-based heuristics only) — callers can extend this list via
/// [`SensitiveConfig::with_extra_globs`] with project-specific paths.
pub const DEFAULT_SENSITIVE_GLOBS: &[&str] = &[
    // auth / authn / authz
    "**/auth/**",
    "**/*auth*",
    "**/authn/**",
    "**/authz/**",
    "**/login/**",
    "**/session/**",
    "**/oauth/**",
    "**/*credential*",
    "**/*secret*",
    // payment / billing
    "**/payment*/**",
    "**/*payment*",
    "**/billing/**",
    "**/*billing*",
    "**/checkout/**",
    "**/*stripe*",
    // PII / user data
    "**/pii/**",
    "**/*pii*",
    "**/*personal_data*",
    "**/gdpr/**",
];

/// Configurable sensitive-path glob set. `default()` uses
/// [`DEFAULT_SENSITIVE_GLOBS`]; callers can layer project-specific globs on
/// top via [`SensitiveConfig::with_extra_globs`] (e.g. sourced from
/// condukt's `Config`, mirroring how `shared_globs` is threaded today).
pub struct SensitiveConfig {
    globs: Vec<String>,
}

impl Default for SensitiveConfig {
    fn default() -> Self {
        SensitiveConfig {
            globs: DEFAULT_SENSITIVE_GLOBS
                .iter()
                .map(|s| s.to_string())
                .collect(),
        }
    }
}

impl SensitiveConfig {
    /// Start from the built-in defaults plus caller-supplied extra globs.
    pub fn with_extra_globs(extra: &[String]) -> Self {
        let mut globs: Vec<String> = DEFAULT_SENSITIVE_GLOBS
            .iter()
            .map(|s| s.to_string())
            .collect();
        globs.extend(extra.iter().cloned());
        SensitiveConfig { globs }
    }

    /// Start from ONLY caller-supplied globs (no built-in defaults). Useful
    /// when a project wants to fully own the sensitive-path list.
    pub fn from_globs(globs: Vec<String>) -> Self {
        SensitiveConfig { globs }
    }

    fn build(&self) -> GlobSet {
        let mut b = GlobSetBuilder::new();
        for pat in &self.globs {
            if let Ok(g) = Glob::new(pat) {
                b.add(g);
            }
        }
        b.build().unwrap_or_else(|_| GlobSet::empty())
    }

    /// True when any of `paths` matches a sensitive-path glob.
    pub fn any_sensitive(&self, paths: &[String]) -> bool {
        let set = self.build();
        paths.iter().any(|p| {
            let norm = p.trim().replace('\\', "/");
            !norm.is_empty() && set.is_match(&norm)
        })
    }
}

/// True when a unified-diff-style text contains an added or removed line that
/// changes Rust public API surface: `pub fn`, `pub struct`, `pub enum`,
/// `pub trait`, `pub const`, `pub static`, `pub type`, or `pub mod` (also
/// matches `pub(crate)`-qualified — treated the same as any `pub` marker,
/// since narrowing/widening visibility is itself a surface change).
///
/// Deliberately line-oriented and diff-marker-aware (`+`/`-` prefix): plain
/// context lines (no marker) are ignored so an unrelated diff that merely
/// contains the substring "pub fn" in a comment/context line does not false
/// positive. Pure text scan, no AST parsing — matches blastguard's existing
/// idiom of cheap deterministic pattern matching over full static analysis.
pub fn changes_public_symbol(diff_text: &str) -> bool {
    const MARKERS: &[&str] = &[
        "pub fn ",
        "pub async fn ",
        "pub struct ",
        "pub enum ",
        "pub trait ",
        "pub const ",
        "pub static ",
        "pub type ",
        "pub mod ",
    ];
    diff_text.lines().any(|line| {
        let trimmed = line.trim_start();
        let (marker, body) = if let Some(rest) = trimmed.strip_prefix('+') {
            ('+', rest.trim_start())
        } else if let Some(rest) = trimmed.strip_prefix('-') {
            ('-', rest.trim_start())
        } else {
            return false;
        };
        // Exclude the `+++`/`---` file-header lines unified diffs emit.
        if body.starts_with('+') || body.starts_with('-') {
            return false;
        }
        let _ = marker;
        MARKERS.iter().any(|m| {
            if body.starts_with(m) {
                return true;
            }
            // `pub(crate)`-qualified variant of the same marker. Every entry
            // in MARKERS starts with the literal `"pub "` keyword, so the
            // real Rust source spelling is obtained by replacing that
            // leading `"pub "` with `"pub(crate) "` (e.g. `"pub fn "` ->
            // `"pub(crate) fn "`), NOT by prepending `"pub(crate) "` in
            // front of the already-`"pub "`-prefixed marker (that used to
            // build the non-existent string `"pub(crate) pub fn "`, which
            // never matches any real source line — see finding 6 in
            // docs/review-redesign-implementation-items.md).
            let pub_crate_variant = m.replacen("pub ", "pub(crate) ", 1);
            body.starts_with(&pub_crate_variant)
        })
    })
}

/// Grade the diff-level semantic risk of a change given its touched paths and
/// (optionally empty) unified-diff text, merging into the same
/// [`RiskAssessment`] axes command classification uses:
///
///   - a touched path matches a sensitive-path glob → at least Medium,
///     reversible (a human can still revert the commit; it is the *review*
///     posture that must escalate, not undoability).
///   - a public/exported symbol changed                → at least Medium,
///     reversible, same rationale.
///   - both signals fire                                → High, reversible
///     (compounded review-worthiness forces the same GATED escalation path
///     command classification uses for High + irreversible, via
///     [`RiskAssessment::requires_gate`] which callers can OR with an
///     explicit "review required" check — see module docs).
///
/// Additive only: never LOWERS a risk tier. Callers combine this with
/// [`crate::classify::classify`] on the action text and take the
/// higher-risk verdict (see [`crate::classify::classify_change`]).
///
/// ## Known limitation: `diff_text` is empty on the only in-workspace
/// production call sites (finding 8, `docs/review-redesign-implementation-`
/// `items.md`)
///
/// The two production callers of this function in the workspace —
/// `condukt::gate_exec::gather_assessment` and `condukt::schedule::schedule`
/// — both pass `diff_text = ""`. This is *intentional*, not an oversight:
/// both call sites run at a pre-execution stage (force-gate / schedule-time
/// risk classification) where the command has not run yet, so no actual
/// diff exists to inspect — there is nothing to line-scan for `pub`/
/// `pub(crate)` markers at that point in the pipeline. `changes_public_symbol`
/// therefore always evaluates `false` on those paths today, and the
/// `public_api` signal below only ever fires when a *caller downstream of an
/// actual diff* (e.g. a future post-hoc/post-execution review pass, or any
/// caller — in this crate's own tests, or a future integration — that has a
/// real unified diff in hand) invokes `classify_diff` with non-empty
/// `diff_text`. That is the realistic point at which item D (generalized
/// risk scoring for public-API changes) is reachable and correct; see the
/// unit tests below (`public_symbol_alone_raises_to_medium`,
/// `both_signals_compound_to_high`, `pub_crate_*`) which pin this behavior
/// with real diff text. Wiring an actual diff into the two schedule-time
/// call sites is a separate, condukt-side change and out of scope here.
pub fn classify_diff(
    paths: &[String],
    diff_text: &str,
    config: &SensitiveConfig,
) -> RiskAssessment {
    let sensitive = config.any_sensitive(paths);
    let public_api = changes_public_symbol(diff_text);

    let risk = match (sensitive, public_api) {
        (true, true) => Risk::High,
        (true, false) | (false, true) => Risk::Medium,
        (false, false) => Risk::Low,
    };

    RiskAssessment {
        risk,
        reversible: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensitive_path_defaults_match_auth_payment_pii() {
        let cfg = SensitiveConfig::default();
        assert!(cfg.any_sensitive(&["src/auth/login.rs".to_string()]));
        assert!(cfg.any_sensitive(&["crates/api/src/payment_gateway.rs".to_string()]));
        assert!(cfg.any_sensitive(&["services/billing/invoice.py".to_string()]));
        assert!(cfg.any_sensitive(&["src/pii/redact.rs".to_string()]));
        assert!(cfg.any_sensitive(&["src/checkout/cart.rs".to_string()]));
        assert!(!cfg.any_sensitive(&["src/parser.rs".to_string(), "README.md".to_string()]));
    }

    #[test]
    fn extra_globs_extend_defaults() {
        let cfg = SensitiveConfig::with_extra_globs(&["**/secrets_vault/**".to_string()]);
        assert!(cfg.any_sensitive(&["infra/secrets_vault/keys.rs".to_string()]));
        // Defaults still apply.
        assert!(cfg.any_sensitive(&["src/auth/token.rs".to_string()]));
    }

    #[test]
    fn from_globs_uses_only_caller_list() {
        let cfg = SensitiveConfig::from_globs(vec!["**/custom/**".to_string()]);
        assert!(cfg.any_sensitive(&["a/custom/b.rs".to_string()]));
        assert!(!cfg.any_sensitive(&["src/auth/login.rs".to_string()]));
    }

    #[test]
    fn public_fn_addition_detected() {
        let diff = "-fn helper() {}\n+pub fn helper() {}\n";
        assert!(changes_public_symbol(diff));
    }

    #[test]
    fn public_struct_and_enum_detected() {
        assert!(changes_public_symbol("+pub struct Foo { x: i32 }"));
        assert!(changes_public_symbol("+pub enum Bar { A, B }"));
        assert!(changes_public_symbol("-pub trait Baz {}"));
    }

    #[test]
    fn private_change_not_detected() {
        let diff = "-fn helper() {}\n+fn helper2() {}\n context line pub fn unrelated()\n";
        assert!(!changes_public_symbol(diff));
    }

    #[test]
    fn diff_file_headers_are_not_mistaken_for_pub_markers() {
        // Unified diff file headers start with `+++`/`---`; must not match.
        let diff = "+++ b/src/lib.rs\n--- a/src/lib.rs\n";
        assert!(!changes_public_symbol(diff));
    }

    #[test]
    fn neither_signal_is_low_baseline() {
        let cfg = SensitiveConfig::default();
        let a = classify_diff(
            &["src/parser.rs".to_string()],
            "-fn helper() {}\n+fn helper2() {}\n",
            &cfg,
        );
        assert_eq!(a.risk, Risk::Low);
        assert!(a.reversible);
        assert!(!a.requires_gate());
    }

    #[test]
    fn sensitive_path_alone_raises_to_medium() {
        let cfg = SensitiveConfig::default();
        let a = classify_diff(&["src/auth/login.rs".to_string()], "", &cfg);
        assert_eq!(a.risk, Risk::Medium);
        assert!(a.reversible);
    }

    #[test]
    fn public_symbol_alone_raises_to_medium() {
        let cfg = SensitiveConfig::default();
        let a = classify_diff(&["src/lib.rs".to_string()], "+pub fn new_api() {}", &cfg);
        assert_eq!(a.risk, Risk::Medium);
        assert!(a.reversible);
    }

    #[test]
    fn both_signals_compound_to_high() {
        let cfg = SensitiveConfig::default();
        let a = classify_diff(
            &["src/auth/token.rs".to_string()],
            "+pub fn issue_token() {}",
            &cfg,
        );
        assert_eq!(a.risk, Risk::High);
        assert!(a.reversible);
    }

    // -- finding 6: pub(crate) needle regression coverage --------------------

    #[test]
    fn pub_crate_fn_addition_detected() {
        let diff = "-fn helper() {}\n+pub(crate) fn helper() {}\n";
        assert!(changes_public_symbol(diff));
    }

    #[test]
    fn pub_crate_fn_removal_detected() {
        let diff = "-pub(crate) fn helper() {}\n+fn helper() {}\n";
        assert!(changes_public_symbol(diff));
    }

    #[test]
    fn pub_crate_struct_and_enum_and_trait_detected() {
        assert!(changes_public_symbol("+pub(crate) struct Foo { x: i32 }"));
        assert!(changes_public_symbol("+pub(crate) enum Bar { A, B }"));
        assert!(changes_public_symbol("-pub(crate) trait Baz {}"));
    }

    #[test]
    fn pub_crate_async_fn_detected() {
        assert!(changes_public_symbol("+pub(crate) async fn fetch() {}"));
    }

    #[test]
    fn pub_crate_alone_raises_diff_to_medium() {
        // Regression for finding 6: before the fix, the needle built the
        // non-existent string "pub(crate) pub fn " and never matched real
        // `pub(crate) fn` source lines, so this diff-only change was
        // silently scored Low instead of Medium.
        let cfg = SensitiveConfig::default();
        let a = classify_diff(
            &["src/lib.rs".to_string()],
            "+pub(crate) fn internal_api() {}",
            &cfg,
        );
        assert_eq!(a.risk, Risk::Medium);
        assert!(a.reversible);
    }

    #[test]
    fn malformed_pub_crate_needle_does_not_falsely_match() {
        // The old (buggy) needle was "pub(crate) pub fn " — assert real
        // source using the correct spelling doesn't require that broken
        // prefix, i.e. this line must be detected without the redundant
        // "pub " duplication ever being present in real source.
        let diff = "+pub(crate) fn only_one_pub_keyword() {}\n";
        assert!(!diff.contains("pub(crate) pub"));
        assert!(changes_public_symbol(diff));
    }
}
