//! Map-driven CORRECTNESS audit scope + structural findings + shard assembly.
//!
//! This module is the deterministic core of the read-only `spec-audit` feature.
//! It is a *consumer* of the persisted [`crate::specmap::SpecMap`] store (built
//! earlier, `.specguard/spec-map.toml`): it never mutates the map, never fixes
//! anything, and never spawns an agent. It emits (1) deterministic structural
//! findings and (2) LLM audit shards packaged in the SAME machine-readable
//! envelope shape as `prompt --json`, so the command layer can reuse the
//! existing `specguard ingest` parse→report→sentinel pipeline unchanged.
//!
//! ## Semantic distinction (important)
//!
//! `spec-audit` is NOT `spec-drift`. Drift asks "do spec and impl AGREE?"
//! (consistency). Audit asks whether the implementation and the spec are
//! actually RIGHT — spec soundness, implementation correctness, and test
//! adequacy — beyond mere mutual consistency. Fixing is drift-map's job; this
//! feature is read-only and only surfaces findings for a report/sentinel.
//!
//! ## Layers (kept separate so classification is unit-testable without a disk)
//!   * pure helpers — [`is_undocumented`], [`is_untested`], [`dangling_refs`]
//!     and [`structural_findings`] take entry data (and, for dangling refs, an
//!     injected existence predicate) so they are testable with no filesystem.
//!   * [`scan_map_filtered`] — the thin FS wrapper that supplies the real
//!     on-disk existence check (and optional targeting filter).
//!   * [`build_envelope`] — assembles one audit shard per auditable entry from
//!     the embedded [`DEFAULT_AUDIT_TEMPLATE`], embedding each entry's
//!     structural signals, into an [`AuditEnvelope`].

use crate::config::Config;
use crate::parse::MARKER;
use crate::specmap::{MapEntry, SpecMap};
use serde::Serialize;
use std::path::Path;

/// The embedded correctness-audit shard template (read-only). Framed as a
/// CORRECTNESS audit (spec soundness + impl correctness + test adequacy), not a
/// drift/consistency check. Emits the same completion marker the ingest pipeline
/// expects.
pub const DEFAULT_AUDIT_TEMPLATE: &str = include_str!("../templates/spec-audit-prompt.md");

/// Placeholders the spec-audit shard template must contain — the render contract.
pub const AUDIT_PLACEHOLDERS: &[&str] = &[
    "{{PROJECT_NAME}}",
    "{{DATE}}",
    "{{MARKER}}",
    "{{FEATURE_KEY}}",
    "{{SPEC_DOC}}",
    "{{IMPL_FILES}}",
    "{{TEST_FILES}}",
    "{{STRUCTURAL_SIGNALS}}",
];

/// Upper bound on the number of audit shards emitted per run, so a large map
/// can never dispatch an unbounded number of LLM calls (mirrors the
/// `MAX_DECISIONS` / `MAX_SAMPLE_FILES` truncation idiom elsewhere). Entries are
/// taken in the map's stable (BTreeMap) key order.
pub const MAX_AUDIT_SHARDS: usize = 50;

/// A deterministic, LLM-free structural signal about one map entry. These are
/// pre-computed and handed to the auditor as signals to confirm/extend — never a
/// verdict on their own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructuralFinding {
    /// The map entry key this finding is about.
    pub key: String,
    /// What kind of structural gap was detected.
    pub kind: StructuralKind,
    /// Human-readable detail (e.g. the offending paths).
    pub detail: String,
}

/// The deterministic structural classifications spec-audit computes from the map
/// entry triple (spec_doc / impl_files / test_files) plus on-disk existence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructuralKind {
    /// impl_files present but spec_doc empty/missing → the feature is
    /// implemented but undocumented.
    Undocumented,
    /// A referenced impl_files/test_files path does not exist on disk → the map
    /// points at something that is no longer there.
    DanglingReference,
    /// impl_files present but test_files empty → the feature is implemented but
    /// has no attributed tests.
    Untested,
}

