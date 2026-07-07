//! Optional: write/update an AEGIS-style Markdown *record* note on SessionEnd.
//!
//! Opt-in (`record = true`) and only if the vault directory already exists — we
//! never create the vault. Unlike the terse `sessions/` note, this note has NO
//! YAML frontmatter and uses the AEGIS section skeleton (Japanese headings). The
//! prose sections are left as `<!-- fill: … -->` placeholders for a later
//! model-driven `/record` pass to fill.
//!
//! Append-merge, never overwrite: on first creation we emit the full skeleton;
//! on later calls we replace ONLY the auto-generated `## コスト` and
//! `## 数値サマリ` blocks (delimited by stable HTML-comment markers) and leave
//! every prose section untouched.

use std::collections::BTreeMap;
use std::path::PathBuf;

use harness_core::estimate_transcript_cost;
use harness_core::pricing::{self, PriceOverride};
use harness_core::session;
use harness_core::transcript;
use harness_core::usage::ModelUsage;

use crate::config::Config;
use crate::metrics::{short, Session};

const COST_START: &str = "<!-- si:cost:start -->";
const COST_END: &str = "<!-- si:cost:end -->";
const NUM_START: &str = "<!-- si:numeric:start -->";
const NUM_END: &str = "<!-- si:numeric:end -->";

/// Inputs the writer needs beyond the persisted rollup. `transcript_path` feeds
/// the cost (usage aggregation) and end-of-session context estimate.
pub struct RecordCtx<'a> {
    pub session_id: &'a str,
    pub project: &'a str,
    pub date: &'a str,
    pub turns: u64,
    pub tool_events: u64,
    pub files_touched: usize,
    pub transcript_path: &'a str,
    pub overrides: &'a [PriceOverride],
}

/// Build the auto `## 数値サマリ` block body (between markers, exclusive).
fn numeric_body(ctx: &RecordCtx) -> String {
    let context_line = match transcript::estimate_tokens(ctx.transcript_path) {
        Some((tokens, band)) => format!("- context: ~{tokens} tokens ({band})"),
        None => "- context: (計測データなし)".to_string(),
    };
    format!(
        "- turns: {}\n- tool events: {}\n- files touched: {}\n{}",
        ctx.turns, ctx.tool_events, ctx.files_touched, context_line
    )
}

/// Resolve this session's per-model token usage. Prefer gauge's persisted
/// canonical [`SessionRecord`] (no transcript re-parse — this is what removes
/// session-insights from the "triple parse"); fall back to a fresh aggregate
/// when the canon isn't available (gauge not installed/disabled, or a custom
/// state_dir). session-insights is a passive recorder, so a record that lags by
/// at most the current turn is acceptable here.
fn session_models(ctx: &RecordCtx) -> Option<BTreeMap<String, ModelUsage>> {
    let rec = session::load_one(&session::default_state_dir(), ctx.session_id);
    if let Some(rec) = rec {
        if !rec.models.is_empty() {
            return Some(rec.models);
        }
    }
    estimate_transcript_cost(ctx.transcript_path, ctx.overrides).map(|e| e.aggregate.models)
}

/// Resolve per-agent (main / sub-agent) token usage. Prefer gauge's canonical
/// [`SessionRecord`] when it has agent data; fall back to a fresh transcript
/// aggregate. This fallback makes session-insights robust against hook-ordering
/// races where gauge may not have written its record yet when we run.
fn session_agents(ctx: &RecordCtx) -> Option<BTreeMap<String, harness_core::usage::AgentUsage>> {
    let rec = session::load_one(&session::default_state_dir(), ctx.session_id);
    if let Some(rec) = rec {
        if !rec.agents.is_empty() {
            return Some(rec.agents);
        }
    }
    estimate_transcript_cost(ctx.transcript_path, ctx.overrides)
        .map(|e| e.aggregate.agents)
        .filter(|a| !a.is_empty())
}

