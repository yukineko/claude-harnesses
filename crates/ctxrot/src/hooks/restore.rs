//! `ctxrot restore` — SessionStart hook.
//!
//! At session start, find the most recent rescue/distill note for this project
//! and inject a COMPACT carryover (decisions + open todos + a link), so the prior
//! session's conclusions survive without re-bloating context. We never inject the
//! whole note — just the durable signal plus a pointer to read more on demand.

use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::loadset::LoadSet;
use harness_core::hook::HookInput;
use harness_core::store::Store;
use harness_core::verdict::Determination;

const READ_CAP: u64 = 256 * 1024;
const SECTION_CAP_CHARS: usize = 1500;
/// Cap on how many pinned items we inject as pointers (feature ②/③), so a long
/// loadset can't itself become a rot source.
const PINNED_INJECT_MAX: usize = 12;

/// Returns the additionalContext text to inject, or None if nothing applies.
///
/// Two independent sources, each individually switchable (feature ③):
///   * the prior-session carryover note (Decisions / Open todos), and
///   * the project's pinned loadset items (feature ②), surfaced as pointers.
///
/// `restore_enabled=false` turns the whole thing off.
pub fn run(input: &HookInput, cfg: &Config) -> Option<String> {
    if !cfg.restore_enabled {
        return None;
    }
    // Don't re-inject right after a compaction restart — restore is for a fresh
    // session picking up prior work, not for the compact handoff.
    if input.source == "compact" {
        return None;
    }

    let cwd = input.cwd_or_current();
    let note_block = note_carryover(input, cfg, &cwd);
    let pinned_block = if cfg.inject_pinned {
        pinned_carryover(cfg, &cwd)
    } else {
        None
    };

    match (note_block, pinned_block) {
        (None, None) => None,
        (Some(n), None) => Some(n),
        (None, Some(p)) => Some(format!(
            "[ctxrot restore] ピン留め中の参照（本文は貼らず必要時に読む）:\n{p}"
        )),
        (Some(n), Some(p)) => Some(format!("{n}\n\n■ ピン留め中の参照:\n{p}")),
    }
}

/// Pick the note to restore from: this session's own note first, then the
/// cross-session fallback.
///
/// `None` means **there is no note to restore** — a real observation. It is not
/// where an unreadable note store lands: if [`Store::list_notes`] came back
/// `Undetermined`, that is reported on stderr and this returns `None` only
/// after having said so. Before the tri-state migration the `?` here swallowed
/// an unreadable store into the same silent `None` as a fresh project, so a
/// SessionStart injected nothing and never explained why.
fn auto_select_note(store: &Store, cwd: &Path, session_id: &str) -> Option<PathBuf> {
    let own = match store.latest_note_for_session(cwd, session_id) {
        Determination::Known(own) => own,
        Determination::Undetermined(why) => {
            eprintln!(
                "ctxrot restore: 退避ノートを読めませんでした（前回の引き継ぎ有無は不明。\
                 「引き継ぎ無し」ではありません）: {why}"
            );
            return None;
        }
    };
    if own.is_some() {
        return own;
    }
    match store.latest_fallback_note(cwd) {
        Determination::Known(fallback) => fallback,
        Determination::Undetermined(why) => {
            eprintln!(
                "ctxrot restore: 退避ノートを読めませんでした（前回の引き継ぎ有無は不明。\
                 「引き継ぎ無し」ではありません）: {why}"
            );
            None
        }
    }
}

