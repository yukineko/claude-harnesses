//! `ctxrot guard` — UserPromptSubmit hook (port of the Python v1).
//!
//! Output is CONDITIONAL AND MINIMAL: injecting a fixed block every turn would
//! itself accumulate and *cause* rot, so when nothing is relevant we print
//! nothing. Anything returned here on exit 0 is injected into the model context.
//!
//!   T1  large-reference detection (per-prompt): a big local file / URL / "全文"
//!       keyword -> tell the agent to read it via a sub-agent, not main ctx.
//!   T2  context-budget bands (per-session, escalate-only): when real usage
//!       crosses into a higher band, inject distill/offload advice ONCE.

use std::path::Path;

use regex::Regex;

use crate::config::Config;
use harness_core::hook::HookInput;
use harness_core::store::Store;
use harness_core::transcript;

/// Drop-priority for the per-turn injection cap (`guard_inject_max_chars`). When
/// the assembled blocks exceed the cap, the *lowest* priority is dropped first.
/// The anchor is purely supplemental ("あると良い" — its absence is harmless), so
/// it goes first; safety-critical warnings (large-ref, and the danger-band budget
/// which says "you are losing context NOW") are kept to the last and only
/// truncated if a single one still overflows. `Ord` is derived for `min_by_key`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Prio {
    Anchor = 0,
    Advice = 1,
    Safety = 2,
}

/// Returns the text to inject (already trimmed), or None to stay silent.
pub fn run(input: &HookInput, cfg: &Config) -> Option<String> {
    let mut blocks: Vec<(Prio, String)> = Vec::new();

    // Post-compaction recovery: if the async distill (feature ④) wrote a fresh
    // high-quality note since last turn, re-inject it ONCE. This is the only way
    // the distilled signal re-enters the live context after a /compact, since
    // PreCompact/PostCompact can't inject. Safety prio so the cap never drops it.
    if let Some(b) = check_distilled(input, cfg) {
        blocks.push((Prio::Safety, b));
    }
    if let Some(b) = check_large_references(&input.prompt, &input.cwd_or_current(), cfg) {
        blocks.push((Prio::Safety, b));
    }
    if let Some((band, b)) = check_context_budget(input, cfg) {
        // The top band is a "losing context now" warning → keep it like a safety
        // block; lower bands are advice that may be dropped before the warnings.
        let prio = if band >= cfg.bands.len() {
            Prio::Safety
        } else {
            Prio::Advice
        };
        blocks.push((prio, b));
    }
    if let Some(b) = check_reanchor(input, cfg) {
        blocks.push((Prio::Anchor, b));
    }

    let block_count = blocks.len();
    let out = cap_blocks(blocks, cfg.guard_inject_max_chars)?;

    // Observe ctxrot's OWN per-turn injection (post-cap). Several UserPromptSubmit
    // hooks across the harness family inject every prompt; summing this `inject`
    // metric is the in-repo foundation for the cross-harness injection budget
    // (see docs/adr/0001-cross-harness-injection-budget.md).
    crate::metrics::emit(
        cfg,
        &input.session_id,
        "inject",
        serde_json::json!({ "chars": out.chars().count(), "blocks": block_count }),
    );

    Some(out)
}

/// Apply the per-turn injection cap. Blocks keep their original order; when the
/// combined render exceeds `max_chars` (CJK-safe char count), whole blocks are
/// dropped lowest-priority first (anchor → advice → safety). If a single block
/// still overflows on its own it is truncated rather than dropped, so a
/// safety-critical warning is never silently lost. `max_chars == 0` disables the
/// cap (legacy behaviour: inject every block in full).
fn cap_blocks(mut blocks: Vec<(Prio, String)>, max_chars: usize) -> Option<String> {
    if blocks.is_empty() {
        return None;
    }
    let render = |bs: &[(Prio, String)]| {
        bs.iter()
            .map(|(_, s)| s.as_str())
            .collect::<Vec<_>>()
            .join("\n\n")
    };

    if max_chars == 0 {
        return Some(render(&blocks));
    }

    // Drop whole blocks (lowest priority first) until the rest fit or one remains.
    while blocks.len() > 1 && render(&blocks).chars().count() > max_chars {
        // Lowest priority wins; on ties drop the *later* block so the earlier
        // (typically more safety-critical) one is kept.
        let drop_idx = blocks
            .iter()
            .enumerate()
            .min_by_key(|(i, (p, _))| (*p, std::cmp::Reverse(*i)))
            .map(|(i, _)| i)
            .expect("non-empty");
        blocks.remove(drop_idx);
    }

    // A lone block may still exceed the cap — truncate it instead of dropping the
    // last (possibly safety) block to nothing. `truncate_chars` appends a 13-char
    // " …[truncated]" marker, so leave room for it.
    if render(&blocks).chars().count() > max_chars {
        let budget = max_chars.saturating_sub(13).max(1);
        if let Some((_, text)) = blocks.first_mut() {
            *text = transcript::truncate_chars(text, budget);
        }
    }

    Some(render(&blocks))
}

