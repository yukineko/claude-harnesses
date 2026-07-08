//! Render the audit prompt from the template + resolved scope.
//!
//! The template is data, not code: by default the embedded
//! `templates/audit-prompt.md` is used, but a project can point
//! `[prompt].template` at its own file. Project specifics (areas, canon
//! pointers, invariants) are injected as placeholders so the prompt stays
//! generic while the canon itself is never copied in — the agent always reads
//! the live canon files, which keeps the prompt from drifting against them.

use crate::config::Config;
use crate::parse::MARKER;
use crate::scope::{AreaHit, Scope};

/// The embedded default template. Override via `[prompt].template`.
pub const DEFAULT_TEMPLATE: &str = include_str!("../templates/audit-prompt.md");

/// The embedded D3 (decision freshness/obsolescence) template.
pub const DECISIONS_TEMPLATE: &str = include_str!("../templates/decisions-prompt.md");

/// The embedded V1 (adversarial verification / refute) template.
pub const REFUTE_TEMPLATE: &str = include_str!("../templates/refute-prompt.md");

/// The embedded V2 (completeness critique) template.
pub const COMPLETENESS_TEMPLATE: &str = include_str!("../templates/completeness-prompt.md");

/// The pre-task spec-briefing template (read-only; prevents drift before coding).
/// Advisory — it produces no findings/sentinel, so it is NOT part of the
/// ratification (meta-canon) surface.
pub const BRIEF_TEMPLATE: &str = include_str!("../templates/brief-prompt.md");

/// Maximum number of sample changed files listed per area in the prompt.
const MAX_SAMPLE_FILES: usize = 12;

/// Placeholders the audit (D1/D2) template must contain — the machine contract
/// part of the prompt's meta-canon, deterministically checkable at ratification.
pub const AUDIT_PLACEHOLDERS: &[&str] = &[
    "{{PROJECT_NAME}}",
    "{{DATE}}",
    "{{MARKER}}",
    "{{SCOPE_SUMMARY}}",
    "{{AREAS}}",
    "{{INVARIANTS}}",
];

/// Placeholders the D3 decisions template must contain.
pub const DECISIONS_PLACEHOLDERS: &[&str] = &[
    "{{PROJECT_NAME}}",
    "{{DATE}}",
    "{{MARKER}}",
    "{{DECISIONS}}",
    "{{INSCOPE_CANON}}",
];

/// Placeholders the V1 refute template must contain — contract-checked at
/// ratification when the refute gate is active (DESIGN-VERIFY.md §7).
pub const REFUTE_PLACEHOLDERS: &[&str] = &[
    "{{PROJECT_NAME}}",
    "{{DATE}}",
    "{{MARKER}}",
    "{{CANON}}",
    "{{FINDINGS}}",
];

/// Placeholders the V2 completeness template must contain — contract-checked at
/// ratification when the completeness gate is active (DESIGN-VERIFY.md §7).
pub const COMPLETENESS_PLACEHOLDERS: &[&str] = &[
    "{{PROJECT_NAME}}",
    "{{DATE}}",
    "{{MARKER}}",
    "{{CANON}}",
    "{{SHARD}}",
];

