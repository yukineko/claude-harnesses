//! `ctxrot distill-bg` — async, high-quality distill on compaction (feature ④).
//!
//! PreCompact / PostCompact cannot inject context, and the model can't invoke
//! `/compact` or a skill on its own — so the deterministic `rescue` note is the
//! only thing that lands synchronously before a `/compact`. That note is a cheap
//! regex skim of the recent window; the *quality* of what's rescued is lossy.
//!
//! This module closes that gap WITHOUT a new hook and without blocking compaction:
//!   1. `spawn_detached` is called from the PreCompact `rescue` path. It fire-and-
//!      forgets a DETACHED `ctxrot distill-bg` (via `nohup … &`) so it outlives the
//!      10s hook and the compaction itself.
//!   2. `run_bg` (this binary, `distill-bg` subcommand) reads the *pre-compaction*
//!      transcript, runs the configured headless model (`claude -p`, same auth as
//!      the session → subscription, no API key) to produce a real distill note,
//!      writes it as a `distill-*` note (so `restore`/anchor/prune treat it as the
//!      high-value note), and drops a `<state_dir>/<safe>.distilled` marker.
//!   3. The next `guard` (UserPromptSubmit) consumes that marker and re-injects the
//!      distilled Decisions/todos — the post-compaction in-session recovery that no
//!      hook can do directly.
//!
//! Recursion is impossible: the spawned `claude -p` runs with `GUARD_DISABLE=1`
//! (every ctxrot hook no-ops in the child) and `CTXROT_DISTILL_CHILD=1`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use wait_timeout::ChildExt;

use crate::config::Config;
use crate::hooks::guard::safe_session;
use crate::hooks::restore::{has_section, REQUIRED_SECTIONS};
use harness_core::hook::HookInput;
use harness_core::store::{save_bytes, session_tag, Store};
use harness_core::transcript::{self, Turn};

/// How much of the conversation we feed the distiller. Far more generous than the
/// deterministic rescue (60×1200) — the whole point is a higher-fidelity pass —
/// but still bounded so a runaway transcript can't blow up the model call.
const DISTILL_MAX_TURNS: usize = 200;
const DISTILL_MAX_TURN_CHARS: usize = 3000;

/// Env flag set on the child so a (theoretically) nested invocation never re-spawns.
const CHILD_ENV: &str = "CTXROT_DISTILL_CHILD";

/// Path of the "a fresh distill landed, re-inject it next turn" marker for a
/// session. `guard` reads+deletes it; `run_bg` writes it. Single source of truth
/// so both sides agree on the filename.
pub(crate) fn marker_path(cfg: &Config, session_id: &str) -> PathBuf {
    cfg.state_dir
        .join(format!("{}.distilled", safe_session(session_id)))
}

/// From the PreCompact `rescue` path: fire-and-forget a detached background distill
/// if it's enabled and we're not already inside a distill child. Never blocks, never
/// fails the hook — any error just means no high-quality note this time (the
/// deterministic rescue note already landed as the safety net).
pub fn spawn_detached(input: &HookInput, cfg: &Config) {
    if !cfg.distill_on_compact {
        return;
    }
    fire(input);
}

/// From the guard's danger-band crossing (UserPromptSubmit, feature ⑤): fire the
/// SAME detached distill *proactively* at ≈200k, before any `/compact`. This is
/// how "auto-distill at 200k" is realized — hooks can't trigger compaction, so we
/// externalize the heavy history to a `distill-*` note now and let the next guard
/// re-inject the summary. Gated on `auto_distill_on_band` (independent of the
/// on-compact path). The guard's escalate-only band gate already limits this to
/// once per upward crossing, so it spends a bounded number of model calls.
pub fn spawn_for_band(input: &HookInput, cfg: &Config) {
    if !cfg.auto_distill_on_band {
        return;
    }
    fire(input);
}

/// Shared spawn core: fire-and-forget a detached `ctxrot distill-bg` for this
/// session's transcript. No-op inside a distill child (never recurse) or with no
/// transcript. Never blocks, never fails the caller — any error just means no
/// high-quality note this time (the deterministic rescue note is the safety net).
fn fire(input: &HookInput) {
    if std::env::var(CHILD_ENV).is_ok() {
        return; // defensive: never recurse
    }
    if input.transcript_path.is_empty() {
        return;
    }
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return,
    };
    let cwd = input.cwd_or_current();

    // `nohup … &` inside `sh -c` reparents the worker to init, so it survives this
    // hook exiting (or being killed at the 10s timeout). All of the worker's std
    // streams are detached from the hook's pipes.
    let line = format!(
        "nohup {} distill-bg --session {} --transcript {} --cwd {} >/dev/null 2>&1 &",
        shq(&exe.to_string_lossy()),
        shq(&input.session_id),
        shq(&input.transcript_path),
        shq(&cwd.to_string_lossy()),
    );
    let _ = harness_core::shell::command(&line)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .and_then(|mut c| c.wait()); // sh returns immediately after backgrounding
}