// ----------------------------------------------------------------- T1

fn heavy_kw_re() -> Regex {
    // No lookaround needed here.
    Regex::new(
        r"(?i)(全文|全部|まるごと|丸ごと|ログ全部|一字一句|そのまま貼|paste the (?:whole|entire|full)|entire file|whole file|all (?:the )?logs|dump (?:the|all))",
    )
    .expect("static regex")
}

const CONTENT_EXTS: &[&str] = &[
    "log", "json", "jsonl", "csv", "tsv", "txt", "sql", "xml", "html", "htm", "md", "out", "dump",
    "ndjson", "parquet",
];

/// A whitespace token looks like a local path (absolute/home, or has a
/// content-ish extension). Rust's regex has no lookbehind, so we tokenize and
/// test each token instead of one big pattern.
fn looks_like_path(tok: &str) -> bool {
    if tok.starts_with('/') || tok.starts_with("~/") || tok == "~" {
        return true;
    }
    if let Some(ext) = tok.rsplit('.').next() {
        if ext != tok && CONTENT_EXTS.contains(&ext.to_ascii_lowercase().as_str()) {
            return true;
        }
    }
    false
}

fn is_url(tok: &str) -> bool {
    tok.starts_with("http://") || tok.starts_with("https://")
}

fn strip_token(tok: &str) -> &str {
    // Trim surrounding quotes/brackets but keep ':' so "path:line" survives.
    tok.trim_matches(|c: char| {
        matches!(
            c,
            '"' | '\'' | '<' | '>' | '(' | ')' | '[' | ']' | ',' | ';'
        )
    })
    .trim_end_matches(['.', ',', ';'])
}

fn check_large_references(prompt: &str, cwd: &Path, cfg: &Config) -> Option<String> {
    if prompt.trim().is_empty() {
        return None;
    }

    let mut hits: Vec<String> = Vec::new();
    let mut seen: Vec<String> = Vec::new();

    for raw_tok in prompt.split_whitespace() {
        let tok = strip_token(raw_tok);
        if tok.is_empty() {
            continue;
        }

        if is_url(tok) {
            if hits.len() < 6 && !seen.iter().any(|s| s == tok) {
                seen.push(tok.to_string());
                hits.push(format!("{tok} (URL)"));
            }
            continue;
        }

        if looks_like_path(tok) {
            if seen.iter().any(|s| s == tok) {
                continue;
            }
            seen.push(tok.to_string());
            let expanded = crate::config::expand_tilde(tok);
            let path = if expanded.is_absolute() {
                expanded
            } else {
                cwd.join(&expanded)
            };
            if let Ok(meta) = std::fs::metadata(&path) {
                if meta.is_file() && meta.len() >= cfg.large_file_bytes {
                    let kb = meta.len() as f64 / 1024.0;
                    let tok_est = meta.len() / 4;
                    hits.push(format!("{tok} (~{kb:.0}KB, 推定~{tok_est}tok)"));
                }
            }
        }
        if hits.len() >= 6 {
            break;
        }
    }

    let heavy_kw = heavy_kw_re().is_match(prompt);

    if hits.is_empty() && !heavy_kw {
        return None;
    }

    let mut lines = vec!["[context-rot guard] 大きい参照を検知:".to_string()];
    for h in hits.iter().take(6) {
        lines.push(format!("  - {h}"));
    }
    if heavy_kw && hits.is_empty() {
        lines.push("  - 「全文/まるごと」系の指示を検知".to_string());
    }
    lines.push(
        "→ 全文を main context に載せないでください。Explore（読み取り専用・該当箇所だけ抜粋） \
         または general-purpose sub-agent に読ませ、要約・該当行・結論だけを受け取って作業を。 \
         大きい生データを本文に貼らないこと。"
            .to_string(),
    );
    Some(lines.join("\n"))
}