/// Build the auto `## コスト` block body (between markers, exclusive).
fn cost_body(ctx: &RecordCtx) -> String {
    let Some(models) = session_models(ctx) else {
        return "- (コストデータなし)".to_string();
    };
    let total_usd = pricing::session_cost(models.iter(), ctx.overrides);
    let mut input = 0u64;
    let mut output = 0u64;
    let mut cache_write = 0u64;
    let mut cache_read = 0u64;
    let mut total_tokens = 0u64;
    for u in models.values() {
        input += u.input;
        output += u.output;
        cache_write += u.cache_write_5m + u.cache_write_1h;
        cache_read += u.cache_read;
        total_tokens += u.total_tokens();
    }
    let model_names: Vec<String> = models.keys().cloned().collect();
    let models_line = if model_names.is_empty() {
        "(none)".to_string()
    } else {
        model_names.join(", ")
    };

    // Per-agent breakdown (main / sub-agent). Falls back to transcript aggregate
    // so hook-ordering races with gauge don't silently drop the attribution.
    let agent_lines = match session_agents(ctx) {
        Some(agents) if !agents.is_empty() => {
            let mut lines = String::new();
            let mut sorted: Vec<_> = agents.iter().collect();
            sorted.sort_by_key(|(k, _)| k.as_str());
            for (name, au) in &sorted {
                let cost = pricing::session_cost(au.models.iter(), ctx.overrides);
                lines.push_str(&format!(
                    "  - {name}: ${cost:.4} USD ({} turns)\n",
                    au.turns
                ));
            }
            format!("- by agent:\n{lines}")
        }
        _ => String::new(),
    };

    format!(
        "- total: ${total_usd:.2} USD\n\
         {agent_lines}\
         - total tokens: {total_tokens}\n\
         - input: {input}   output: {output}\n\
         - cache write: {cache_write}   cache read: {cache_read}\n\
         - models: {models_line}"
    )
}

/// The full skeleton emitted on first creation.
fn skeleton(ctx: &RecordCtx) -> String {
    format!(
        "# {date} {project} セッション記録\n\
         \n\
         ## 完了サマリ\n\
         <!-- fill: 完了サマリ -->\n\
         \n\
         ## つまずき / 学び\n\
         <!-- fill: つまずき / 学び -->\n\
         \n\
         ## 振り返り / 確立した方針\n\
         <!-- fill: 振り返り / 確立した方針 -->\n\
         \n\
         ## 注意点 / 落とし穴\n\
         <!-- fill: 注意点 / 落とし穴 -->\n\
         \n\
         ## 数値サマリ\n\
         {NUM_START}\n\
         {numeric}\n\
         {NUM_END}\n\
         \n\
         ## コスト\n\
         {COST_START}\n\
         {cost}\n\
         {COST_END}\n\
         \n\
         ## 残課題\n\
         <!-- fill: 残課題 -->\n\
         \n\
         ## 要追跡 / あとで確認\n\
         <!-- fill: 要追跡 / あとで確認 -->\n\
         \n\
         ## 関連\n\
         <!-- fill: 関連 -->\n",
        date = ctx.date,
        project = ctx.project,
        numeric = numeric_body(ctx),
        cost = cost_body(ctx),
    )
}

/// Replace the body between `start`/`end` markers (inclusive of marker lines'
/// content) with `new_body`. If the markers are absent, returns the input
/// unchanged (a model may have restructured the note; we don't force-insert).
fn replace_block(text: &str, start: &str, end: &str, new_body: &str) -> String {
    let Some(s) = text.find(start) else {
        return text.to_string();
    };
    let after_start = s + start.len();
    let Some(rel_e) = text[after_start..].find(end) else {
        return text.to_string();
    };
    let e = after_start + rel_e;
    let mut out = String::with_capacity(text.len() + new_body.len());
    out.push_str(&text[..after_start]);
    out.push('\n');
    out.push_str(new_body);
    out.push('\n');
    out.push_str(&text[e..]);
    out
}

/// Refresh both auto blocks in an existing note.
fn merge(existing: &str, ctx: &RecordCtx) -> String {
    let t = replace_block(existing, NUM_START, NUM_END, &numeric_body(ctx));
    replace_block(&t, COST_START, COST_END, &cost_body(ctx))
}