impl StructuralKind {
    /// Stable lowercase-ish token for JSON/human output.
    pub fn as_str(self) -> &'static str {
        match self {
            StructuralKind::Undocumented => "undocumented",
            StructuralKind::DanglingReference => "dangling-reference",
            StructuralKind::Untested => "untested",
        }
    }
}

/// Pure: an entry is *undocumented* when it has implementation files but no
/// authored spec-doc (`None`, or present-but-blank). No filesystem access.
pub fn is_undocumented(spec_doc: Option<&str>, impl_files: &[String]) -> bool {
    let has_spec = spec_doc.map(|s| !s.trim().is_empty()).unwrap_or(false);
    !impl_files.is_empty() && !has_spec
}

/// Pure: an entry is *untested* when it has implementation files but no
/// attributed test files. No filesystem access.
pub fn is_untested(impl_files: &[String], test_files: &[String]) -> bool {
    !impl_files.is_empty() && test_files.is_empty()
}

/// Pure (over an injected existence predicate): the referenced impl/test paths
/// that do NOT exist per `exists`. The predicate is the only impurity, so the
/// classification is unit-testable with a fake set and no disk.
pub fn dangling_refs(entry: &MapEntry, exists: impl Fn(&str) -> bool) -> Vec<String> {
    entry
        .impl_files
        .iter()
        .chain(entry.test_files.iter())
        .filter(|p| !exists(p))
        .cloned()
        .collect()
}

/// Pure (over an injected existence predicate): all structural findings for one
/// entry, in a stable order (undocumented → dangling → untested).
pub fn structural_findings(
    entry: &MapEntry,
    exists: impl Fn(&str) -> bool,
) -> Vec<StructuralFinding> {
    let mut out = Vec::new();
    if is_undocumented(entry.spec_doc.as_deref(), &entry.impl_files) {
        out.push(StructuralFinding {
            key: entry.key.clone(),
            kind: StructuralKind::Undocumented,
            detail: format!(
                "{} 実装ファイルがあるが spec_doc が未記載",
                entry.impl_files.len()
            ),
        });
    }
    let dangling = dangling_refs(entry, &exists);
    if !dangling.is_empty() {
        out.push(StructuralFinding {
            key: entry.key.clone(),
            kind: StructuralKind::DanglingReference,
            detail: format!("存在しない参照: {}", dangling.join(", ")),
        });
    }
    if is_untested(&entry.impl_files, &entry.test_files) {
        out.push(StructuralFinding {
            key: entry.key.clone(),
            kind: StructuralKind::Untested,
            detail: format!("{} 実装ファイルに対しテストなし", entry.impl_files.len()),
        });
    }
    out
}

/// Thin FS wrapper: structural findings for every entry in the map matching
/// `filter` (see [`entry_matches`]; an empty filter matches all), using a real
/// on-disk existence check rooted at `repo_root` (repo-relative paths). Pure
/// classification is delegated to [`structural_findings`].
pub fn scan_map_filtered(map: &SpecMap, repo_root: &Path, filter: &str) -> Vec<StructuralFinding> {
    let exists = |p: &str| repo_root.join(p).exists();
    map.entries
        .values()
        .filter(|e| entry_matches(e, filter))
        .flat_map(|e| structural_findings(e, exists))
        .collect()
}

/// Pure: does `entry` match the (case-insensitive substring) `query`? An
/// empty/blank query matches every entry (the whole-map default). Otherwise the
/// query is matched against the entry key, its spec_doc, any impl/test file
/// path, and — for endpoint entries — the api route. This lets a user scope the
/// audit to a specific command/crate/API by path or route (e.g. `drift-map`,
/// `crates/specguard`, `/health`). No filesystem access.
pub fn entry_matches(entry: &MapEntry, query: &str) -> bool {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return true;
    }
    let hay = |s: &str| s.to_lowercase().contains(&q);
    hay(&entry.key)
        || entry.spec_doc.as_deref().is_some_and(hay)
        || entry.impl_files.iter().any(|p| hay(p))
        || entry.test_files.iter().any(|p| hay(p))
        || entry.api.as_ref().is_some_and(|a| hay(&a.route))
}