// ----------------------------------------------------------------- T2

/// Returns `(band, advice text)` so the caller can prioritise the danger band as
/// a safety block under the injection cap. None when no escalation fires.
fn check_context_budget(input: &HookInput, cfg: &Config) -> Option<(usize, String)> {
    if input.transcript_path.is_empty() {
        return None;
    }
    let (est_tokens, _src) = transcript::estimate_tokens(&input.transcript_path)?;
    let frac = est_tokens as f64 / cfg.context_window as f64;
    let band = cfg.band_for(frac);

    // Persist current band (incl. 0). Real usage drops after /compact, so when it
    // falls and later re-climbs, the same band re-fires (not a one-way ratchet).
    let _ = std::fs::create_dir_all(&cfg.state_dir);
    let safe = safe_session(&input.session_id);
    let state_file = cfg.state_dir.join(format!("{safe}.band"));
    let last: usize = std::fs::read_to_string(&state_file)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);

    if band != last {
        // Write to a temp sibling then rename (mirror compass outcome/carve
        // stores) so concurrent readers never see a truncated file.
        let tmp = cfg.state_dir.join(format!("{safe}.band.tmp"));
        if std::fs::write(&tmp, band.to_string()).is_ok() {
            let _ = std::fs::rename(&tmp, &state_file);
        }
    }

    // Metrics: emit one trajectory sample per measured prompt (incl. band 0), so
    // the token curve and every crossing are observable. Independent of whether
    // we inject advice below.
    let crossed = band > last;
    crate::metrics::emit(
        cfg,
        &input.session_id,
        "budget",
        serde_json::json!({
            "est_tokens": est_tokens,
            "frac": (frac * 1000.0).round() / 1000.0,
            "band": band,
            "band_prev": last,
            "crossed": crossed,
            "src": _src,
        }),
    );

    if band == 0 || band <= last {
        return None;
    }

    let pct = (frac * 100.0) as i64;
    let mut body = match band {
        1 => format!(
            "context使用が推定~{pct}%。区切りの良い所で、確定した結論・決定事項だけ残し、\
             試行錯誤の経過は要約に畳む準備を。以降の重い読み込みは sub-agent 経由に。"
        ),
        2 => format!(
            "context使用が推定~{pct}%。退避を推奨: 長い成果物は外部doc（Obsidian等）へ書き出し、\
             main context は「要約＋リンク」に置換を。/distill で能動蒸留、詳細調査は sub-agent に委譲して結論だけ受け取る運用へ切替。"
        ),
        _ => format!(
            "context使用が推定~{pct}%（危険域）。今やる: (1) /distill で未保存の成果物を外部docへ退避 \
             (2) /compact もしくは会話の蒸留 (3) 以降の重い読み込み・検索は必ず sub-agent 経由。"
        ),
    };

    // Preemptive rescue (P1-1a): from band 2 (≈75%) up, write a fresh durable
    // rescue note NOW — don't wait for PreCompact, which a manual `/clear` never
    // fires. The band gate already escalates at most once per crossing, so this
    // writes a bounded number of notes per session. A failed write just means no
    // confirmation line; the advice itself is unaffected.
    if band >= 2 {
        if let Some(path) = crate::hooks::rescue::write(input, cfg, &format!("band-{pct}%")) {
            body.push_str(&format!(
                "\n先行退避ノートを書き出しました（このまま /compact・/clear しても安全）: {}",
                path.display()
            ));
        }
    }

    // Proactive auto-distill (feature ⑤): on the FIRST crossing into the top
    // (danger) band — the ≈200k line — fire the high-quality background distill
    // NOW instead of waiting for a `/compact`. Hooks can't trigger compaction, so
    // this is how "auto-distill at 200k" is realized: the heavy history is
    // externalized to a `distill-*` note and the next guard re-injects the summary
    // (main trends toward 要約＋リンク). Real token release still needs `/compact`.
    // The escalate-only band gate above means this fires at most once per upward
    // crossing; `spawn_for_band` is a no-op when `auto_distill_on_band` is off.
    if band >= cfg.bands.len() {
        crate::hooks::distill::spawn_for_band(input, cfg);
        if cfg.auto_distill_on_band {
            body.push_str(
                "\n高品質 distill をバックグラウンド起動しました（重い履歴を外部ノートへ退避。\
                 次ターンで要約を再注入）。実トークン解放には /compact が必要です。",
            );
        }
    }

    Some((band, format!("[context-rot guard] {body}")))
}