/// The prior-session carryover derived from the latest note, honoring the
/// `inject_decisions` / `inject_todos` switches. None when there is no note (or
/// both sections are off/empty and the note carries nothing else worth a pointer).
fn note_carryover(input: &HookInput, cfg: &Config, cwd: &Path) -> Option<String> {
    let store = Store::new(cfg.store_dir.clone());

    // Check for a user-pinned note first (`ctxrot ctx use-note`). If the pinned
    // path no longer exists, fall back to auto-selection and surface a warning.
    let ls = LoadSet::load(&cfg.state_dir, cwd);
    let (latest, preferred) = if let Some(ref pref) = ls.preferred_note {
        let p = PathBuf::from(pref);
        if p.exists() {
            (p, true)
        } else {
            // Pinned note is gone; fall back silently to auto-selection.
            (auto_select_note(&store, cwd, &input.session_id)?, false)
        }
    } else {
        // Normal auto-selection: prefer this session's own note, then latest safe note.
        (auto_select_note(&store, cwd, &input.session_id)?, false)
    };

    let meta = std::fs::metadata(&latest).ok()?;
    if meta.len() > READ_CAP {
        // Unexpectedly large note: just point at it, don't inline.
        let label = if preferred {
            "[ctxrot restore] 指定ノート（固定中）"
        } else {
            "[ctxrot restore] 前回の退避ノート"
        };
        return Some(format!(
            "{label}あり: {}\n→ 必要なら読み込んで続きから作業を。",
            latest.display()
        ));
    }
    let text = std::fs::read_to_string(&latest).ok()?;

    // Section gating (feature ③): a disabled section is simply never extracted.
    let decisions = if cfg.inject_decisions {
        extract_section(&text, &["決定事項", "Decisions"])
    } else {
        None
    };
    let todos = if cfg.inject_todos {
        extract_section(&text, &["残課題", "Open todos", "todos"])
    } else {
        None
    };

    let mut out = String::new();
    if preferred {
        out.push_str(
            "[ctxrot restore] 指定ノートから引き継ぎ（`ctxrot ctx use-note` で固定中）:\n",
        );
    } else {
        out.push_str("[ctxrot restore] 前回セッションからの引き継ぎ（要約）:\n");
    }
    if let Some(d) = &decisions {
        out.push_str("\n■ 決定事項:\n");
        out.push_str(d);
        out.push('\n');
    }
    if let Some(t) = &todos {
        out.push_str("\n■ 残課題:\n");
        out.push_str(t);
        out.push('\n');
    }
    out.push_str(&format!(
        "\n→ 全文: {}\n（必要時のみ読む。本文には貼らず要約＋リンク運用を維持）",
        latest.display()
    ));

    // Quality nudge (P4): if the carryover came from a deterministic `rescue-*`
    // note (no `/distill` was run last session), its Decisions/todos are just
    // regex-extracted and may be thin/empty. One line nudging /distill now —
    // kept to a single line so the injection itself doesn't bloat.
    if !harness_core::store::is_distill(&latest) {
        out.push_str("\n（前回 /distill 未実行。重要な結論は今のうちに /distill で蒸留推奨）");
    }

    // If both sections were empty/missing/off, only the pointer is useful.
    if decisions.is_none() && todos.is_none() {
        let msg = format!(
            "[ctxrot restore] 前回の退避ノートあり: {}\n→ 続きから作業する場合は読み込んで。",
            latest.display()
        );
        emit_restore(cfg, input, &latest, msg.len(), false, false);
        return Some(msg);
    }
    emit_restore(
        cfg,
        input,
        &latest,
        out.len(),
        decisions.is_some(),
        todos.is_some(),
    );
    Some(out)
}

/// A bullet list of the project's pinned loadset items (paths/labels), capped at
/// `PINNED_INJECT_MAX`. Pointers only — never the file contents. None if empty.
fn pinned_carryover(cfg: &Config, cwd: &Path) -> Option<String> {
    let ls = LoadSet::load(&cfg.state_dir, cwd);
    if ls.pinned.is_empty() {
        return None;
    }
    let shown = ls.pinned.len().min(PINNED_INJECT_MAX);
    let mut out = String::new();
    for item in ls.pinned.iter().take(shown) {
        out.push_str("- ");
        out.push_str(item);
        out.push('\n');
    }
    if ls.pinned.len() > shown {
        out.push_str(&format!(
            "…他 {} 件（`/ctx list` で全件）\n",
            ls.pinned.len() - shown
        ));
    }
    Some(out.trim_end().to_string())
}

fn emit_restore(
    cfg: &Config,
    input: &HookInput,
    note: &Path,
    bytes: usize,
    had_decisions: bool,
    had_todos: bool,
) {
    crate::metrics::emit(
        cfg,
        &input.session_id,
        "restore",
        serde_json::json!({
            "note": note.to_string_lossy(),
            "bytes": bytes,
            "had_decisions": had_decisions,
            "had_todos": had_todos,
        }),
    );
}

/// The section headings `restore` depends on for carryover. This is the single
/// source of truth for the distill *contract*: a distilled note that omits one
/// of these silently produces an empty carryover, so `note write
/// --require-sections` rejects it. Each entry is `(human label, heading aliases)`.
pub const REQUIRED_SECTIONS: &[(&str, &[&str])] = &[
    ("決定事項 / Decisions", &["決定事項", "Decisions"]),
    ("残課題 / Open todos", &["残課題", "Open todos", "todos"]),
];

/// True if `text` has a `## …` heading matching any of `titles` (presence only —
/// an empty "_(なし)_" section still counts, since `restore` handles that).
/// Matches exactly what `extract_section` keys on, so the contract guarantees
/// `restore` can find the section.
pub fn has_section(text: &str, titles: &[&str]) -> bool {
    text.lines().any(|l| {
        l.trim()
            .strip_prefix("## ")
            .map(|rest| titles.iter().any(|t| rest.contains(t)))
            .unwrap_or(false)
    })
}