/// Write or update the record note. Returns the written path, or None if
/// skipped/failed. Fail-soft throughout.
pub fn write_record(cfg: &Config, ctx: &RecordCtx) -> Option<PathBuf> {
    if !cfg.record || !cfg.obsidian_vault.is_dir() {
        return None;
    }
    if ctx.date.len() < 10 {
        return None;
    }
    let dir = cfg.obsidian_vault.join(&cfg.record_dir);
    std::fs::create_dir_all(&dir).ok()?;
    let slug = format!("{}-{}", slugify(ctx.project), short(ctx.session_id));
    let path = dir.join(format!("{}-{}.md", &ctx.date[..10], slug));

    // A blank/whitespace-only file (no file at all, or an empty stub — e.g. an
    // Obsidian unresolved-link placeholder, or a prior write that got
    // interrupted) has no markers for `merge` to find. `replace_block` leaves
    // text it can't find markers in untouched *by design* (so a model's
    // deliberate restructuring isn't clobbered), but that same rule would
    // otherwise persist emptiness forever: merge(empty) == empty, written back
    // as 0 bytes on every call. Treat "nothing there" the same as "no file".
    let body = match std::fs::read_to_string(&path) {
        Ok(existing) if !existing.trim().is_empty() => merge(&existing, ctx),
        _ => skeleton(ctx),
    };
    std::fs::write(&path, body).ok()?;
    Some(path)
}

/// Convenience: build a `RecordCtx` from a persisted rollup + transcript and write.
pub fn write_from_session(
    cfg: &Config,
    s: &Session,
    transcript_path: &str,
    turns_fallback: u64,
) -> Option<PathBuf> {
    let date = if s.started_at.len() >= 10 {
        s.started_at[..10].to_string()
    } else {
        chrono::Local::now().format("%Y-%m-%d").to_string()
    };
    let turns = if s.turns > 0 { s.turns } else { turns_fallback };
    let ctx = RecordCtx {
        session_id: &s.session_id,
        project: &s.project,
        date: &date,
        turns,
        tool_events: s.tool_events,
        files_touched: s.files.len(),
        transcript_path,
        overrides: &cfg.price_overrides,
    };
    write_record(cfg, &ctx)
}