// ---------------------------------------------- async-distill re-inject (feature ④)

/// Hard ceiling per re-injected section after a compaction (CJK-safe char count).
const DISTILL_SECTION_CAP_CHARS: usize = 700;

/// Consume the `<state_dir>/<safe>.distilled` marker left by the async-distill
/// worker and re-inject the distilled Decisions/todos ONCE. The marker is deleted
/// on read so this fires a single time per distill — the post-compaction handoff
/// that no hook can do directly. None when no distill landed since last turn.
fn check_distilled(input: &HookInput, cfg: &Config) -> Option<String> {
    let marker = crate::hooks::distill::marker_path(cfg, &input.session_id);
    let note_path = std::fs::read_to_string(&marker).ok()?;
    let note_path = note_path.trim();
    // Consume it now so a failure below doesn't make us re-fire every turn.
    let _ = std::fs::remove_file(&marker);
    if note_path.is_empty() {
        return None;
    }
    let note = Path::new(note_path);
    let text = std::fs::read_to_string(note).ok()?;

    let decisions = crate::hooks::restore::extract_section(&text, &["決定事項", "Decisions"])
        .map(|s| transcript::truncate_chars(&s, DISTILL_SECTION_CAP_CHARS));
    let todos = crate::hooks::restore::extract_section(&text, &["残課題", "Open todos", "todos"])
        .map(|s| transcript::truncate_chars(&s, DISTILL_SECTION_CAP_CHARS));

    let mut out = String::from(
        "[ctxrot distill] /compact 後の高品質蒸留が利用可能（直前の会話から再注入）:\n",
    );
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
        "\n→ 全文: {note_path}\n（必要時のみ読む。本文には貼らず要約＋リンク運用を維持）"
    ));

    crate::metrics::emit(
        cfg,
        &input.session_id,
        "distill_inject",
        serde_json::json!({
            "note": note_path,
            "bytes": out.len(),
            "decisions": decisions.is_some(),
            "todos": todos.is_some(),
        }),
    );

    Some(out)
}

// ----------------------------------------------------------------- re-anchor (P1)

