//! `ctxrot rescue` — PreCompact hook.
//!
//! Fires right before `/compact` (manual or auto). Compaction is lossy and
//! opaque; this is a cheap deterministic safety net that streams the recent
//! transcript and writes a durable markdown "rescue note" so decisions, open
//! todos, touched files and conclusions survive even if the summary drops them.
//!
//! No LLM here (PreCompact has a tight timeout). High-quality summarization is
//! the job of the `/distill` skill — or, when `distill_on_compact` is enabled, of
//! the detached async distiller this hook fire-and-forgets (see `hooks::distill`).

use std::path::{Path, PathBuf};

use regex::Regex;

use crate::config::Config;
use harness_core::hook::HookInput;
use harness_core::store::{project_key, session_tag, Store};
use harness_core::transcript::{self, Turn};

const MAX_TURNS: usize = 60;
const MAX_TURN_CHARS: usize = 1200;

/// Run the rescue (PreCompact). Returns the written note path (for logging), or
/// None if there was nothing to save.
pub fn run(input: &HookInput, cfg: &Config) -> Option<PathBuf> {
    let trigger = if input.trigger.is_empty() {
        "precompact"
    } else {
        &input.trigger
    };
    write(input, cfg, trigger)
}

/// Write a rescue note NOW under the given `trigger` label, returning its path.
///
/// Shared by the PreCompact hook (`run`, trigger=`precompact`/auto) and by
/// guard's *preemptive* band-crossing rescue (trigger=`band-NN%`). The latter
/// keeps the durable note fresh so a manual `/compact` or `/clear` is safe even
/// though `/clear` never fires PreCompact.
pub fn write(input: &HookInput, cfg: &Config, trigger: &str) -> Option<PathBuf> {
    if input.transcript_path.is_empty() {
        return None;
    }

    let cwd = input.cwd_or_current();
    let store = Store::new(cfg.store_dir.clone());

    // Coalescing (P3): a manual band 2→3 climb + `/compact` can spawn several
    // near-identical rescues in minutes. Skip a *preemptive* (`band-NN%`) write
    // when this session already has a fresh rescue. PreCompact/auto are NEVER
    // coalesced — they fire right before real loss, so one must always land.
    if trigger.starts_with("band-") && cfg.rescue_coalesce_secs > 0 {
        match store.recent_session_rescue(&cwd, &input.session_id, cfg.rescue_coalesce_secs) {
            harness_core::verdict::Determination::Known(Some(existing)) => return Some(existing),
            harness_core::verdict::Determination::Known(None) => {}
            // Cannot tell whether a fresh rescue already exists. Fall through
            // and write one: a duplicate note costs disk, a skipped one costs
            // the carryover this hook exists to preserve. Reached by *seeing*
            // the undetermined answer, not by mistaking it for "none exists".
            harness_core::verdict::Determination::Undetermined(why) => {
                eprintln!(
                    "ctxrot rescue: 既存の退避ノートを確認できませんでした（重複を許して\
                     新規作成します）: {why}"
                );
            }
        }
    }

    let turns = transcript::recent_turns(&input.transcript_path, MAX_TURNS, MAX_TURN_CHARS);
    if turns.is_empty() {
        return None;
    }

    let extracted = extract(&turns);
    let pct = transcript::estimate_tokens(&input.transcript_path)
        .map(|(t, _)| (t as f64 / cfg.context_window as f64 * 100.0) as i64);

    let now = chrono::Local::now();
    let iso = now.format("%Y-%m-%dT%H:%M:%S%:z").to_string();
    // Session tag in the filename so this session can find its own rescue note
    // even when parallel sessions write into the same project dir.
    let slug = format!(
        "rescue-{}-{}",
        session_tag(&input.session_id),
        now.format("%Y%m%d-%H%M%S")
    );
    let body = render_note(&cwd, input, trigger, &iso, pct, &extracted, &turns);

    let path = store.write_note(&cwd, &slug, &body).ok();
    if let Some(p) = &path {
        crate::metrics::emit(
            cfg,
            &input.session_id,
            "rescue",
            serde_json::json!({
                "trigger": trigger,
                "note": p.to_string_lossy(),
                "note_bytes": body.len(),
                "pct": pct,
                "decisions": extracted.decisions.len(),
                "todos": extracted.todos.len(),
            }),
        );
    }
    path
}

struct Extracted {
    decisions: Vec<String>,
    todos: Vec<String>,
    files: Vec<String>,
    links: Vec<String>,
}