/// An entry is worth auditing when there is something to judge the correctness of
/// — an authored spec-doc, or attributed impl/test files. A wholly orphaned entry
/// (no spec, no files) is skipped.
pub fn is_auditable(entry: &MapEntry) -> bool {
    let has_spec = entry
        .spec_doc
        .as_deref()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    has_spec || !entry.impl_files.is_empty() || !entry.test_files.is_empty()
}

/// Machine-readable envelope for `audit --json`: the SAME shape as the drift
/// `prompt --json` envelope (`{project, baseline, head, date, marker, shards}`),
/// so the command layer can hand each shard to a read-only subagent and feed the
/// outputs straight back through `specguard ingest` (which matches by `label`).
#[derive(Debug, Serialize)]
pub struct AuditEnvelope {
    pub project: String,
    pub baseline: String,
    pub head: String,
    pub date: String,
    /// The marker each shard's report must end with (identical to the drift
    /// marker, so `ingest`'s parser works unchanged).
    pub marker: String,
    /// The applied targeting filter (see [`entry_matches`]), so the caller can
    /// see the audited scope. Omitted from the JSON when empty (whole-map), so
    /// an unfiltered envelope keeps the exact drift `prompt --json` shape.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub filter: String,
    pub shards: Vec<AuditShard>,
}

/// One audit shard: a label (the map entry key) + the rendered prompt. Mirrors
/// the drift `ShardJson` shape exactly.
#[derive(Debug, Serialize)]
pub struct AuditShard {
    pub label: String,
    pub prompt: String,
}

/// Render one entry's structural signals as a markdown bullet list for the shard
/// (or an explicit "none" note so the auditor still evaluates independently).
fn signals_block(findings: &[StructuralFinding]) -> String {
    if findings.is_empty() {
        return "- (構造シグナルなし — 独自に (a)/(b)/(c) を評価すること)\n".to_string();
    }
    findings
        .iter()
        .map(|f| format!("- **{}**: {}\n", f.kind.as_str(), f.detail))
        .collect()
}

/// Render one entry's file list as a markdown bullet list (or a "none" note).
fn files_block(files: &[String]) -> String {
    if files.is_empty() {
        return "  - (なし)\n".to_string();
    }
    files.iter().map(|f| format!("  - `{f}`\n")).collect()
}

/// Display value for the spec_doc placeholder.
fn spec_doc_display(entry: &MapEntry) -> String {
    match entry.spec_doc.as_deref() {
        Some(s) if !s.trim().is_empty() => format!("`{}`", s.trim()),
        _ => "(未記載)".to_string(),
    }
}

/// Render a single entry's correctness-audit shard prompt from `template`,
/// substituting the project/date/marker and the entry's spec↔impl↔test triple
/// plus its pre-computed structural signals.
pub fn render_audit_shard(
    template: &str,
    cfg: &Config,
    entry: &MapEntry,
    findings: &[StructuralFinding],
    date: &str,
) -> String {
    template
        .replace("{{PROJECT_NAME}}", &cfg.project.name)
        .replace("{{DATE}}", date)
        .replace("{{MARKER}}", MARKER)
        .replace("{{FEATURE_KEY}}", &entry.key)
        .replace("{{SPEC_DOC}}", &spec_doc_display(entry))
        .replace("{{IMPL_FILES}}", &files_block(&entry.impl_files))
        .replace("{{TEST_FILES}}", &files_block(&entry.test_files))
        .replace("{{STRUCTURAL_SIGNALS}}", &signals_block(findings))
}