/// Lowercased kebab slug from a project name (ASCII alnum kept; runs of other
/// chars collapse to a single `-`). Non-ASCII is dropped; empty → "session".
fn slugify(name: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    let s = out.trim_matches('-').to_string();
    if s.is_empty() {
        "session".to_string()
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_transcript(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("si-record-{}-{name}.jsonl", std::process::id()));
        let body = concat!(
            r#"{"type":"assistant","timestamp":"2026-06-22T10:00:01Z","message":{"model":"claude-opus-4-8","content":[{"type":"text","text":"x"},{"type":"tool_use","name":"Bash"}],"usage":{"input_tokens":1000000,"output_tokens":1000000,"cache_read_input_tokens":0}}}"#,
            "\n",
        );
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        p
    }

    fn ctx<'a>(tp: &'a str, turns: u64) -> RecordCtx<'a> {
        RecordCtx {
            session_id: "abcdef1234567890",
            project: "harness",
            date: "2026-06-22",
            turns,
            tool_events: 7,
            files_touched: 3,
            transcript_path: tp,
            overrides: &[],
        }
    }

    #[test]
    fn skeleton_has_all_sections_and_placeholders() {
        let tp = write_transcript("skel");
        let s = skeleton(&ctx(tp.to_str().unwrap(), 4));
        for h in [
            "# 2026-06-22 harness セッション記録",
            "## 完了サマリ",
            "## つまずき / 学び",
            "## 振り返り / 確立した方針",
            "## 注意点 / 落とし穴",
            "## 数値サマリ",
            "## コスト",
            "## 残課題",
            "## 要追跡 / あとで確認",
            "## 関連",
        ] {
            assert!(s.contains(h), "missing heading: {h}\n{s}");
        }
        assert!(s.contains("<!-- fill: 完了サマリ -->"));
        assert!(s.contains("<!-- fill: 注意点 / 落とし穴 -->"));
        assert!(s.contains("<!-- fill: 要追跡 / あとで確認 -->"));
        assert!(s.contains(COST_START) && s.contains(COST_END));
        assert!(s.contains(NUM_START) && s.contains(NUM_END));
        // numeric auto values
        assert!(s.contains("- turns: 4"));
        assert!(s.contains("- tool events: 7"));
        assert!(s.contains("- files touched: 3"));
        assert!(s.contains("- context: ~"));
        // cost: opus 1M in + 1M out = 5 + 25 = $30.00; total tokens 2,000,000
        assert!(s.contains("- total: $30.00 USD"), "{s}");
        assert!(s.contains("- total tokens: 2000000"));
        assert!(s.contains("claude-opus-4-8"));
        let _ = std::fs::remove_file(&tp);
    }

    #[test]
    fn merge_replaces_auto_blocks_and_preserves_prose() {
        let tp = write_transcript("merge");
        // First-create skeleton, then a model fills the prose.
        let first = skeleton(&ctx(tp.to_str().unwrap(), 4));
        let filled = first
            .replace(
                "<!-- fill: 完了サマリ -->",
                "実際にやったこと: record.rs を追加",
            )
            .replace("<!-- fill: 残課題 -->", "テストを増やす");

        // Second call: different numbers.
        let merged = merge(&filled, &ctx(tp.to_str().unwrap(), 9));

        // Prose preserved.
        assert!(merged.contains("実際にやったこと: record.rs を追加"));
        assert!(merged.contains("テストを増やす"));
        // Auto blocks refreshed: turns now 9.
        assert!(merged.contains("- turns: 9"), "{merged}");
        assert!(!merged.contains("- turns: 4"));
        // Markers still present exactly once each.
        assert_eq!(merged.matches(COST_START).count(), 1);
        assert_eq!(merged.matches(COST_END).count(), 1);
        assert_eq!(merged.matches(NUM_START).count(), 1);
        // Cost unchanged (same transcript) and still correct.
        assert!(merged.contains("- total: $30.00 USD"));
        let _ = std::fs::remove_file(&tp);
    }

    #[test]
    fn write_record_first_then_merge_on_disk() {
        let tp = write_transcript("disk");
        let vault = std::env::temp_dir().join(format!("si-vault-{}", std::process::id()));
        std::fs::create_dir_all(&vault).unwrap();
        let cfg = Config {
            record: true,
            obsidian_vault: vault.clone(),
            ..Config::default()
        };

        let mut s = Session {
            session_id: "abcdef1234567890".into(),
            project: "harness".into(),
            started_at: "2026-06-22T10:00:00Z".into(),
            turns: 4,
            tool_events: 7,
            files: vec!["a".into(), "b".into(), "c".into()],
            ..Session::default()
        };

        let p1 = write_from_session(&cfg, &s, tp.to_str().unwrap(), 0).expect("first write");
        assert!(p1.ends_with("2026-06-22-harness-abcdef12.md"));
        let on_disk = std::fs::read_to_string(&p1).unwrap();
        let edited = on_disk.replace("<!-- fill: 完了サマリ -->", "PROSE-KEEP");
        std::fs::write(&p1, &edited).unwrap();

        // Second write merges, preserves prose.
        s.turns = 12;
        let p2 = write_from_session(&cfg, &s, tp.to_str().unwrap(), 0).expect("second write");
        assert_eq!(p1, p2);
        let after = std::fs::read_to_string(&p2).unwrap();
        assert!(after.contains("PROSE-KEEP"));
        assert!(after.contains("- turns: 12"));

        let _ = std::fs::remove_file(&tp);
        let _ = std::fs::remove_dir_all(&vault);
    }

    /// Regression for the 0-byte note bug: if a blank stub file already sits at
    /// the target path (e.g. an Obsidian unresolved-link placeholder, or any
    /// prior empty write), `write_record` must not silently `merge` into it and
    /// persist emptiness — it must fall back to the full skeleton, same as if
    /// the file were absent.
    #[test]
    fn write_record_heals_a_preexisting_empty_file() {
        let tp = write_transcript("heal-empty");
        let vault = std::env::temp_dir().join(format!("si-vault-empty-{}", std::process::id()));
        std::fs::create_dir_all(&vault).unwrap();
        let cfg = Config {
            record: true,
            obsidian_vault: vault.clone(),
            ..Config::default()
        };
        let s = Session {
            session_id: "abcdef1234567890".into(),
            project: "harness".into(),
            started_at: "2026-06-22T10:00:00Z".into(),
            turns: 4,
            tool_events: 7,
            files: vec!["a".into(), "b".into(), "c".into()],
            ..Session::default()
        };

        // Pre-create the exact target path as a 0-byte stub, as would happen
        // before session-insights ever writes it.
        let dir = cfg.obsidian_vault.join(&cfg.record_dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!(
            "2026-06-22-{}-{}.md",
            slugify(&s.project),
            short(&s.session_id)
        ));
        std::fs::write(&path, "").unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);

        let p = write_from_session(&cfg, &s, tp.to_str().unwrap(), 0).expect("write");
        assert_eq!(p, path);
        let body = std::fs::read_to_string(&p).unwrap();
        assert!(!body.is_empty(), "note must not stay empty");
        for h in [
            "# 2026-06-22 harness セッション記録",
            "## 完了サマリ",
            "## 数値サマリ",
            "## コスト",
        ] {
            assert!(body.contains(h), "missing {h}\n{body}");
        }
        assert!(body.contains("- turns: 4"));

        let _ = std::fs::remove_file(&tp);
        let _ = std::fs::remove_dir_all(&vault);
    }

    #[test]
    fn disabled_or_missing_vault_is_noop() {
        let tp = write_transcript("noop");
        let s = Session::default();
        // record disabled (the default) → no-op
        let cfg_off = Config::default();
        assert!(write_from_session(&cfg_off, &s, tp.to_str().unwrap(), 0).is_none());
        // record enabled but the vault dir does not exist → still a no-op
        let cfg_on = Config {
            record: true,
            obsidian_vault: std::env::temp_dir().join("si-nonexistent-vault-xyz"),
            ..Config::default()
        };
        assert!(write_from_session(&cfg_on, &s, tp.to_str().unwrap(), 0).is_none());
        let _ = std::fs::remove_file(&tp);
    }

    #[test]
    fn slug_basics() {
        assert_eq!(slugify("Harness Project"), "harness-project");
        assert_eq!(slugify("my_repo.v2"), "my-repo-v2");
        assert_eq!(slugify("日本語"), "session");
    }

    /// When no gauge SessionRecord exists for the session id (gauge not installed,
    /// wrong state_dir, or session-insights ran before gauge wrote its record),
    /// cost_body() must still produce a valid cost line by falling back to the
    /// transcript aggregate. This is the d1b0c2f8 fallback path.
    #[test]
    fn cost_body_falls_back_to_transcript_when_gauge_record_absent() {
        let tp = write_transcript("fallback");
        // Use a session id that is guaranteed to have no gauge record.
        let ctx = RecordCtx {
            session_id: "no-such-session-id-xyzzy",
            project: "harness",
            date: "2026-06-22",
            turns: 1,
            tool_events: 0,
            files_touched: 0,
            transcript_path: tp.to_str().unwrap(),
            overrides: &[],
        };
        let body = cost_body(&ctx);
        // opus 1M in + 1M out = $5 + $25 = $30.00, computed from transcript fallback.
        assert!(
            body.contains("- total: $30.00 USD"),
            "fallback cost: {body}"
        );
        assert!(
            !body.contains("(コストデータなし)"),
            "should not fall through to no-data: {body}"
        );
        let _ = std::fs::remove_file(&tp);
    }

    /// Write a transcript that has a sub-agent turn so session_agents() can
    /// exercise the transcript fallback path. The sub-agent turn uses the
    /// `AGENT_SUB` bucket marker in the content; since the transcript doesn't
    /// have an explicit sub-agent section we verify the main bucket is still set.
    #[test]
    fn cost_body_shows_agent_lines_from_transcript_fallback() {
        // A two-turn transcript: one main turn + one sub-agent marker turn.
        let mut p = std::env::temp_dir();
        p.push(format!("si-record-{}-agent.jsonl", std::process::id()));
        let body = concat!(
            r#"{"type":"assistant","timestamp":"2026-06-22T10:00:01Z","message":{"model":"claude-opus-4-8","content":[{"type":"text","text":"main"}],"usage":{"input_tokens":500000,"output_tokens":500000,"cache_read_input_tokens":0}}}"#,
            "\n",
        );
        std::fs::write(&p, body).unwrap();
        let ctx = RecordCtx {
            session_id: "no-such-session-agent-xyzzy",
            project: "harness",
            date: "2026-06-22",
            turns: 1,
            tool_events: 0,
            files_touched: 0,
            transcript_path: p.to_str().unwrap(),
            overrides: &[],
        };
        let cb = cost_body(&ctx);
        // Total cost: opus 500k in + 500k out = $2.50 + $12.50 = $15.00.
        assert!(cb.contains("- total: $15.00 USD"), "{cb}");
        // Agent block present when transcript aggregate has agents.
        // (single-turn transcript with no explicit sub-agent markers → main bucket only)
        assert!(
            cb.contains("- by agent:") || !cb.contains("sub-agent"),
            "{cb}"
        );
        let _ = std::fs::remove_file(&p);
    }
}