fn decision_re() -> Regex {
    Regex::new(r"(?i)(決定|方針|採用|will use|decided|let's use|going with|に統一|を採用)").unwrap()
}
fn todo_re() -> Regex {
    Regex::new(r"(?i)(残課題|次に|あとで|todo|fixme|未対応|next step|follow.?up|要対応)").unwrap()
}
fn url_re() -> Regex {
    Regex::new(r#"https?://[^\s'"<>)\]]+"#).unwrap()
}
fn path_re() -> Regex {
    // path-ish tokens with a code/content extension OR an absolute path
    Regex::new(r"(?:~?/[\w./\-]+|[\w./\-]+\.(?:rs|py|ts|tsx|js|jsx|go|java|c|cpp|h|toml|yaml|yml|json|md|sql|sh|rb|html|css))").unwrap()
}

fn extract(turns: &[Turn]) -> Extracted {
    let d_re = decision_re();
    let t_re = todo_re();
    let u_re = url_re();
    let p_re = path_re();

    let mut decisions = Vec::new();
    let mut todos = Vec::new();
    let mut files: Vec<String> = Vec::new();
    let mut links: Vec<String> = Vec::new();

    for turn in turns {
        for line in turn.text.lines() {
            let l = line.trim();
            if l.is_empty() {
                continue;
            }
            // Mutually exclusive: a line is a decision OR a todo (decision wins)
            // to avoid the same line appearing in both buckets.
            if d_re.is_match(l) {
                if decisions.len() < 30 {
                    decisions.push(clip(l));
                }
            } else if t_re.is_match(l) && todos.len() < 30 {
                todos.push(clip(l));
            }
        }
        for m in u_re.find_iter(&turn.text) {
            let s = m.as_str().to_string();
            if !links.contains(&s) && links.len() < 30 {
                links.push(s);
            }
        }
        for m in p_re.find_iter(&turn.text) {
            let s = m.as_str().trim_end_matches(['.', ',', ')']).to_string();
            // skip bare URLs already captured and trivially short tokens
            if s.len() >= 4 && !s.starts_with("http") && !files.contains(&s) && files.len() < 40 {
                files.push(s);
            }
        }
    }

    dedup_keep_order(&mut decisions);
    dedup_keep_order(&mut todos);
    Extracted {
        decisions,
        todos,
        files,
        links,
    }
}

fn clip(s: &str) -> String {
    transcript::truncate_chars(s, 240)
}

fn dedup_keep_order(v: &mut Vec<String>) {
    let mut seen = std::collections::HashSet::new();
    v.retain(|x| seen.insert(x.clone()));
}

fn render_note(
    cwd: &Path,
    input: &HookInput,
    trigger: &str,
    iso: &str,
    pct: Option<i64>,
    ex: &Extracted,
    turns: &[Turn],
) -> String {
    let proj = cwd
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("project");
    let pct_s = pct.map(|p| format!("~{p}%")).unwrap_or_else(|| "?".into());

    let mut s = String::new();
    s.push_str("---\n");
    s.push_str("type: ctxrot-rescue\n");
    s.push_str(&format!("project: {proj}\n"));
    s.push_str(&format!("project_key: {}\n", project_key(cwd)));
    s.push_str(&format!("session: {}\n", input.session_id));
    s.push_str(&format!("trigger: {trigger}\n"));
    s.push_str(&format!("context: {pct_s}\n"));
    s.push_str(&format!("created: {iso}\n"));
    s.push_str("---\n\n");

    s.push_str(&format!("# ctxrot rescue {iso} (project: {proj})\n\n"));
    s.push_str(&format!(
        "退避ノート（trigger: {trigger}, context: {pct_s}）。compact/clear で失われる前の素材保全。\n\n"
    ));

    push_bullets(&mut s, "決定事項 / Decisions", &ex.decisions);
    push_bullets(&mut s, "残課題 / Open todos", &ex.todos);
    push_bullets(&mut s, "触ったファイル / Files", &ex.files);
    push_bullets(&mut s, "成果物・リンク / Links", &ex.links);

    s.push_str("## Raw 抽出（直近の会話）\n\n");
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

fn push_bullets(s: &mut String, title: &str, items: &[String]) {
    s.push_str(&format!("## {title}\n\n"));
    if items.is_empty() {
        s.push_str("_(なし / none)_\n\n");
    } else {
        for it in items {
            s.push_str(&format!("- {it}\n"));
        }
        s.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_buckets() {
        let turns = vec![
            Turn {
                role: "assistant".into(),
                text: "方針: serde を採用する。\n次に tests を書く。\nsrc/main.rs を編集した。\nhttps://docs.rs/serde 参照".into(),
            },
        ];
        let ex = extract(&turns);
        assert!(ex.decisions.iter().any(|d| d.contains("採用")));
        assert!(ex.todos.iter().any(|t| t.contains("次に")));
        assert!(ex.files.iter().any(|f| f.contains("src/main.rs")));
        assert!(ex.links.iter().any(|l| l.contains("docs.rs")));
    }

    #[test]
    fn preemptive_coalesces_precompact_does_not() {
        // Auto-cleaned unique temp dir (atomic mkdtemp, no pid-collision TOCTOU).
        let tmp = tempfile::tempdir().expect("tempdir");
        let base = tmp.path();
        let cwd = base.join("proj");
        std::fs::create_dir_all(&cwd).unwrap();
        let cfg = Config {
            state_dir: base.join("state"),
            store_dir: base.join("store"),
            ..Config::default() // rescue_coalesce_secs = 120
        };
        let input = HookInput {
            session_id: "sess-coalesce".into(),
            transcript_path: "tests/fixtures/transcript.jsonl".into(),
            cwd: cwd.to_string_lossy().into_owned(),
            ..HookInput::default()
        };

        let p1 = write(&input, &cfg, "band-80%").expect("first preemptive writes");
        assert!(std::fs::read_to_string(&p1)
            .unwrap()
            .contains("trigger: band-80%"));

        // Second preemptive within the window → coalesced: same path, no new file,
        // original content untouched.
        let p2 = write(&input, &cfg, "band-85%").expect("coalesced path returned");
        assert_eq!(p1, p2);
        assert!(std::fs::read_to_string(&p2)
            .unwrap()
            .contains("trigger: band-80%"));

        // PreCompact is never coalesced — it writes a fresh note (here onto the
        // same per-second slug, so content flips to the precompact trigger).
        let p3 = write(&input, &cfg, "precompact").expect("precompact always writes");
        assert!(std::fs::read_to_string(&p3)
            .unwrap()
            .contains("trigger: precompact"));
    }
}