/// Human labels of any `REQUIRED_SECTIONS` whose heading is missing from `text`.
/// Empty vec means the note satisfies the contract.
pub fn missing_sections(text: &str) -> Vec<&'static str> {
    REQUIRED_SECTIONS
        .iter()
        .filter(|(_, aliases)| !has_section(text, aliases))
        .map(|(label, _)| *label)
        .collect()
}

/// Sections that materially improve carryover quality but aren't load-bearing
/// for `restore` (which only consumes Decisions/todos). These mirror the rest of
/// the distill template's shape. `note write --require-sections` only *warns* on
/// these — it never rejects — so the full structure is encouraged without making
/// a thin-but-valid note impossible. Each entry is `(human label, heading aliases)`.
pub const RECOMMENDED_SECTIONS: &[(&str, &[&str])] = &[
    ("触ったファイル / Files", &["触ったファイル", "Files"]),
    (
        "重要な事実 / Key facts",
        &["重要な事実", "重要事実", "Key facts"],
    ),
    ("現在地 / Where we are", &["現在地", "Where we are"]),
];

/// Human labels of any `RECOMMENDED_SECTIONS` whose heading is missing from
/// `text`. Empty vec means the note carries the full distill shape.
pub fn missing_recommended_sections(text: &str) -> Vec<&'static str> {
    RECOMMENDED_SECTIONS
        .iter()
        .filter(|(_, aliases)| !has_section(text, aliases))
        .map(|(label, _)| *label)
        .collect()
}

/// Pull the body under a `## <title>` heading (any of the aliases), bounded.
/// Skips the "_(なし / none)_" placeholder. Public so the re-anchor check
/// (`guard::check_reanchor`) reuses the exact same extraction `restore` does.
pub fn extract_section(text: &str, titles: &[&str]) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim();
        if let Some(rest) = line.strip_prefix("## ") {
            if titles.iter().any(|t| rest.contains(t)) {
                let mut body = String::new();
                i += 1;
                while i < lines.len() && !lines[i].trim_start().starts_with("## ") {
                    body.push_str(lines[i]);
                    body.push('\n');
                    i += 1;
                }
                let body = body.trim();
                if body.is_empty() || body.contains("_(なし") {
                    return None;
                }
                return Some(clip_section(body));
            }
        }
        i += 1;
    }
    None
}

fn clip_section(s: &str) -> String {
    harness_core::transcript::truncate_chars(s, SECTION_CAP_CHARS)
}

