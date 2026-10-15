//! Deterministic retrieval: score each note against the prompt by term overlap
//! (no embeddings, no API), then select the top notes under the char budget.
//!
//! Weighting, strongest first: explicit `triggers` > `tags` > title words >
//! body words. `always` notes are injected unconditionally: they bypass both the
//! score threshold and the char budget (a normative note is never truncated or
//! dropped), and any overrun they create is shed from the scored notes instead.

use std::collections::HashSet;

use harness_core::inject::CharBudget;

use crate::config::Config;
use crate::store::Note;

const STOP: &[&str] = &[
    "the",
    "and",
    "for",
    "with",
    "this",
    "that",
    "you",
    "your",
    "are",
    "was",
    "from",
    "have",
    "してください",
    "を",
    "に",
    "は",
    "が",
    "の",
    "で",
    "と",
    "も",
    "して",
    "する",
    "した",
];

/// Tokenize to a lowercase set: ASCII words (len ≥ 2) plus each CJK char on its own.
pub fn tokenize(s: &str) -> HashSet<String> {
    let mut set = HashSet::new();
    let mut cur = String::new();
    let flush = |cur: &mut String, set: &mut HashSet<String>| {
        if cur.chars().count() >= 2 && !STOP.contains(&cur.as_str()) {
            set.insert(cur.clone());
        }
        cur.clear();
    };
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            cur.extend(c.to_lowercase());
        } else if is_cjk(c) {
            // index CJK as individual chars (no bigrams)
            if !cur.is_empty() {
                flush(&mut cur, &mut set);
            }
            set.insert(c.to_string());
        } else {
            flush(&mut cur, &mut set);
        }
    }
    flush(&mut cur, &mut set);
    set
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3040..=0x30ff |   // hiragana + katakana
        0x4e00..=0x9fff |   // CJK unified
        0xff66..=0xff9d) // halfwidth katakana
}

pub struct Scored<'a> {
    pub note: &'a Note,
    pub score: i64,
}

/// Score one note against the tokenized prompt.
fn score(note: &Note, prompt: &HashSet<String>) -> i64 {
    let mut s = 0i64;
    for t in &note.meta.triggers {
        if prompt.contains(&t.to_lowercase()) {
            s += 5;
        }
    }
    for t in &note.meta.tags {
        if prompt.contains(&t.to_lowercase()) {
            s += 3;
        }
    }
    for t in tokenize(&note.meta.title) {
        if prompt.contains(&t) {
            s += 2;
        }
    }
    // body overlap, capped so a long note can't dominate on noise
    let body_hits = tokenize(&note.body)
        .iter()
        .filter(|t| prompt.contains(*t))
        .count();
    s += (body_hits as i64).min(4);
    s
}

/// Select notes to inject for a prompt, honoring score threshold, top_k, and the
/// char budget. `always` notes come first.
pub fn select<'a>(notes: &'a [Note], prompt: &str, cfg: &Config) -> Vec<&'a Note> {
    let toks = tokenize(prompt);

    let mut scored: Vec<Scored> = notes
        .iter()
        .map(|n| Scored {
            note: n,
            score: score(n, &toks),
        })
        .collect();
    // highest score first; stable by slug for determinism
    scored.sort_by(|a, b| b.score.cmp(&a.score).then(a.note.slug.cmp(&b.note.slug)));

    let mut chosen: Vec<&Note> = Vec::new();
    let mut budget = CharBudget::new(cfg.max_chars);
    let mut seen: HashSet<&str> = HashSet::new();

    // 1) always-notes first — injected in full regardless of the char budget
    //    (normative notes must not silently drop). They still charge the budget,
    //    so any overrun they create is shed from the scored notes below.
    for n in notes.iter().filter(|n| n.meta.always) {
        push(&mut chosen, &mut budget, &mut seen, n, true);
    }
    // 2) then scored notes above threshold, up to top_k. These honor the budget,
    //    so they are the only ones dropped once `always` notes exhaust it.
    for sc in &scored {
        if chosen.len() >= cfg.top_k + chosen.iter().filter(|n| n.meta.always).count() {
            break;
        }
        if sc.score < cfg.min_score || sc.note.meta.always {
            continue;
        }
        push(&mut chosen, &mut budget, &mut seen, sc.note, false);
    }
    chosen
}

