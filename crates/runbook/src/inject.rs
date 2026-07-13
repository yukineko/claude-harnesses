//! Prompt scanning + injection rendering.
//!
//! The hook expands `!name` macros that the user typed into the matching
//! procedure. A macro only fires when `name` resolves to an existing runbook,
//! so stray `!` in prose or code (`x != y`, `!!`, `foo!`) never injects
//! anything — there is nothing to resolve to.

use harness_core::inject::{truncate_chars, CharBudget};

use crate::config::Config;
use crate::store::Runbook;

pub struct Expansion<'a> {
    /// Matched runbooks in first-invocation order, deduped by name.
    pub matched: Vec<&'a Runbook>,
    /// The `!<index_token>` meta-macro was used (and is not a real runbook).
    pub want_index: bool,
}

impl Expansion<'_> {
    pub fn is_empty(&self) -> bool {
        self.matched.is_empty() && !self.want_index
    }
}

/// Extract macro tokens (`prefix` + `[A-Za-z0-9][A-Za-z0-9_-]*`) that are at the
/// start of the prompt or preceded by whitespace or a common opener. Returned
/// lowercased, in order, with duplicates preserved (dedup happens at resolve).
pub fn scan_tokens(prompt: &str, prefix: char) -> Vec<String> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = prompt.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == prefix {
            let boundary = i == 0 || is_opener(chars[i - 1]);
            let mut j = i + 1;
            if boundary && j < chars.len() && chars[j].is_ascii_alphanumeric() {
                let start = j;
                while j < chars.len()
                    && (chars[j].is_ascii_alphanumeric() || chars[j] == '_' || chars[j] == '-')
                {
                    j += 1;
                }
                let tok: String = chars[start..j].iter().collect();
                tokens.push(tok.to_lowercase());
                i = j;
                continue;
            }
        }
        i += 1;
    }
    tokens
}

fn is_opener(c: char) -> bool {
    c.is_whitespace() || matches!(c, '(' | '[' | '{' | ',' | '、' | '。' | '「' | '（' | '【')
}

/// Resolve scanned tokens against the available runbooks.
pub fn expand<'a>(prompt: &str, runbooks: &'a [Runbook], cfg: &Config) -> Expansion<'a> {
    let tokens = scan_tokens(prompt, cfg.prefix);
    let index_is_real = runbooks.iter().any(|r| r.matches(&cfg.index_token));
    let mut matched: Vec<&Runbook> = Vec::new();
    let mut want_index = false;

    for tok in tokens {
        if tok == cfg.index_token && !index_is_real {
            want_index = true;
            continue;
        }
        if let Some(rb) = runbooks.iter().find(|r| r.matches(&tok)) {
            if !matched.iter().any(|m| m.name == rb.name) {
                matched.push(rb);
            }
        }
    }
    Expansion {
        matched,
        want_index,
    }
}

const HEADER: &str = "<!-- runbook -->\n以下はユーザーが `!名前` で明示的に呼び出した作業手順です。対象タスクではこの手順に厳密に従い、「禁止事項 / Forbidden Actions」があれば必ず守ってください。";

/// Render the injection text, or None if there is nothing to add.
pub fn render(exp: &Expansion, all: &[Runbook], cfg: &Config) -> Option<String> {
    if exp.is_empty() {
        return None;
    }
    let mut out = String::from(HEADER);
    let mut budget = CharBudget::new(cfg.max_chars);

    for (idx, rb) in exp.matched.iter().enumerate() {
        let desc = if rb.meta.description.is_empty() {
            String::new()
        } else {
            format!(" — {}", rb.meta.description)
        };
        let body = truncate_chars(&rb.body, cfg.per_runbook_chars, "\n…（以下省略）");
        let block = format!("\n\n## 📓 {}{}\n{}", rb.name, desc, body);
        let len = block.chars().count();
        if budget.would_overflow(len) {
            out.push_str(&format!(
                "\n\n…（残り {} 件の runbook は文字数上限のため省略。`runbook show <name>` で参照）",
                exp.matched.len() - idx
            ));
            break;
        }
        out.push_str(&block);
        budget.add(len);
    }

    if exp.want_index {
        out.push_str(&render_index(all, cfg));
    }
    Some(out)
}

fn render_index(all: &[Runbook], cfg: &Config) -> String {
    if all.is_empty() {
        return format!(
            "\n\n（利用可能な runbook はありません。`runbook new <name>` で `{}` に作成できます）",
            cfg.project_dir
        );
    }
    let mut s = String::from("\n\n利用可能な runbook（`!名前` で呼び出し）:");
    for rb in all {
        let scope = if rb.global { "global" } else { "project" };
        let desc = if rb.meta.description.is_empty() {
            String::new()
        } else {
            format!(" — {}", rb.meta.description)
        };
        s.push_str(&format!(
            "\n- `{}{}`{} ({})",
            cfg.prefix, rb.name, desc, scope
        ));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Meta, Runbook};
    use std::path::PathBuf;

    fn rb(name: &str, aliases: &[&str]) -> Runbook {
        Runbook {
            name: name.into(),
            path: PathBuf::new(),
            global: false,
            meta: Meta {
                description: String::new(),
                aliases: aliases.iter().map(|s| s.to_string()).collect(),
            },
            body: format!("procedure for {name}"),
        }
    }

    #[test]
    fn scans_boundaried_tokens_only() {
        let toks = scan_tokens("run !deploy then !test", '!');
        assert_eq!(toks, vec!["deploy", "test"]);
    }

    #[test]
    fn ignores_non_boundary_and_operators() {
        // `!=`, `!!`, and mid-word `!` must not produce tokens.
        assert!(scan_tokens("if x != y", '!').is_empty());
        assert!(scan_tokens("history !! repeat", '!').is_empty());
        assert!(scan_tokens("foo!bar baz", '!').is_empty());
    }

    #[test]
    fn token_after_opener() {
        let toks = scan_tokens("(!deploy) and 「!test」", '!');
        assert_eq!(toks, vec!["deploy", "test"]);
    }

    #[test]
    fn expand_matches_and_dedupes() {
        let books = vec![rb("deploy", &["ship"]), rb("test", &[])];
        let cfg = Config::default();
        let e = expand("!deploy !ship !test !deploy", &books, &cfg);
        // deploy (once), then test; !ship is an alias of deploy so deduped.
        assert_eq!(e.matched.len(), 2);
        assert_eq!(e.matched[0].name, "deploy");
        assert_eq!(e.matched[1].name, "test");
        assert!(!e.want_index);
    }

    #[test]
    fn unknown_token_injects_nothing() {
        let books = vec![rb("deploy", &[])];
        let cfg = Config::default();
        let e = expand("please !nonexistent now", &books, &cfg);
        assert!(e.is_empty());
        assert!(render(&e, &books, &cfg).is_none());
    }

    #[test]
    fn index_token_requests_index() {
        let books = vec![rb("deploy", &[])];
        let cfg = Config::default();
        let e = expand("what can I do? !runbooks", &books, &cfg);
        assert!(e.want_index);
        let out = render(&e, &books, &cfg).unwrap();
        assert!(out.contains("!deploy"));
    }
}