/// Required placeholders missing from `template` — a non-empty result means the
/// template contradicts the parser/render contract (refuse to ratify).
pub fn missing_placeholders(template: &str, required: &[&'static str]) -> Vec<&'static str> {
    required
        .iter()
        .filter(|p| !template.contains(**p))
        .copied()
        .collect()
}

/// Maximum number of decision records listed in the D3 prompt.
const MAX_DECISIONS: usize = 30;

/// The signal an auditor emits in its report body to request a WIDER scope when
/// the bounded relevant-file map (t4 Part A) was insufficient to complete the
/// audit. Detected deterministically ([`signals_insufficient_context`]) so the
/// harness can re-dispatch that one shard exactly once with a widened map
/// (t4 Part B). Reuses the plain-token-in-body idiom of the existing marker
/// mechanism rather than a new trailer field, so `parse.rs` is untouched.
pub const NEEDS_WIDER_SCOPE_SIGNAL: &str = "<<<NEEDS_WIDER_SCOPE>>>";

/// True when a shard's audit output signals insufficient context (it emitted
/// [`NEEDS_WIDER_SCOPE_SIGNAL`]). Untrusted input: this is a fixed-token scan, so
/// audited repo content cannot smuggle any behavior beyond "please widen once".
pub fn signals_insufficient_context(text: &str) -> bool {
    text.contains(NEEDS_WIDER_SCOPE_SIGNAL)
}

/// Render the bounded relevant-file map preamble (t4 Part A): the limited set of
/// files most relevant to this shard, so the auditor reads them FIRST instead of
/// broadly re-scanning the tree, plus the escalation contract (how to request a
/// wider scope). Empty map -> empty string (the caller then renders exactly as
/// today — the fugu-absent / feature-off fallback).
fn relevant_map_block(map: &[String], widened: bool) -> String {
    if map.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    out.push_str("# 参照ファイルマップ (relevant-file map)\n\n");
    if widened {
        out.push_str(
            "このマップは既に **1 段階拡張済み** です (これが最終スコープ。これ以上の自動拡張はありません)。\n",
        );
    } else {
        out.push_str(
            "この shard に最も関連するファイルの **限定リスト** です。まずこの範囲を読み、必要に応じてのみ広げてください:\n",
        );
    }
    for f in map {
        out.push_str(&format!("- `{f}`\n"));
    }
    if !widened {
        out.push_str(&format!(
            "\nこの限定マップでは監査を完了できない (関連ファイルが欠けている) 場合は、レポート本文に\n`{NEEDS_WIDER_SCOPE_SIGNAL}` を単独行で 1 つ含めて、より広いスコープを要求してください。\nハーネスがこの shard を **1 回だけ** 拡張スコープで再監査します (無限拡張はしません)。\n",
        ));
    }
    out.push('\n');
    out
}

/// Like [`render_shard`], but prepends the bounded relevant-file map preamble so
/// the auditor reads the limited high-signal set first (t4 Part A). An empty
/// `map` (fugu-router absent / feature off) renders byte-for-byte identically to
/// [`render_shard`] — the backward-compatible fallback. `widened` marks the ONE
/// escalation re-dispatch (t4 Part B) so the preamble states it is final.
pub fn render_shard_with_map(
    template: &str,
    cfg: &Config,
    scope: &Scope,
    shard: Shard,
    date: &str,
    map: &[String],
    widened: bool,
) -> String {
    let base = render_shard(template, cfg, scope, shard, date);
    let block = relevant_map_block(map, widened);
    if block.is_empty() {
        base
    } else {
        format!("{block}{base}")
    }
}

/// One audit shard: a single in-scope area (index into `scope.in_scope`), the
/// invariant set, or the decision-record audit (D3). Each shard is rendered into
/// its own focused prompt and audited by a separate agent process (fresh
/// context).
#[derive(Debug, Clone, Copy)]
pub enum Shard {
    Area(usize),
    Invariants,
    Decisions,
}

/// Build the shard list for a run: one per in-scope area, plus an invariant
/// shard when any invariants are defined, plus a decisions (D3) shard when any
/// decision records exist.
pub fn shards(cfg: &Config, scope: &Scope) -> Vec<Shard> {
    let mut v: Vec<Shard> = (0..scope.in_scope.len()).map(Shard::Area).collect();
    if !crate::scope::invariants_in_scope(cfg, scope).is_empty() {
        v.push(Shard::Invariants);
    }
    if !scope.decision_files.is_empty() {
        v.push(Shard::Decisions);
    }
    v
}

/// Human label for a shard (area name, "invariants", or "decisions").
pub fn shard_label(cfg: &Config, scope: &Scope, shard: Shard) -> String {
    match shard {
        Shard::Area(i) => cfg.areas[scope.in_scope[i].area_index].name.clone(),
        Shard::Invariants => "invariants".to_string(),
        Shard::Decisions => "decisions".to_string(),
    }
}

/// Render a single shard's focused prompt. An area shard sees only that area's
/// canon + changed files (invariants deferred to their own shard); the invariant
/// shard sees only the invariants. This keeps each agent context small and
/// homogeneous, mitigating context rot on multi-area runs.
pub fn render_shard(
    template: &str,
    cfg: &Config,
    scope: &Scope,
    shard: Shard,
    date: &str,
) -> String {
    // The decisions (D3) shard uses its own embedded template, not the D1/D2 one.
    if let Shard::Decisions = shard {
        return render_decisions(cfg, scope, date);
    }
    let (areas, invariants, summary) = match shard {
        Shard::Area(i) => {
            let hit = &scope.in_scope[i];
            (
                area_block_one(cfg, hit),
                "(この shard では不変条件を扱わない — 不変条件は別 shard で照合する)\n".to_string(),
                shard_scope_summary(cfg, scope, Some(hit)),
            )
        }
        Shard::Invariants => (
            "(この shard は不変条件のみを照合する。D1 領域監査は別 shard で実施する。)\n"
                .to_string(),
            invariants_block_scoped(cfg, scope),
            shard_scope_summary(cfg, scope, None),
        ),
        Shard::Decisions => unreachable!("handled above"),
    };
    template
        .replace("{{PROJECT_NAME}}", &cfg.project.name)
        .replace("{{DATE}}", date)
        .replace("{{MARKER}}", MARKER)
        .replace("{{SCOPE_SUMMARY}}", &summary)
        .replace("{{AREAS}}", &areas)
        .replace("{{INVARIANTS}}", &invariants)
}

/// Render the D3 decisions prompt: list the decision records to audit and the
/// in-scope canon to cross-check them against. Judgment (read each record's live
/// content, check freshness + obsolescence) is the agent's job.
fn render_decisions(cfg: &Config, scope: &Scope, date: &str) -> String {
    let mut decisions = String::new();
    for f in scope.decision_files.iter().take(MAX_DECISIONS) {
        decisions.push_str(&format!("- `{f}`\n"));
    }
    if scope.decision_files.len() > MAX_DECISIONS {
        eprintln!(
            "specguard: decision files truncated to {MAX_DECISIONS} (total {}; {} omitted)",
            scope.decision_files.len(),
            scope.decision_files.len() - MAX_DECISIONS
        );
        decisions.push_str(&format!(
            "- … ほか {} 件 (このランでは未掲載)\n",
            scope.decision_files.len() - MAX_DECISIONS
        ));
    }

    // In-scope canon pointers (area canon + invariant canon) for cross-reference.
    let mut canon: Vec<String> = Vec::new();
    for hit in &scope.in_scope {
        for c in &cfg.areas[hit.area_index].canon {
            if !canon.contains(c) {
                canon.push(c.clone());
            }
        }
    }
    for inv in &cfg.invariants {
        for c in &inv.canon {
            if !canon.contains(c) {
                canon.push(c.clone());
            }
        }
    }
    let inscope_canon = if canon.is_empty() {
        "(in-scope の canon なし — 全 decision について「理由が今も成立するか」を中心に確認)\n"
            .to_string()
    } else {
        canon
            .iter()
            .map(|c| format!("- `{c}`\n"))
            .collect::<String>()
    };

    DECISIONS_TEMPLATE
        .replace("{{PROJECT_NAME}}", &cfg.project.name)
        .replace("{{DATE}}", date)
        .replace("{{MARKER}}", MARKER)
        .replace("{{DECISIONS}}", &decisions)
        .replace("{{INSCOPE_CANON}}", &inscope_canon)
}

/// Canon pointers backing a shard, as a markdown bullet list (pointers only —
/// the content is never copied; the verifying agent reads the live canon). Used
/// by the verification gates (refute / completeness) which need the same canon a
/// shard was audited against. An area shard carries its area's canon; the
/// invariant shard the union of invariant canon; the decisions shard the
/// in-scope canon it cross-references.
fn shard_canon_block(cfg: &Config, scope: &Scope, shard: Shard) -> String {
    let mut canon: Vec<String> = Vec::new();
    let mut push = |c: &String| {
        if !canon.contains(c) {
            canon.push(c.clone());
        }
    };
    match shard {
        Shard::Area(i) => {
            for c in &cfg.areas[scope.in_scope[i].area_index].canon {
                push(c);
            }
        }
        Shard::Invariants => {
            for inv in &cfg.invariants {
                for c in &inv.canon {
                    push(c);
                }
            }
        }
        Shard::Decisions => {
            for hit in &scope.in_scope {
                for c in &cfg.areas[hit.area_index].canon {
                    push(c);
                }
            }
            for inv in &cfg.invariants {
                for c in &inv.canon {
                    push(c);
                }
            }
        }
    }
    if canon.is_empty() {
        "- (canon ポインタ指定なし — プロジェクト横断の正典を参照)\n".to_string()
    } else {
        canon.iter().map(|c| format!("- `{c}`\n")).collect()
    }
}

/// Render the V1 refute prompt for one shard: the shard's canon pointers plus the
/// audit's findings body (the agent re-derives each `needs_user=yes` finding and
/// drops only those it can refute with a verbatim quote).
pub fn render_refute(
    cfg: &Config,
    scope: &Scope,
    shard: Shard,
    date: &str,
    findings: &str,
) -> String {
    REFUTE_TEMPLATE
        .replace("{{PROJECT_NAME}}", &cfg.project.name)
        .replace("{{DATE}}", date)
        .replace("{{MARKER}}", MARKER)
        .replace("{{CANON}}", &shard_canon_block(cfg, scope, shard))
        .replace("{{FINDINGS}}", findings.trim())
}

/// Render the V2 completeness prompt for one shard: the shard's canon pointers so
/// the agent can list verifiable rules the sampling audit never matched.
pub fn render_completeness(cfg: &Config, scope: &Scope, shard: Shard, date: &str) -> String {
    COMPLETENESS_TEMPLATE
        .replace("{{PROJECT_NAME}}", &cfg.project.name)
        .replace("{{DATE}}", date)
        .replace("{{MARKER}}", MARKER)
        .replace("{{CANON}}", &shard_canon_block(cfg, scope, shard))
        .replace("{{SHARD}}", &shard_label(cfg, scope, shard))
}

/// Scope summary for a single shard: the overall baseline, but a target scoped
/// to just this shard so the agent is told exactly what it (and only it) owns.
fn shard_scope_summary(cfg: &Config, scope: &Scope, hit: Option<&AreaHit>) -> String {
    let mut s = String::new();
    s.push_str(&format!("- baseline ref: `{}`\n", scope.baseline));
    if scope.fell_back {
        s.push_str(
            "  - 注意: 設定された baseline が解決できず fallback を使用した (レポートに明記すること)\n",
        );
    }
    s.push_str(&format!(
        "- 変更ファイル数 (リポジトリ全体): {}\n",
        scope.changed_files.len()
    ));
    match hit {
        Some(hit) => {
            let canon_note = if hit.changed_canon.is_empty() {
                String::new()
            } else {
                format!(", canon 変更 {} 件", hit.changed_canon.len())
            };
            s.push_str(&format!(
                "- この shard の監査対象: 領域「{}」(実装変更 {} 件{})\n",
                cfg.areas[hit.area_index].name,
                hit.matched_files.len(),
                canon_note
            ));
        }
        None => s.push_str(&format!(
            "- この shard の監査対象: 不変条件 {} 件 (変更の有無に関わらず毎回)\n",
            cfg.invariants.len()
        )),
    }
    s.push_str(
        "- 注記: 他の領域・不変条件は別プロセス (fresh context) で監査される。本 shard はこの対象だけに集中すること。\n",
    );
    s.push_str(
        "\nこの shard が監査した対象は、レポートのスコープ欄に必ず明記すること (網羅偽装の防止)。\n",
    );
    s
}

/// Render the D1 block for a single area (its canon pointers + changed files).
fn area_block_one(cfg: &Config, hit: &AreaHit) -> String {
    let area = &cfg.areas[hit.area_index];
    let mut out = String::new();
    out.push_str(&format!("### 領域: {}\n", area.name));
    out.push_str("参照すべき正典 (ポインタ。中身でなく「どこを読むか」):\n");
    if area.canon.is_empty() {
        out.push_str("- (このエリアに canon 指定なし — プロジェクト横断の正典を参照)\n");
    } else {
        for c in &area.canon {
            out.push_str(&format!("- `{c}`\n"));
        }
    }
    out.push_str("変更ファイル (この領域の実装):\n");
    if hit.matched_files.is_empty() {
        out.push_str("- (実装側の変更なし)\n");
    }
    // Per-shard character budget (approx-token proxy; `0` = disabled, the
    // default, preserving prior unbounded-by-budget behavior). Even when
    // disabled, the MAX_SAMPLE_FILES cap above still applies.
    let budget = cfg.prompt.max_shard_chars;
    let mut included = 0usize;
    for f in hit.matched_files.iter().take(MAX_SAMPLE_FILES) {
        let line = format!("- `{f}`\n");
        if budget > 0 && included > 0 && out.len() + line.len() > budget {
            // Budget reached: stop before this file, but always keep at
            // least one file line so the shard is never emptied outright.
            break;
        }
        out.push_str(&line);
        included += 1;
    }
    let omitted = hit.matched_files.len() - included;
    if omitted > 0 {
        eprintln!(
            "specguard: matched files for area '{}' truncated to {included} (total {}; {omitted} omitted)",
            cfg.areas[hit.area_index].name,
            hit.matched_files.len(),
        );
        out.push_str(&format!("- … ほか {omitted} 件\n"));
    }
    if !hit.changed_canon.is_empty() {
        out.push_str(
            "**この領域の canon (仕様) が変更された** — 実装がこの変更に追従しているか D1 で確認すること:\n",
        );
        for f in &hit.changed_canon {
            out.push_str(&format!("- `{f}`\n"));
        }
    }
    out.push('\n');
    out
}

/// Render the pre-task spec briefing. Unlike an audit shard there is no git
/// scope: a brief lists EVERY configured area (with its canon pointers) plus all
/// invariants, and the agent routes from the task text to the relevant ones.
pub fn render_brief(template: &str, cfg: &Config, task: &str, date: &str) -> String {
    template
        .replace("{{PROJECT_NAME}}", &cfg.project.name)
        .replace("{{DATE}}", date)
        .replace("{{TASK}}", task.trim())
        .replace("{{AREAS}}", &brief_areas_block(cfg))
        .replace("{{INVARIANTS}}", &invariants_block(cfg))
}

/// Every configured area as a markdown block: name, impl globs, and canon
/// pointers (pointers only — never the content). No scope/changed files (a brief
/// runs before any change exists).
fn brief_areas_block(cfg: &Config) -> String {
    if cfg.areas.is_empty() {
        return "(領域の定義なし)\n".to_string();
    }
    let mut out = String::new();
    for area in &cfg.areas {
        out.push_str(&format!("### 領域: {}\n", area.name));
        if !area.globs.is_empty() {
            let g: Vec<String> = area.globs.iter().map(|s| format!("`{s}`")).collect();
            out.push_str(&format!("- 実装範囲 (glob): {}\n", g.join(", ")));
        }
        if area.canon.is_empty() {
            out.push_str("- 正典: (指定なし — プロジェクト横断の正典を参照)\n");
        } else {
            let c: Vec<String> = area.canon.iter().map(|s| format!("`{s}`")).collect();
            out.push_str(&format!("- 正典: {}\n", c.join(", ")));
        }
    }
    out
}

/// Render every configured invariant, unconditionally. Used only by
/// [`render_brief`], which has no git scope (it runs before any change
/// exists) and intentionally lists ALL invariants regardless of `always`.
fn invariants_block(cfg: &Config) -> String {
    render_invariants_list(cfg.invariants.iter().collect())
}

/// Diff-scope-aware invariant rendering for the audit's invariants shard:
/// only invariants [`crate::scope::invariants_in_scope`] considers in scope
/// (`always` ones unconditionally, plus non-`always` ones whose canon the
/// diff touched) are rendered. With the default `always = true`, this
/// renders identically to [`invariants_block`] (full backward compatibility).
fn invariants_block_scoped(cfg: &Config, scope: &Scope) -> String {
    render_invariants_list(crate::scope::invariants_in_scope(cfg, scope))
}

/// Shared renderer for a subset of invariants: `- **name**: desc` plus an
/// optional `  - 正典: ...` line, or the "no invariants" placeholder when the
/// subset is empty.
fn render_invariants_list(invariants: Vec<&crate::config::Invariant>) -> String {
    if invariants.is_empty() {
        return "(不変条件の定義なし)\n".to_string();
    }
    let mut out = String::new();
    for inv in invariants {
        out.push_str(&format!("- **{}**", inv.name));
        if !inv.description.trim().is_empty() {
            out.push_str(&format!(": {}", inv.description));
        }
        out.push('\n');
        if !inv.canon.is_empty() {
            let pointers: Vec<String> = inv.canon.iter().map(|c| format!("`{c}`")).collect();
            out.push_str(&format!("  - 正典: {}\n", pointers.join(", ")));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scope::{AreaHit, Scope};

    fn sample_cfg() -> Config {
        toml::from_str(
            r#"
            [project]
            name = "Demo"

            [[area]]
            name = "logging"
            globs = ["logging/**"]
            canon = ["logging/SPEC.md", "docs/signing.md"]

            [[invariant]]
            name = "signing"
            description = "all signing via signature.py"
            canon = ["docs/signing.md"]
            "#,
        )
        .unwrap()
    }

    fn sample_scope() -> Scope {
        Scope {
            baseline: "abc123".into(),
            fell_back: false,
            changed_files: vec!["logging/sig.py".into()],
            in_scope: vec![AreaHit {
                area_index: 0,
                matched_files: vec!["logging/sig.py".into()],
                changed_canon: vec![],
            }],
            skipped_areas: vec![],
            decision_files: vec![],
        }
    }

    #[test]
    fn shards_are_one_per_area_plus_invariants() {
        let cfg = sample_cfg();
        let scope = sample_scope();
        let s = shards(&cfg, &scope);
        assert_eq!(s.len(), 2); // one area + one invariant shard (no decisions)
        assert_eq!(shard_label(&cfg, &scope, s[0]), "logging");
        assert_eq!(shard_label(&cfg, &scope, s[1]), "invariants");
    }

    /// A synthetic large area shard (many changed files) that would exceed a
    /// configured per-shard character budget has its file list truncated with
    /// an explicit "N files omitted" note, mirroring the MAX_DECISIONS /
    /// MAX_SAMPLE_FILES truncation idiom.
    #[test]
    fn area_shard_over_budget_truncates_file_list_with_omitted_note() {
        let mut cfg = sample_cfg();
        cfg.prompt.max_shard_chars = 200; // small budget, well under the full list
        let mut scope = sample_scope();
        scope.in_scope[0].matched_files = (0..50)
            .map(|i| format!("logging/very/long/nested/path/to/file_{i:03}.py"))
            .collect();
        let out = render_shard(DEFAULT_TEMPLATE, &cfg, &scope, Shard::Area(0), "2026-06-17");
        // Not every file made it in — the budget forced a cut.
        assert!(
            !out.contains("file_049.py"),
            "budget should have truncated before the last file"
        );
        // ...and the cut left an explicit omitted-count note behind.
        assert!(
            out.contains("ほか") && out.contains("件"),
            "expected an explicit omitted-files note, got: {out}"
        );
        // At least one file was still kept (the shard is never emptied).
        assert!(out.contains("file_000.py"), "keeps at least one file");
        assert!(!out.contains("{{"));
    }

    /// A small area shard whose rendered content stays under a configured
    /// budget is unaffected — every changed file is still listed and no
    /// omitted-files note is emitted.
    #[test]
    fn area_shard_under_budget_is_unchanged() {
        let mut cfg = sample_cfg();
        cfg.prompt.max_shard_chars = 100_000; // generous budget, nothing near it
        let scope = sample_scope();
        let out = render_shard(DEFAULT_TEMPLATE, &cfg, &scope, Shard::Area(0), "2026-06-17");
        assert!(out.contains("logging/sig.py"));
        assert!(!out.contains("ほか"), "no file should be omitted");
        assert!(!out.contains("{{"));

        // The default (budget disabled) renders byte-for-byte the same as the
        // explicit generous budget, for inputs under the sample-file cap.
        let default_cfg = sample_cfg();
        assert_eq!(default_cfg.prompt.max_shard_chars, 0);
        let out_default = render_shard(
            DEFAULT_TEMPLATE,
            &default_cfg,
            &scope,
            Shard::Area(0),
            "2026-06-17",
        );
        assert_eq!(out, out_default);
    }

    #[test]
    fn decisions_shard_added_when_records_exist() {
        let cfg = sample_cfg();
        let mut scope = sample_scope();
        scope.decision_files = vec!["/vault/decisions/2026-06-17-x.md".into()];
        let s = shards(&cfg, &scope);
        assert_eq!(s.len(), 3);
        assert_eq!(shard_label(&cfg, &scope, s[2]), "decisions");

        let out = render_shard(
            DECISIONS_TEMPLATE,
            &cfg,
            &scope,
            Shard::Decisions,
            "2026-06-17",
        );
        assert!(out.contains("2026-06-17-x.md"), "lists the decision record");
        assert!(
            out.contains("logging/SPEC.md"),
            "lists in-scope canon to cross-check"
        );
        assert!(out.contains(MARKER));
        assert!(!out.contains("{{"));
    }

    #[test]
    fn area_shard_flags_canon_change() {
        let cfg = sample_cfg();
        let mut scope = sample_scope();
        scope.in_scope[0].matched_files = vec![];
        scope.in_scope[0].changed_canon = vec!["docs/signing.md".into()];
        let out = render_shard(DEFAULT_TEMPLATE, &cfg, &scope, Shard::Area(0), "2026-06-17");
        assert!(out.contains("canon (仕様) が変更された"));
        assert!(out.contains("docs/signing.md"));
        assert!(!out.contains("{{"));
    }

    #[test]
    fn area_shard_fills_placeholders_and_defers_invariants() {
        let cfg = sample_cfg();
        let scope = sample_scope();
        let out = render_shard(DEFAULT_TEMPLATE, &cfg, &scope, Shard::Area(0), "2026-06-17");
        assert!(out.contains("Demo"));
        assert!(out.contains("2026-06-17"));
        assert!(out.contains("abc123"));
        assert!(out.contains("logging/SPEC.md")); // the area's canon
        assert!(out.contains(MARKER));
        // The invariant body belongs to a different shard, not this one.
        assert!(!out.contains("all signing via signature.py"));
        assert!(out.contains("不変条件を扱わない"));
        // No unsubstituted placeholders remain.
        assert!(!out.contains("{{"));
    }

    #[test]
    fn refute_prompt_carries_canon_findings_and_no_placeholders() {
        let cfg = sample_cfg();
        let scope = sample_scope();
        let out = render_refute(
            &cfg,
            &scope,
            Shard::Area(0),
            "2026-06-17",
            "| rule | quote | verdict 矛盾 | needs_user yes |",
        );
        assert!(out.contains("反証監査"), "is the refute prompt");
        assert!(out.contains("logging/SPEC.md"), "the area's canon pointer");
        assert!(out.contains("verdict 矛盾"), "the audit findings injected");
        assert!(out.contains(MARKER));
        assert!(!out.contains("{{"));
        // Contract: every required placeholder was substituted.
        assert!(missing_placeholders(REFUTE_TEMPLATE, REFUTE_PLACEHOLDERS).is_empty());
    }

    #[test]
    fn completeness_prompt_carries_canon_and_shard_label() {
        let cfg = sample_cfg();
        let scope = sample_scope();
        let out = render_completeness(&cfg, &scope, Shard::Area(0), "2026-06-17");
        assert!(out.contains("網羅性批評"), "is the completeness prompt");
        assert!(out.contains("logging"), "names the shard");
        assert!(out.contains("logging/SPEC.md"), "the area's canon pointer");
        assert!(out.contains(MARKER));
        assert!(!out.contains("{{"));
        assert!(missing_placeholders(COMPLETENESS_TEMPLATE, COMPLETENESS_PLACEHOLDERS).is_empty());
    }

    // -- relevant-file map + escalation signal (t4) -------------------------

    #[test]
    fn signals_insufficient_context_detects_token() {
        assert!(signals_insufficient_context(
            "body\n<<<NEEDS_WIDER_SCOPE>>>\nmore"
        ));
        assert!(!signals_insufficient_context("a normal clean audit body"));
    }

    /// Part A: with a non-empty map, the rendered shard carries a BOUNDED,
    /// PRESENT relevant-file map preamble AND the escalation contract, on top of
    /// the normal shard prompt.
    #[test]
    fn render_shard_with_map_includes_bounded_map_and_signal_instruction() {
        let cfg = sample_cfg();
        let scope = sample_scope();
        let map = vec![
            "logging/SPEC.md".to_string(),
            "logging/sig.py".to_string(),
            "src/related.rs".to_string(),
        ];
        let out = render_shard_with_map(
            DEFAULT_TEMPLATE,
            &cfg,
            &scope,
            Shard::Area(0),
            "2026-06-17",
            &map,
            false,
        );
        assert!(out.contains("参照ファイルマップ"), "map header present");
        for f in &map {
            assert!(out.contains(f.as_str()), "map lists {f}");
        }
        // The escalation contract (how to request a wider scope) is stated.
        assert!(out.contains(NEEDS_WIDER_SCOPE_SIGNAL));
        // The underlying shard prompt is still fully rendered.
        assert!(out.contains(MARKER));
        assert!(!out.contains("{{"));
    }

    /// The widened (escalation) render marks the map as final and omits the
    /// "request wider scope" invitation (no second escalation).
    #[test]
    fn render_shard_with_map_widened_marks_final_and_omits_request() {
        let cfg = sample_cfg();
        let scope = sample_scope();
        let map = vec!["logging/SPEC.md".to_string(), "src/extra.rs".to_string()];
        let out = render_shard_with_map(
            DEFAULT_TEMPLATE,
            &cfg,
            &scope,
            Shard::Area(0),
            "2026-06-17",
            &map,
            true,
        );
        assert!(
            out.contains("拡張済み"),
            "states the map is already widened"
        );
        assert!(
            !out.contains(NEEDS_WIDER_SCOPE_SIGNAL),
            "widened pass must not invite another escalation"
        );
    }

    /// Backward-compatible fallback: an EMPTY map renders byte-for-byte the same
    /// as the plain `render_shard` (fugu-router absent / feature off).
    #[test]
    fn render_shard_with_empty_map_equals_plain_render() {
        let cfg = sample_cfg();
        let scope = sample_scope();
        let plain = render_shard(DEFAULT_TEMPLATE, &cfg, &scope, Shard::Area(0), "2026-06-17");
        let mapped = render_shard_with_map(
            DEFAULT_TEMPLATE,
            &cfg,
            &scope,
            Shard::Area(0),
            "2026-06-17",
            &[],
            false,
        );
        assert_eq!(plain, mapped);
    }

    #[test]
    fn invariant_shard_carries_invariants_not_areas() {
        let cfg = sample_cfg();
        let scope = sample_scope();
        let out = render_shard(
            DEFAULT_TEMPLATE,
            &cfg,
            &scope,
            Shard::Invariants,
            "2026-06-17",
        );
        assert!(out.contains("all signing via signature.py"));
        assert!(out.contains(MARKER));
        // Area canon is audited by the area shard, not here.
        assert!(out.contains("不変条件のみ"));
        assert!(!out.contains("{{"));
    }

    // -- diff-scoped invariant shard (complete 550d154b) --------------------

    /// A config with one `always = true` invariant ("kept") and one
    /// `always = false` invariant ("scoped") whose canon is `docs/scoped.md`.
    fn cfg_with_scoped_invariant() -> Config {
        toml::from_str(
            r#"
            [project]
            name = "Demo"

            [[invariant]]
            name = "kept"
            description = "always active"
            canon = ["docs/kept.md"]
            always = true

            [[invariant]]
            name = "scoped"
            description = "diff-scoped only"
            canon = ["docs/scoped.md"]
            always = false
            "#,
        )
        .unwrap()
    }

    fn scope_with_changed(changed_files: Vec<String>) -> Scope {
        Scope {
            baseline: "abc123".into(),
            fell_back: false,
            changed_files,
            in_scope: vec![],
            skipped_areas: vec![],
            decision_files: vec![],
        }
    }

    /// An `always = false` invariant whose canon the diff did NOT touch is
    /// excluded from both shard emission and rendering.
    #[test]
    fn shards_omits_invariants_shard_when_only_non_always_invariant_untouched() {
        let mut cfg = cfg_with_scoped_invariant();
        cfg.invariants.retain(|i| i.name == "scoped"); // only the non-always one
        let scope = scope_with_changed(vec!["unrelated/file.rs".to_string()]);
        let s = shards(&cfg, &scope);
        assert!(
            !s.iter().any(|sh| matches!(sh, Shard::Invariants)),
            "no invariants shard when the sole invariant is out of scope"
        );
    }

    /// The same `always = false` invariant, but the diff DOES touch its
    /// canon: the invariants shard is emitted, and its scoped rendering
    /// includes the invariant.
    #[test]
    fn shards_includes_invariants_shard_and_renders_it_when_diff_touches_canon() {
        let mut cfg = cfg_with_scoped_invariant();
        cfg.invariants.retain(|i| i.name == "scoped");
        let scope = scope_with_changed(vec!["docs/scoped.md".to_string()]);
        let s = shards(&cfg, &scope);
        assert!(
            s.iter().any(|sh| matches!(sh, Shard::Invariants)),
            "invariants shard must be emitted once its canon is touched"
        );
        let out = invariants_block_scoped(&cfg, &scope);
        assert!(
            out.contains("diff-scoped only"),
            "renders the invariant: {out}"
        );
    }

    /// `always = true` (the default) invariant is included even when the
    /// diff never touches its canon at all — full backward compatibility.
    #[test]
    fn always_true_invariant_included_regardless_of_diff() {
        let mut cfg = cfg_with_scoped_invariant();
        cfg.invariants.retain(|i| i.name == "kept");
        let scope = scope_with_changed(vec!["unrelated/file.rs".to_string()]);
        let s = shards(&cfg, &scope);
        assert!(
            s.iter().any(|sh| matches!(sh, Shard::Invariants)),
            "always=true invariant shard must be emitted regardless of diff"
        );
        let out = invariants_block_scoped(&cfg, &scope);
        assert!(
            out.contains("always active"),
            "renders the invariant: {out}"
        );
    }

    /// Mixed config: one `always = true` + one `always = false` untouched.
    /// The scoped rendering includes only the always-true invariant.
    #[test]
    fn invariants_block_scoped_mixed_includes_only_in_scope() {
        let cfg = cfg_with_scoped_invariant();
        let scope = scope_with_changed(vec!["unrelated/file.rs".to_string()]);
        let out = invariants_block_scoped(&cfg, &scope);
        assert!(out.contains("always active"), "always=true included: {out}");
        assert!(
            !out.contains("diff-scoped only"),
            "untouched always=false excluded: {out}"
        );
    }
}