/// Assemble the audit envelope from the map: one shard per auditable entry that
/// also matches `filter` (see [`entry_matches`]; empty filter = whole map),
/// bounded by [`MAX_AUDIT_SHARDS`] in stable key order, each carrying its own
/// deterministic structural signals (computed with an on-disk existence check
/// rooted at `repo_root`). The result is `prompt --json`-shaped and
/// ingest-compatible.
pub fn build_envelope(
    cfg: &Config,
    map: &SpecMap,
    repo_root: &Path,
    baseline: &str,
    head: &str,
    date: &str,
    filter: &str,
) -> AuditEnvelope {
    debug_assert!(
        AUDIT_PLACEHOLDERS
            .iter()
            .all(|p| DEFAULT_AUDIT_TEMPLATE.contains(p)),
        "spec-audit template is missing a required placeholder"
    );
    let exists = |p: &str| repo_root.join(p).exists();
    let in_scope = |e: &&MapEntry| is_auditable(e) && entry_matches(e, filter);
    let total_auditable = map.entries.values().filter(in_scope).count();
    let shards: Vec<AuditShard> = map
        .entries
        .values()
        .filter(in_scope)
        .take(MAX_AUDIT_SHARDS)
        .map(|e| {
            let findings = structural_findings(e, exists);
            AuditShard {
                label: e.key.clone(),
                prompt: render_audit_shard(DEFAULT_AUDIT_TEMPLATE, cfg, e, &findings, date),
            }
        })
        .collect();
    if total_auditable > MAX_AUDIT_SHARDS {
        eprintln!(
            "specguard audit: auditable entries truncated to {MAX_AUDIT_SHARDS} (total {total_auditable}; {} omitted this run)",
            total_auditable - MAX_AUDIT_SHARDS
        );
    }
    AuditEnvelope {
        project: cfg.project.name.clone(),
        baseline: baseline.to_string(),
        head: head.to_string(),
        date: date.to_string(),
        marker: MARKER.to_string(),
        filter: filter.trim().to_string(),
        shards,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::specmap::{EntryKind, Status};

    fn entry(
        key: &str,
        spec_doc: Option<&str>,
        impl_files: &[&str],
        test_files: &[&str],
    ) -> MapEntry {
        MapEntry {
            key: key.to_string(),
            kind: EntryKind::Feature,
            spec_doc: spec_doc.map(|s| s.to_string()),
            status: Status::Tracked,
            last_ref: None,
            impl_files: impl_files.iter().map(|s| s.to_string()).collect(),
            test_files: test_files.iter().map(|s| s.to_string()).collect(),
            client_refs: vec![],
            api: None,
        }
    }

    fn cfg() -> Config {
        toml::from_str(
            r#"
            [project]
            name = "Demo"
            [[area]]
            name = "a"
            globs = ["src/**"]
            "#,
        )
        .unwrap()
    }

    // -- pure structural classification -------------------------------------

    #[test]
    fn undocumented_when_impl_present_and_no_spec() {
        assert!(is_undocumented(None, &["src/x.rs".to_string()]));
        assert!(is_undocumented(Some("   "), &["src/x.rs".to_string()]));
        // Documented: has a spec-doc.
        assert!(!is_undocumented(
            Some("docs/x.md"),
            &["src/x.rs".to_string()]
        ));
        // No impl files: nothing to be undocumented about.
        assert!(!is_undocumented(None, &[]));
    }

    #[test]
    fn untested_when_impl_present_and_no_tests() {
        assert!(is_untested(&["src/x.rs".to_string()], &[]));
        // Has tests → not untested.
        assert!(!is_untested(
            &["src/x.rs".to_string()],
            &["tests/x.rs".to_string()]
        ));
        // No impl → not classified untested.
        assert!(!is_untested(&[], &[]));
    }

    #[test]
    fn dangling_refs_reported_for_missing_paths_via_injected_predicate() {
        let e = entry(
            "feat",
            Some("docs/feat.md"),
            &["src/present.rs", "src/gone.rs"],
            &["tests/present_test.rs"],
        );
        // Only src/present.rs and the test exist; src/gone.rs is dangling.
        let exists = |p: &str| p == "src/present.rs" || p == "tests/present_test.rs";
        let dangling = dangling_refs(&e, exists);
        assert_eq!(dangling, vec!["src/gone.rs".to_string()]);
    }

    #[test]
    fn structural_findings_orders_undocumented_dangling_untested() {
        // impl present, no spec (undocumented), one dangling impl path, no tests
        // (untested) → all three, in the fixed order.
        let e = entry("feat", None, &["src/a.rs", "src/missing.rs"], &[]);
        let exists = |p: &str| p == "src/a.rs";
        let findings = structural_findings(&e, exists);
        let kinds: Vec<StructuralKind> = findings.iter().map(|f| f.kind).collect();
        assert_eq!(
            kinds,
            vec![
                StructuralKind::Undocumented,
                StructuralKind::DanglingReference,
                StructuralKind::Untested,
            ]
        );
        // The dangling detail names the missing path.
        assert!(findings[1].detail.contains("src/missing.rs"));
    }

    #[test]
    fn clean_entry_has_no_structural_findings() {
        let e = entry(
            "feat",
            Some("docs/feat.md"),
            &["src/a.rs"],
            &["tests/a_test.rs"],
        );
        let exists = |_: &str| true; // everything exists on disk
        assert!(structural_findings(&e, exists).is_empty());
    }

    // -- envelope / shard assembly ------------------------------------------

    #[test]
    fn build_envelope_shape_and_labels_match_entries() {
        let tmp = tempfile::tempdir().unwrap();
        // Create the impl file so it is NOT flagged dangling; leave the test file
        // absent so the shard also carries a dangling signal we can assert on.
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("src/login.rs"), "fn main() {}").unwrap();

        let mut map = SpecMap::default();
        map.entries.insert(
            "login".to_string(),
            entry(
                "login",
                Some("docs/login.md"),
                &["src/login.rs"],
                &["tests/gone_test.rs"],
            ),
        );
        // An orphaned entry (no spec, no files) must be skipped as non-auditable.
        map.entries
            .insert("orphan".to_string(), entry("orphan", None, &[], &[]));

        let env = build_envelope(
            &cfg(),
            &map,
            tmp.path(),
            "base123",
            "head456",
            "2026-07-08",
            "",
        );

        // Envelope scalars match the drift `prompt --json` shape.
        assert_eq!(env.project, "Demo");
        assert_eq!(env.baseline, "base123");
        assert_eq!(env.head, "head456");
        assert_eq!(env.date, "2026-07-08");
        assert_eq!(env.marker, MARKER);

        // One shard (orphan skipped), labelled by the entry key.
        assert_eq!(env.shards.len(), 1);
        let shard = &env.shards[0];
        assert_eq!(shard.label, "login");
        // The shard prompt is fully rendered (no leftover placeholders) and
        // carries the marker + the entry triple + the dangling structural signal.
        assert!(
            !shard.prompt.contains("{{"),
            "no unsubstituted placeholders"
        );
        assert!(shard.prompt.contains(MARKER));
        assert!(shard.prompt.contains("login"));
        assert!(shard.prompt.contains("docs/login.md"));
        assert!(shard.prompt.contains("dangling-reference"));
        assert!(shard.prompt.contains("tests/gone_test.rs"));
        // Framed as a CORRECTNESS audit, not a drift/consistency check.
        assert!(shard.prompt.contains("正当性"));
    }

    #[test]
    fn envelope_serializes_to_prompt_json_shape() {
        let tmp = tempfile::tempdir().unwrap();
        let mut map = SpecMap::default();
        map.entries.insert(
            "feat".to_string(),
            entry("feat", Some("docs/feat.md"), &["src/feat.rs"], &[]),
        );
        let env = build_envelope(&cfg(), &map, tmp.path(), "b", "h", "2026-07-08", "");
        let json = serde_json::to_value(&env).unwrap();
        // No filter → the `filter` key is omitted, preserving the exact drift shape.
        assert!(json.get("filter").is_none());
        // Same top-level keys the drift envelope emits.
        for k in ["project", "baseline", "head", "date", "marker", "shards"] {
            assert!(json.get(k).is_some(), "envelope missing key {k}");
        }
        let shard0 = &json["shards"][0];
        assert!(shard0.get("label").is_some());
        assert!(shard0.get("prompt").is_some());
    }

    // -- targeted filter (entry_matches) ------------------------------------

    #[test]
    fn entry_matches_on_key_spec_paths_route_case_insensitive() {
        let mut e = entry(
            "drift-map",
            Some("docs/specs/DriftMap.md"),
            &["crates/specguard/src/drift.rs"],
            &["crates/specguard/tests/drift_test.rs"],
        );
        // Matches on the key (case-insensitive).
        assert!(entry_matches(&e, "drift-map"));
        assert!(entry_matches(&e, "DRIFT-MAP"));
        // Matches on the spec_doc.
        assert!(entry_matches(&e, "driftmap.md"));
        // Matches on an impl path fragment.
        assert!(entry_matches(&e, "crates/specguard"));
        // Matches on a test path fragment.
        assert!(entry_matches(&e, "drift_test"));
        // Non-match.
        assert!(!entry_matches(&e, "unrelated-feature"));

        // Matches on the api route for an endpoint entry.
        e.api = Some(crate::specmap::ApiRef {
            method: "GET".to_string(),
            route: "/api/health".to_string(),
        });
        assert!(entry_matches(&e, "/health"));
    }

    #[test]
    fn entry_matches_empty_query_matches_all() {
        let e = entry("anything", None, &["src/x.rs"], &[]);
        assert!(entry_matches(&e, ""));
        assert!(entry_matches(&e, "   "));
    }

    #[test]
    fn build_envelope_filter_restricts_to_matching_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let mut map = SpecMap::default();
        map.entries.insert(
            "login".to_string(),
            entry("login", Some("docs/login.md"), &["src/login.rs"], &[]),
        );
        map.entries.insert(
            "logout".to_string(),
            entry("logout", Some("docs/logout.md"), &["src/logout.rs"], &[]),
        );
        // Filter to just "login" → one shard, and the envelope records the filter.
        let env = build_envelope(&cfg(), &map, tmp.path(), "b", "h", "2026-07-08", "login");
        assert_eq!(env.shards.len(), 1);
        assert_eq!(env.shards[0].label, "login");
        assert_eq!(env.filter, "login");
        // The filter surfaces in the serialized JSON when non-empty.
        let json = serde_json::to_value(&env).unwrap();
        assert_eq!(json["filter"], "login");
    }

    #[test]
    fn template_contains_all_required_placeholders() {
        for p in AUDIT_PLACEHOLDERS {
            assert!(
                DEFAULT_AUDIT_TEMPLATE.contains(p),
                "template missing placeholder {p}"
            );
        }
    }

    #[test]
    fn scan_map_filtered_uses_real_disk_existence() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("src/there.rs"), "x").unwrap();
        let mut map = SpecMap::default();
        map.entries.insert(
            "feat".to_string(),
            entry(
                "feat",
                Some("docs/feat.md"),
                &["src/there.rs", "src/nowhere.rs"],
                &["tests/t.rs"],
            ),
        );
        let findings = scan_map_filtered(&map, tmp.path(), "");
        // src/nowhere.rs and tests/t.rs are dangling; there.rs is not.
        let dangling: Vec<&StructuralFinding> = findings
            .iter()
            .filter(|f| f.kind == StructuralKind::DanglingReference)
            .collect();
        assert_eq!(dangling.len(), 1);
        assert!(dangling[0].detail.contains("src/nowhere.rs"));
        assert!(dangling[0].detail.contains("tests/t.rs"));
        assert!(!dangling[0].detail.contains("src/there.rs"));
    }
}