/// Filesystem-safe form of a session id, for `<state_dir>/<safe>.{band,anchor,distilled}`.
/// Shared with `distill` so the async-distill marker keys on the same name.
pub(crate) fn safe_session(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Hard ceiling per re-anchored section (CJK-safe char count, much tighter than
/// `restore`'s full carryover — this is a periodic re-surfacing, not a handoff).
const ANCHOR_SECTION_CAP_CHARS: usize = 600;

/// Best-effort snapshot time of the note the anchor is drawn from: the
/// frontmatter `created:` field if present, else the file's mtime. Surfaced in
/// the anchor heading so the model can judge how fresh the re-surfaced decisions
/// are — a note that predates the latest decisions can otherwise re-float a stale
/// conclusion and mislead. None only if neither source is readable.
fn note_freshness(note: &Path, text: &str) -> Option<String> {
    for line in text.lines().take(15) {
        if let Some(rest) = line.trim().strip_prefix("created:") {
            let v = rest.trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    // Fallback: the file's mtime as a local timestamp.
    let mtime = std::fs::metadata(note).ok()?.modified().ok()?;
    let dt: chrono::DateTime<chrono::Local> = mtime.into();
    Some(dt.format("%Y-%m-%dT%H:%M:%S%:z").to_string())
}

/// Re-anchor (P1): fight lost-in-the-middle by periodically re-surfacing THIS
/// session's already-recorded Decisions / Open todos near the end of the window,
/// where attention is strongest. The `restore` carryover injected at SessionStart
/// sinks into the mid-context blind spot as the session grows; this lifts the
/// durable signal back to the tail.
///
/// Deliberately conservative (added tokens vs. ctxrot's own goal are in tension):
///   * only at/above `reanchor_min_band`,
///   * at most once per `reanchor_every_prompts` qualifying prompts (cooldown in
///     `<state_dir>/<safe>.anchor`), and
///   * only when this session's own note actually has Decisions/todos substance.
fn check_reanchor(input: &HookInput, cfg: &Config) -> Option<String> {
    if !cfg.reanchor_enabled || input.transcript_path.is_empty() {
        return None;
    }
    let (est_tokens, _src) = transcript::estimate_tokens(&input.transcript_path)?;
    let frac = est_tokens as f64 / cfg.context_window as f64;
    let band = cfg.band_for(frac);
    if band < cfg.reanchor_min_band {
        return None;
    }

    // Cadence gate. The cooldown counts DOWN only on qualifying prompts (band ≥
    // floor), so it freezes below the floor and resumes after a /compact-driven
    // dip — re-fireable, never a one-way ratchet.
    let _ = std::fs::create_dir_all(&cfg.state_dir);
    let anchor_file = cfg
        .state_dir
        .join(format!("{}.anchor", safe_session(&input.session_id)));
    let cooldown: u64 = std::fs::read_to_string(&anchor_file)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    if cooldown > 0 {
        let _ = std::fs::write(&anchor_file, (cooldown - 1).to_string());
        return None;
    }

    // Only re-surface what THIS session already committed to its own note; never
    // a sibling/fallback note (that's restore's job at SessionStart).
    let cwd = input.cwd_or_current();
    let note =
        Store::new(cfg.store_dir.clone()).latest_note_for_session(&cwd, &input.session_id)?;
    let text = std::fs::read_to_string(&note).ok()?;

    let decisions = crate::hooks::restore::extract_section(&text, &["決定事項", "Decisions"])
        .map(|s| transcript::truncate_chars(&s, ANCHOR_SECTION_CAP_CHARS));
    let todos = crate::hooks::restore::extract_section(&text, &["残課題", "Open todos", "todos"])
        .map(|s| transcript::truncate_chars(&s, ANCHOR_SECTION_CAP_CHARS));
    if decisions.is_none() && todos.is_none() {
        // No substance (empty / "_(なし)_" only) → stay silent, leave cooldown at 0
        // so we fire as soon as the note gains substance.
        return None;
    }

    // Label the source note's freshness so the re-surfaced decisions can be
    // weighed against anything decided since (the note is a past snapshot).
    let mut out = match note_freshness(&note, &text) {
        Some(ts) => {
            format!("[ctxrot anchor] 直近の確定事項（{ts}時点の退避ノートより・末尾再浮上）:\n")
        }
        None => String::from("[ctxrot anchor] 直近の確定事項（再掲・末尾再浮上）:\n"),
    };
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

    // Armed: hold off for the next `reanchor_every_prompts` qualifying prompts.
    let _ = std::fs::write(&anchor_file, cfg.reanchor_every_prompts.to_string());

    crate::metrics::emit(
        cfg,
        &input.session_id,
        "anchor",
        serde_json::json!({
            "bytes": out.len(),
            "band": band,
            "decisions": decisions.is_some(),
            "todos": todos.is_some(),
        }),
    );

    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_detected() {
        assert!(is_url("https://example.com/x"));
        assert!(!is_url("example.com"));
    }

    #[test]
    fn path_shapes() {
        assert!(looks_like_path("/var/log/app.log"));
        assert!(looks_like_path("~/data.csv"));
        assert!(looks_like_path("notes.md"));
        assert!(!looks_like_path("hello"));
        assert!(!looks_like_path("function"));
    }

    #[test]
    fn heavy_kw() {
        assert!(heavy_kw_re().is_match("このログ全部貼って"));
        assert!(heavy_kw_re().is_match("paste the entire file"));
        assert!(!heavy_kw_re().is_match("少しだけ見せて"));
    }

    #[test]
    fn band_thresholds() {
        let cfg = Config::default();
        assert_eq!(cfg.band_for(0.10), 0);
        assert_eq!(cfg.band_for(0.50), 1);
        assert_eq!(cfg.band_for(0.80), 2);
        assert_eq!(cfg.band_for(0.95), 3);
    }

    #[test]
    fn band_crossing_writes_preemptive_rescue() {
        // Auto-cleaned unique temp dir (atomic mkdtemp, no pid-collision TOCTOU).
        let tmp = tempfile::tempdir().expect("tempdir");
        let base = tmp.path();
        let cwd = base.join("proj");
        std::fs::create_dir_all(&cwd).unwrap();

        let cfg = Config {
            state_dir: base.join("state"),
            store_dir: base.join("store"),
            // Keep this test about the deterministic rescue note: don't fire the
            // proactive band-3 distill (it would shell out to `claude distill-bg`).
            auto_distill_on_band: false,
            ..Config::default()
        };
        let input = HookInput {
            session_id: "sess-guard".into(),
            // Fixture usage ≈ 184200 / 200000 ≈ 92% → band 3 (≥2).
            transcript_path: "tests/fixtures/transcript.jsonl".into(),
            cwd: cwd.to_string_lossy().into_owned(),
            ..HookInput::default()
        };

        // First crossing: advice mentions the preemptive note, and one is on disk.
        let (band, out) =
            check_context_budget(&input, &cfg).expect("band advice on first crossing");
        assert_eq!(band, 3, "fixture usage ≈92% → danger band");
        assert!(
            out.contains("先行退避ノート"),
            "should confirm preemptive rescue: {out}"
        );
        // auto_distill disabled → no background-distill confirmation line.
        assert!(
            !out.contains("バックグラウンド起動"),
            "distill line must be absent when auto_distill_on_band is off: {out}"
        );

        let store = harness_core::store::Store::new(cfg.store_dir.clone());
        let notes = store.list_notes(&cwd);
        assert!(
            notes.iter().any(|p| p
                .file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|n| n.starts_with("rescue-"))),
            "expected a rescue-*.md note, got {notes:?}"
        );

        // Escalate-only: the same band does not re-fire (so it won't re-rescue every turn).
        assert!(check_context_budget(&input, &cfg).is_none());
    }

    /// Build a temp cfg + cwd and a session note carrying the given Decisions /
    /// Open-todos bodies, tagged so `latest_note_for_session` routes to it.
    fn reanchor_fixture(
        name: &str,
        session: &str,
        decisions: &str,
        todos: &str,
    ) -> (Config, std::path::PathBuf, HookInput) {
        // Unique base dir via atomic `mkdtemp` (no pid-collision TOCTOU).
        let base = tempfile::Builder::new()
            .prefix(&format!("ctxrot-anchor-{name}-"))
            .tempdir()
            .expect("tempdir")
            .keep();
        let cwd = base.join("proj");
        std::fs::create_dir_all(&cwd).unwrap();
        let cfg = Config {
            state_dir: base.join("state"),
            store_dir: base.join("store"),
            reanchor_every_prompts: 3,
            ..Config::default()
        };
        let body = format!(
            "---\ntype: ctxrot-rescue\ncreated: 2026-01-01T00:00:00+09:00\n---\n\n\
             ## 決定事項 / Decisions\n\n{decisions}\n\n## 残課題 / Open todos\n\n{todos}\n"
        );
        let slug = format!(
            "rescue-{}-20260101-000000",
            harness_core::store::session_tag(session)
        );
        harness_core::store::Store::new(cfg.store_dir.clone())
            .write_note(&cwd, &slug, &body)
            .unwrap();
        let input = HookInput {
            session_id: session.into(),
            // Fixture usage ≈ 92% → band 3 (≥ reanchor_min_band 2).
            transcript_path: "tests/fixtures/transcript.jsonl".into(),
            cwd: cwd.to_string_lossy().into_owned(),
            ..HookInput::default()
        };
        (cfg, base, input)
    }

    #[test]
    fn reanchor_fires_then_respects_cadence() {
        let (cfg, base, input) =
            reanchor_fixture("fire", "sess-anchor", "- serde を採用", "- tests を書く");

        let out = check_reanchor(&input, &cfg).expect("anchor on first qualifying prompt");
        assert!(out.contains("[ctxrot anchor]"));
        assert!(out.contains("serde を採用"));
        assert!(out.contains("tests を書く"));

        // Cooldown of reanchor_every_prompts (3) qualifying prompts before re-fire.
        assert!(check_reanchor(&input, &cfg).is_none());
        assert!(check_reanchor(&input, &cfg).is_none());
        assert!(check_reanchor(&input, &cfg).is_none());
        assert!(check_reanchor(&input, &cfg)
            .expect("re-fires after the cadence window")
            .contains("[ctxrot anchor]"));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn reanchor_heading_shows_note_freshness() {
        // The note carries a `created:` frontmatter; the anchor heading must label
        // its snapshot time so a stale note is recognisable.
        let (cfg, base, input) =
            reanchor_fixture("fresh", "sess-fresh", "- serde を採用", "- tests を書く");
        let out = check_reanchor(&input, &cfg).expect("anchor fires");
        assert!(
            out.contains("2026-01-01T00:00:00+09:00時点の退避ノートより"),
            "heading must carry the note's created timestamp: {out}"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn note_freshness_falls_back_to_mtime() {
        // No frontmatter `created:` → use the file mtime (here: just written, so a
        // current local timestamp). We only assert a plausible timestamp is found.
        // Auto-cleaned unique temp dir (atomic mkdtemp, no pid-collision TOCTOU).
        let tmp = tempfile::tempdir().expect("tempdir");
        let base = tmp.path();
        let note = base.join("note.md");
        std::fs::write(&note, "## 決定事項\n\n- A\n").unwrap();
        let ts = note_freshness(&note, "## 決定事項\n\n- A\n").expect("mtime available");
        assert!(
            ts.starts_with("20"),
            "expected an ISO-ish local timestamp, got {ts}"
        );
    }

    #[test]
    fn reanchor_silent_without_substance() {
        // Only the "none" placeholder in both sections → nothing to re-surface.
        let (cfg, base, input) =
            reanchor_fixture("empty", "sess-empty", "_(なし / none)_", "_(なし / none)_");
        assert!(check_reanchor(&input, &cfg).is_none());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn reanchor_silent_without_own_note() {
        let (mut cfg, base, mut input) = reanchor_fixture("noown", "sess-has-note", "- A", "- B");
        // A different session id has no tagged note of its own → no anchor.
        input.session_id = "sess-other".into();
        cfg.reanchor_every_prompts = 3;
        assert!(check_reanchor(&input, &cfg).is_none());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn reanchor_disabled_stays_silent() {
        let (mut cfg, base, input) = reanchor_fixture("off", "sess-off", "- A を採用", "- B");
        cfg.reanchor_enabled = false;
        assert!(check_reanchor(&input, &cfg).is_none());
        let _ = std::fs::remove_dir_all(&base);
    }

    // ----------------------------------------------------------- N2 inject cap

    /// A fresh fixture where all three blocks fire at once: a high-band fixture
    /// (≈92% → danger budget), a heavy-keyword prompt (large-ref), and a bulky
    /// session note so the anchor block is the largest of the three. The crafted
    /// rescue note is fresh, so the preemptive band rescue coalesces onto it and
    /// `latest_note_for_session` routes the anchor at it.
    fn cap_fixture(name: &str, cap: usize) -> (Config, std::path::PathBuf, HookInput) {
        // Unique base dir via atomic `mkdtemp` (no pid-collision TOCTOU).
        let base = tempfile::Builder::new()
            .prefix(&format!("ctxrot-cap-{name}-"))
            .tempdir()
            .expect("tempdir")
            .keep();
        let cwd = base.join("proj");
        std::fs::create_dir_all(&cwd).unwrap();
        let cfg = Config {
            state_dir: base.join("state"),
            store_dir: base.join("store"),
            guard_inject_max_chars: cap,
            ..Config::default()
        };
        // Bulky Decisions/Open todos → the anchor block dominates and is the cap's
        // first casualty. Each section is truncated to ANCHOR_SECTION_CAP_CHARS.
        let big = format!("- {}", "決定".repeat(400));
        let body = format!("## 決定事項 / Decisions\n\n{big}\n\n## 残課題 / Open todos\n\n{big}\n");
        let slug = format!(
            "rescue-{}-20260101-000000",
            harness_core::store::session_tag("sess-cap")
        );
        harness_core::store::Store::new(cfg.store_dir.clone())
            .write_note(&cwd, &slug, &body)
            .unwrap();
        let input = HookInput {
            session_id: "sess-cap".into(),
            prompt: "このログを全文ください".into(), // heavy keyword → large-ref block
            transcript_path: "tests/fixtures/transcript.jsonl".into(), // ≈92% → band 3
            cwd: cwd.to_string_lossy().into_owned(),
            ..HookInput::default()
        };
        (cfg, base, input)
    }

    #[test]
    fn inject_cap_drops_anchor_keeps_safety() {
        let (cfg, base, input) = cap_fixture("on", 1200);
        let out = run(&input, &cfg).expect("guard injects at high band");
        assert!(
            out.chars().count() <= 1200,
            "combined output must respect the cap, got {} chars",
            out.chars().count()
        );
        // Supplemental anchor is dropped first…
        assert!(
            !out.contains("[ctxrot anchor]"),
            "anchor should be dropped: {out}"
        );
        // …while both safety-critical blocks survive.
        assert!(
            out.contains("大きい参照を検知"),
            "large-ref warning must survive"
        );
        assert!(out.contains("危険域"), "danger-band budget must survive");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn distilled_marker_reinjects_once_then_consumed() {
        // Auto-cleaned unique temp dir (atomic mkdtemp, no pid-collision TOCTOU).
        let tmp = tempfile::tempdir().expect("tempdir");
        let base = tmp.path();
        let cwd = base.join("proj");
        std::fs::create_dir_all(&cwd).unwrap();
        let cfg = Config {
            state_dir: base.join("state"),
            store_dir: base.join("store"),
            ..Config::default()
        };
        let session = "sess-distinj";
        // A high-quality distill note on disk + the marker pointing at it.
        let body = "## 決定事項 / Decisions\n\n- serde を採用\n\n## 残課題 / Open todos\n\n- tests を書く\n";
        let note = harness_core::store::Store::new(cfg.store_dir.clone())
            .write_note(&cwd, "distill-abc-20260101-000000", body)
            .unwrap();
        std::fs::create_dir_all(&cfg.state_dir).unwrap();
        std::fs::write(
            crate::hooks::distill::marker_path(&cfg, session),
            note.to_string_lossy().as_bytes(),
        )
        .unwrap();

        let input = HookInput {
            session_id: session.into(),
            // No transcript → budget/anchor stay silent; only the distill block fires.
            cwd: cwd.to_string_lossy().into_owned(),
            ..HookInput::default()
        };

        let out = run(&input, &cfg).expect("distill re-injection on first prompt");
        assert!(
            out.contains("[ctxrot distill]"),
            "expected distill block: {out}"
        );
        assert!(out.contains("serde を採用"));
        assert!(out.contains("tests を書く"));

        // Consumed: the marker is gone and a second prompt injects nothing.
        assert!(!crate::hooks::distill::marker_path(&cfg, session).exists());
        assert!(run(&input, &cfg).is_none(), "must fire exactly once");
    }

    #[test]
    fn inject_cap_zero_injects_all_blocks() {
        let (cfg, base, input) = cap_fixture("off", 0);
        let out = run(&input, &cfg).expect("guard injects at high band");
        assert!(out.contains("[ctxrot anchor]"), "no cap → anchor present");
        assert!(
            out.contains("大きい参照を検知"),
            "no cap → large-ref present"
        );
        assert!(out.contains("危険域"), "no cap → budget present");
        assert!(
            out.chars().count() > 1200,
            "uncapped output exceeds the default cap"
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}