/// The detached worker (the `distill-bg` subcommand). Runs the headless model on
/// the pre-compaction transcript and writes a `distill-*` note + a re-inject
/// marker. Best-effort throughout: any failure leaves the deterministic rescue
/// note as the durable record.
pub fn run_bg(session_id: &str, transcript_path: &str, cwd: &Path, cfg: &Config) {
    if transcript_path.is_empty() {
        return;
    }
    // Forwarded, not swallowed: "could not read the transcript" is a different
    // fact from "there was nothing to distill", and only the second warrants
    // silence. The deterministic rescue note is the fallback record either way,
    // but a distill that never ran must not look like one that had no material.
    let turns = match transcript::recent_turns(
        transcript_path,
        DISTILL_MAX_TURNS,
        DISTILL_MAX_TURN_CHARS,
    ) {
        harness_core::verdict::Determination::Known(t) => t,
        harness_core::verdict::Determination::Undetermined(why) => {
            eprintln!(
                "ctxrot distill: transcript を読めなかったため蒸留をスキップします\
                     （素材が無かったのではありません）: {transcript_path} — {why}"
            );
            return;
        }
    };
    if turns.is_empty() {
        return;
    }

    let prompt = build_prompt(cwd, &turns);
    let raw = match run_model(&cfg.distill_cmd, &prompt, cfg.distill_timeout_secs) {
        Some(s) if !s.trim().is_empty() => s,
        _ => return, // timeout / empty / spawn failure → rescue note stands
    };

    let body = finalize_note(cwd, session_id, &raw);

    let now = chrono::Local::now();
    let slug = format!(
        "distill-{}-{}",
        session_tag(session_id),
        now.format("%Y%m%d-%H%M%S")
    );
    let store = Store::new(cfg.store_dir.clone());
    let Ok(path) = store.write_note(cwd, &slug, &body) else {
        return;
    };

    // Signal the next guard turn to re-inject this note (post-compact recovery).
    // Durable write (temp + fsync + rename): the note is already persisted, so a
    // crash mid-marker-write must never leave a torn/half pointer that the next
    // turn can't parse — the marker is either the complete note path or absent
    // (falling back to the deterministic rescue note). `save_bytes` also creates
    // the parent state dir.
    save_bytes(
        &marker_path(cfg, session_id),
        path.to_string_lossy().as_bytes(),
    );

    crate::metrics::emit(
        cfg,
        session_id,
        "distill",
        serde_json::json!({
            "note": path.to_string_lossy(),
            "note_bytes": body.len(),
            "turns": turns.len(),
        }),
    );
}

/// Render the bounded transcript + the distill contract into the model prompt.
fn build_prompt(cwd: &Path, turns: &[Turn]) -> String {
    let proj = cwd
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("project");
    let mut s = String::new();
    s.push_str(
        "あなたは会話の蒸留器です。以下は context compaction 直前の会話ログ（直近の抜粋）です。\n\
         後続作業に必要な結論だけを残し、試行錯誤の経過は捨てて、Markdown ノートを出力してください。\n\n\
         厳守:\n\
         - 出力は Markdown 本文のみ（前置き・コードフェンス・説明文は禁止）。\n\
         - 次の見出しを必ず含める（空なら `_(なし / none)_`）:\n\
         \x20 ## 決定事項 / Decisions\n\
         \x20 ## 残課題 / Open todos\n\
         - 可能なら次も含める: ## 触ったファイル / Files, ## 重要な事実 / Key facts, ## 現在地 / Where we are\n\
         - 箇条書き中心・簡潔に。生ログの貼り直しは禁止。\n\n",
    );
    s.push_str(&format!("project: {proj}\n\n---- 会話ログ ----\n\n"));
    for t in turns {
        let who = if t.role == "user" {
            "🧑 user"
        } else {
            "🤖 assistant"
        };
        s.push_str(&format!("**{who}:** {}\n\n", t.text.replace('\n', " ")));
    }
    s
}