// Keep `Path` import used even if helpers are trimmed later.
#[allow(dead_code)]
fn _typecheck(_: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pulls_named_section() {
        let note = "## 決定事項 / Decisions\n\n- A を採用\n- B は不採用\n\n## 残課題 / Open todos\n\n_(なし / none)_\n";
        assert!(extract_section(note, &["決定事項", "Decisions"])
            .unwrap()
            .contains("A を採用"));
        assert!(extract_section(note, &["残課題", "todos"]).is_none());
    }

    #[test]
    fn contract_accepts_required_headings_even_when_empty() {
        // Both headings present (todos is the "none" placeholder) → conformant.
        let note = "## 決定事項 / Decisions\n\n- A\n\n## 残課題 / Open todos\n\n_(なし / none)_\n";
        assert!(missing_sections(note).is_empty());
    }

    #[test]
    fn contract_flags_omitted_section() {
        // The distiller dropped the empty Open-todos heading entirely → violation,
        // exactly the silent-failure restore can't recover from.
        let note = "## 決定事項 / Decisions\n\n- A\n\n## 触ったファイル / Files\n\n- x.rs\n";
        let missing = missing_sections(note);
        assert_eq!(missing, vec!["残課題 / Open todos"]);
    }

    #[test]
    fn contract_flags_both_when_renamed() {
        // Decisions hidden under a non-canonical heading → restore would miss it.
        let note = "## まとめ\n\n- A を採用\n- 次は B\n";
        assert_eq!(missing_sections(note).len(), 2);
    }

    #[test]
    fn recommended_flags_only_soft_sections() {
        // Required headings present, all recommended ones absent → the soft check
        // names the three template extras and the hard check stays clean.
        let note = "## 決定事項 / Decisions\n\n- A\n\n## 残課題 / Open todos\n\n_(なし / none)_\n";
        assert!(missing_sections(note).is_empty());
        assert_eq!(missing_recommended_sections(note).len(), 3);
    }

    fn restore_fixture(name: &str, slug_prefix: &str) -> (Config, std::path::PathBuf, HookInput) {
        // Unique base dir via atomic `mkdtemp` (no pid-collision TOCTOU).
        let base = tempfile::Builder::new()
            .prefix(&format!("ctxrot-restore-{name}-"))
            .tempdir()
            .expect("tempdir")
            .keep();
        let cwd = base.join("proj");
        std::fs::create_dir_all(&cwd).unwrap();
        let cfg = Config {
            state_dir: base.join("state"),
            store_dir: base.join("store"),
            ..Config::default()
        };
        let session = "sess-restore";
        let body = "## 決定事項 / Decisions\n\n- A を採用\n\n## 残課題 / Open todos\n\n- B\n";
        let slug = format!(
            "{slug_prefix}-{}-20260101-000000",
            harness_core::store::session_tag(session)
        );
        harness_core::store::Store::new(cfg.store_dir.clone())
            .write_note(&cwd, &slug, body)
            .unwrap();
        let input = HookInput {
            session_id: session.into(),
            source: "startup".into(),
            cwd: cwd.to_string_lossy().into_owned(),
            ..HookInput::default()
        };
        (cfg, base, input)
    }

    #[test]
    fn nudges_when_only_rescue_exists() {
        let (cfg, base, input) = restore_fixture("rescue", "rescue");
        let out = run(&input, &cfg).expect("carryover from rescue note");
        assert!(out.contains("A を採用"));
        assert!(
            out.contains("/distill 未実行"),
            "rescue-only restore should nudge: {out}"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn no_nudge_when_distill_exists() {
        let (cfg, base, input) = restore_fixture("distill", "distill");
        let out = run(&input, &cfg).expect("carryover from distill note");
        assert!(out.contains("A を採用"));
        assert!(
            !out.contains("/distill 未実行"),
            "distill restore must not nudge: {out}"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    // ----- auto-injection control (feature ③) + pinned (feature ②) -----

    #[test]
    fn restore_disabled_injects_nothing() {
        let (mut cfg, base, input) = restore_fixture("disabled", "distill");
        cfg.restore_enabled = false;
        assert!(
            run(&input, &cfg).is_none(),
            "restore_enabled=false must inject nothing"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn inject_decisions_off_hides_decisions() {
        let (mut cfg, base, input) = restore_fixture("nodec", "distill");
        cfg.inject_decisions = false;
        let out = run(&input, &cfg).expect("todos still carry");
        assert!(
            !out.contains("A を採用"),
            "decisions must be omitted: {out}"
        );
        assert!(out.contains("■ 残課題"), "todos must remain: {out}");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn both_sections_off_leaves_pointer_only() {
        let (mut cfg, base, input) = restore_fixture("nosecs", "distill");
        cfg.inject_decisions = false;
        cfg.inject_todos = false;
        let out = run(&input, &cfg).expect("pointer still useful");
        assert!(!out.contains("A を採用"));
        assert!(
            out.contains("退避ノートあり"),
            "should degrade to a pointer: {out}"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn pinned_items_are_injected_as_pointers() {
        let (cfg, base, input) = restore_fixture("pinned", "distill");
        let cwd = std::path::PathBuf::from(&input.cwd);
        let mut ls = LoadSet::default();
        ls.pin("docs/spec.md");
        ls.save(&cfg.state_dir, &cwd).unwrap();
        let out = run(&input, &cfg).expect("carryover + pinned");
        assert!(
            out.contains("ピン留め中の参照"),
            "pinned header missing: {out}"
        );
        assert!(out.contains("docs/spec.md"), "pinned item missing: {out}");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn pinned_off_suppresses_pinned_block() {
        let (mut cfg, base, input) = restore_fixture("pinoff", "distill");
        cfg.inject_pinned = false;
        let cwd = std::path::PathBuf::from(&input.cwd);
        let mut ls = LoadSet::default();
        ls.pin("docs/spec.md");
        ls.save(&cfg.state_dir, &cwd).unwrap();
        let out = run(&input, &cfg).expect("carryover");
        assert!(
            !out.contains("docs/spec.md"),
            "inject_pinned=false must hide pins: {out}"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn recommended_satisfied_by_full_shape() {
        let note = "## 決定事項 / Decisions\n\n- A\n\n## 残課題 / Open todos\n\n- B\n\n\
                    ## 触ったファイル / Files\n\n- x.rs\n\n## 重要な事実 / Key facts\n\n- k\n\n\
                    ## 現在地 / Where we are\n\n- here\n";
        assert!(missing_recommended_sections(note).is_empty());
    }
}