/// Admit a note into the selection. With `force` (used for `always` notes), the
/// budget is *charged* but never allowed to reject — a normative note is injected
/// in full even past `max_chars`, and the overrun it creates falls on the scored
/// (non-`always`) notes admitted afterward, which still honor `would_overflow`.
fn push<'a>(
    chosen: &mut Vec<&'a Note>,
    budget: &mut CharBudget,
    seen: &mut HashSet<&'a str>,
    n: &'a Note,
    force: bool,
) -> bool {
    if seen.contains(n.slug.as_str()) {
        return false;
    }
    let len = n.injected_len();
    if !force && budget.would_overflow(len) {
        return true; // would overflow budget
    }
    seen.insert(n.slug.as_str());
    budget.add(len);
    chosen.push(n);
    false
}

/// Render the injected knowledge block (UserPromptSubmit stdout = added context).
pub fn render_injection(notes: &[&Note]) -> String {
    let mut out = format!(
        "📒 playbook — このプロジェクトの関連ナレッジ {} 件（過去に確定した規約/地雷）。今回の作業で順守してください:\n",
        notes.len()
    );
    for n in notes {
        let title = if n.meta.title.is_empty() {
            n.slug.clone()
        } else {
            n.meta.title.clone()
        };
        let scope = if n.global { " [global]" } else { "" };
        out.push_str(&format!("\n● {title}{scope}\n{}\n", n.body.trim()));
    }
    out.push_str("\n（出典: playbook store。古ければ `playbook rm <slug>` で削除/更新を。）");
    out
}

/// Debug helper for `playbook search`.
pub fn scored_for<'a>(notes: &'a [Note], prompt: &str) -> Vec<Scored<'a>> {
    let toks = tokenize(prompt);
    let mut v: Vec<Scored> = notes
        .iter()
        .map(|n| Scored {
            note: n,
            score: score(n, &toks),
        })
        .collect();
    v.sort_by(|a, b| b.score.cmp(&a.score).then(a.note.slug.cmp(&b.note.slug)));
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Meta, Note};
    use std::path::PathBuf;

    fn note(
        slug: &str,
        title: &str,
        triggers: &[&str],
        tags: &[&str],
        body: &str,
        always: bool,
    ) -> Note {
        Note {
            slug: slug.into(),
            path: PathBuf::from(format!("{slug}.md")),
            global: false,
            meta: Meta {
                title: title.into(),
                tags: tags.iter().map(|s| s.to_string()).collect(),
                triggers: triggers.iter().map(|s| s.to_string()).collect(),
                scope: "project".into(),
                always,
                created: String::new(),
            },
            body: body.into(),
        }
    }

    fn cfg() -> Config {
        Config::default()
    }

    #[test]
    fn trigger_match_selects_note() {
        let notes = vec![note(
            "mem",
            "memory rule",
            &["lightgbm"],
            &[],
            "use chunksize",
            false,
        )];
        let got = select(&notes, "lightgbm のメモリで落ちる", &cfg());
        assert_eq!(got.len(), 1);
    }

    #[test]
    fn irrelevant_prompt_selects_nothing() {
        let notes = vec![note(
            "mem",
            "memory rule",
            &["lightgbm"],
            &[],
            "use chunksize",
            false,
        )];
        let got = select(&notes, "rename a css class", &cfg());
        assert!(got.is_empty());
    }

    #[test]
    fn always_note_injected_regardless() {
        let notes = vec![note(
            "conv",
            "core convention",
            &[],
            &[],
            "branch first",
            true,
        )];
        let got = select(&notes, "totally unrelated", &cfg());
        assert_eq!(got.len(), 1);
    }

    #[test]
    fn always_notes_exempt_from_budget() {
        // Two `always` notes whose combined length far exceeds max_chars: both
        // are injected in full anyway (normative notes never silently drop).
        let big = "x".repeat(2000);
        let mut c = cfg();
        c.max_chars = 200;
        let notes = vec![
            note("a", "a", &[], &[], &big, true),
            note("b", "b", &[], &[], &big, true),
        ];
        let got = select(&notes, "anything", &c);
        assert_eq!(got.len(), 2); // both always notes survive the budget
                                  // each is carried whole — the full body is present, not truncated
        assert!(got.iter().all(|n| n.body.len() == big.len()));
    }

    #[test]
    fn budget_drops_only_non_always() {
        // An `always` note already overruns the budget; a scored non-`always`
        // note that matches the prompt is the one shed, not the normative note.
        let big = "x".repeat(2000);
        let mut c = cfg();
        c.max_chars = 200;
        let notes = vec![
            note("norm", "core convention", &[], &[], &big, true),
            note(
                "scored",
                "memory rule",
                &["lightgbm"],
                &[],
                "use chunksize",
                false,
            ),
        ];
        let got = select(&notes, "lightgbm のメモリで落ちる", &c);
        // the always note is kept; the budgeted scored note is dropped
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].slug, "norm");
    }
}