/// Run the configured headless command, feeding `prompt` on stdin, capturing
/// stdout, killing it at `timeout_secs`. None on any failure/timeout.
fn run_model(cmdline: &str, prompt: &str, timeout_secs: u64) -> Option<String> {
    let mut cmd = build_command(cmdline);
    // No ctxrot hook may fire inside the child, and it must never re-spawn a distill.
    cmd.env("GUARD_DISABLE", "1")
        .env(CHILD_ENV, "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    // Strip secret-bearing vars so the headless child can't inherit an API key
    // from the parent session. The distill child authenticates via the user's
    // Claude subscription, not an API key, so these are never needed — removing
    // them (rather than a full env_clear, which would also drop PATH/HOME the
    // child relies on) avoids leaking a key into the spawned process.
    for key in [
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "ANTHROPIC_BASE_URL",
        "CLAUDE_API_KEY",
        "CLAUDE_CODE_OAUTH_TOKEN",
    ] {
        cmd.env_remove(key);
    }

    let mut child = cmd.spawn().ok()?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(prompt.as_bytes());
        // drop stdin → EOF so the model starts generating
    }
    match child.wait_timeout(Duration::from_secs(timeout_secs)) {
        Ok(Some(status)) if status.success() => {}
        Ok(Some(_)) => return None,
        Ok(None) => {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        Err(_) => return None,
    }
    let out = child.wait_with_output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Split a command line into program + args, falling back to `sh -c` when it
/// carries shell metacharacters. Mirrors reviewgate's `build_command`.
fn build_command(cmdline: &str) -> Command {
    let cmdline = cmdline.trim();
    if cmdline.contains(['|', '&', ';', '>', '<', '$', '`', '(', ')']) {
        return harness_core::shell::command(cmdline);
    }
    let mut parts = cmdline.split_whitespace();
    let prog = parts.next().unwrap_or("claude");
    let mut c = Command::new(prog);
    for a in parts {
        c.arg(a);
    }
    c
}

/// Turn the model's markdown into a durable note: strip an accidental code fence,
/// guarantee the load-bearing headings exist (so `restore` never silently empties),
/// and prepend frontmatter so `note_freshness` and the distill provenance hold.
fn finalize_note(cwd: &Path, session_id: &str, raw: &str) -> String {
    let mut md = strip_fence(raw).trim().to_string();

    // Contract: if the model dropped a required heading, append an empty one so the
    // note still satisfies `restore`/`note write --require-sections`.
    for (label, aliases) in REQUIRED_SECTIONS {
        if !has_section(&md, aliases) {
            md.push_str(&format!("\n\n## {label}\n\n_(なし / none)_\n"));
        }
    }

    let proj = cwd
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("project");
    let iso = chrono::Local::now()
        .format("%Y-%m-%dT%H:%M:%S%:z")
        .to_string();
    let mut s = String::new();
    s.push_str("---\n");
    s.push_str("type: ctxrot-distill\n");
    s.push_str(&format!("project: {proj}\n"));
    s.push_str(&format!("session: {session_id}\n"));
    s.push_str("trigger: precompact-async\n");
    s.push_str(&format!("created: {iso}\n"));
    s.push_str("---\n\n");
    s.push_str(&md);
    s.push('\n');
    s
}

/// Drop a single surrounding ```…``` fence if the model wrapped the whole note.
fn strip_fence(s: &str) -> String {
    let t = s.trim();
    if let Some(rest) = t.strip_prefix("```") {
        // skip an optional language tag on the opening fence line
        let after_lang = rest.split_once('\n').map(|(_, b)| b).unwrap_or("");
        if let Some(inner) = after_lang.rfind("```") {
            return after_lang[..inner].to_string();
        }
    }
    t.to_string()
}

/// Single-quote a string for safe inclusion in a `sh -c` line.
fn shq(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finalize_adds_missing_required_headings() {
        // Model returned only Decisions → Open todos must be appended as a
        // conformant empty section so restore can consume it.
        let note = finalize_note(
            Path::new("/tmp/proj"),
            "sess-x",
            "## 決定事項 / Decisions\n\n- A を採用\n",
        );
        assert!(has_section(&note, &["決定事項", "Decisions"]));
        assert!(has_section(&note, &["残課題", "Open todos", "todos"]));
        assert!(super::super::restore::missing_sections(&note).is_empty());
        assert!(note.contains("type: ctxrot-distill"));
    }

    #[test]
    fn strip_fence_unwraps_block() {
        let raw = "```markdown\n## 決定事項 / Decisions\n\n- A\n```";
        assert_eq!(strip_fence(raw).trim(), "## 決定事項 / Decisions\n\n- A");
    }

    #[test]
    fn strip_fence_leaves_plain_markdown() {
        let raw = "## 決定事項 / Decisions\n\n- A\n";
        assert_eq!(strip_fence(raw).trim(), "## 決定事項 / Decisions\n\n- A");
    }

    #[test]
    fn shq_escapes_quotes() {
        assert_eq!(shq("/a/b c"), "'/a/b c'");
        assert_eq!(shq("it's"), r"'it'\''s'");
    }

    #[test]
    fn build_command_plain_vs_shell() {
        // plain → prog + args; shell metachars → sh -c (smoke: it doesn't panic)
        let _ = build_command("claude -p");
        let _ = build_command("foo | bar");
    }

    #[test]
    fn marker_path_uses_safe_session() {
        let cfg = Config {
            state_dir: PathBuf::from("/tmp/ctxrot-state"),
            ..Config::default()
        };
        let p = marker_path(&cfg, "abc/def:1");
        assert!(p.ends_with("abc_def_1.distilled"), "got {}", p.display());
    }
}
