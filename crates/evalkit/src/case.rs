//! Golden eval case schema + JSONL parsing.
//!
//! One case per JSONL line. A case names a *subject* — either a `file` (whose
//! contents are read) or a `cmd` (whose stdout is captured) — and a set of
//! assertions over that subject's text. The schema is intentionally small so a
//! golden case is readable in a diff and authorable by hand.
//!
//! ```jsonl
//! {"id":"flow-keeps-blind-exec","file":"crates/flow/skills/flow/SKILL.md","assert":{"contains":["盲目実行しない"]}}
//! {"id":"evalkit-version","cmd":["evalkit","--version"],"assert":{"exit":0,"regex":["evalkit \\d+\\.\\d+"]}}
//! ```

use anyhow::{bail, Context, Result};
use serde::Deserialize;

/// One golden case: a subject (`file` xor `cmd`) plus assertions over it.
#[derive(Debug, Deserialize)]
pub struct Case {
    pub id: String,
    #[serde(default)]
    pub describe: String,
    /// Read this file's contents as the subject (path relative to `--root`).
    #[serde(default)]
    pub file: Option<String>,
    /// Run this command and capture stdout as the subject. `cmd[0]` is the
    /// program; the rest are args.
    #[serde(default)]
    pub cmd: Option<Vec<String>>,
    /// Optional stdin piped to the `cmd` subject.
    #[serde(default)]
    pub stdin: Option<String>,
    #[serde(default)]
    pub assert: Assert,
    /// A promotion draft awaiting a human-authored assertion (emitted by
    /// `curate` when it can't auto-derive one). Draft cases are *skipped* at run
    /// time — never passed, never failed — so an unfilled golden can sit in the
    /// repo without breaking the gate, yet stays visible as pending work.
    #[serde(default)]
    pub draft: bool,
}

/// Assertions over a subject's text. Each field is optional, but a non-draft
/// case must carry at least one: an empty `Assert` asserts nothing, so the
/// runner fails it rather than reporting a vacuous pass. Independently of the
/// assertions, a `cmd` subject must exit `0` unless `exit` names another code.
#[derive(Debug, Deserialize, Default)]
pub struct Assert {
    /// Expected process exit code. Only meaningful for `cmd` cases. When
    /// omitted, a `cmd` case still fails on any non-zero exit.
    #[serde(default)]
    pub exit: Option<i32>,
    /// Substrings that MUST be present in the subject.
    #[serde(default)]
    pub contains: Vec<String>,
    /// Substrings that must NOT be present in the subject.
    #[serde(default)]
    pub not_contains: Vec<String>,
    /// Regexes that MUST match somewhere in the subject.
    #[serde(default)]
    pub regex: Vec<String>,
    /// Regexes that must NOT match anywhere in the subject.
    #[serde(default)]
    pub not_regex: Vec<String>,
}

impl Case {
    /// Short one-line label for reports: `id` plus `describe` if present.
    pub fn label(&self) -> String {
        if self.describe.is_empty() {
            self.id.clone()
        } else {
            format!("{} — {}", self.id, self.describe)
        }
    }
}

/// Parse a JSONL document into cases. Blank lines and `//`-prefixed comment
/// lines are skipped so a golden file can be sectioned and annotated. `source`
/// labels error locations (`path:line`).
pub fn parse_jsonl(text: &str, source: &str) -> Result<Vec<Case>> {
    let mut cases = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        let case: Case = serde_json::from_str(line)
            .with_context(|| format!("{source}:{}: invalid JSON case", i + 1))?;
        validate(&case).with_context(|| format!("{source}:{}", i + 1))?;
        cases.push(case);
    }
    Ok(cases)
}

/// A case must name exactly one subject and a non-empty id. Draft cases are
/// exempt from the subject requirement — they carry no runnable assertion yet
/// and are skipped at run time.
fn validate(c: &Case) -> Result<()> {
    if c.id.trim().is_empty() {
        bail!("case `id` must not be empty");
    }
    if c.draft {
        return Ok(());
    }
    match (c.file.is_some(), c.cmd.is_some()) {
        (true, true) => bail!("case '{}' has both `file` and `cmd` (pick one)", c.id),
        (false, false) => bail!("case '{}' has neither `file` nor `cmd`", c.id),
        _ => {}
    }
    if let Some(cmd) = &c.cmd {
        if cmd.is_empty() {
            bail!("case '{}' has an empty `cmd`", c.id);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_file_and_cmd_cases_skipping_comments_and_blanks() {
        let doc = r#"
// a comment line
{"id":"f","file":"a.md","assert":{"contains":["x"]}}

{"id":"c","cmd":["echo","hi"],"assert":{"exit":0}}
"#;
        let cases = parse_jsonl(doc, "t.jsonl").unwrap();
        assert_eq!(cases.len(), 2);
        assert!(cases[0].cmd.is_none() && cases[0].file.is_some());
        assert!(cases[1].cmd.is_some() && cases[1].file.is_none());
    }

    #[test]
    fn rejects_case_with_both_subjects() {
        let doc = r#"{"id":"bad","file":"a","cmd":["b"],"assert":{}}"#;
        let err = format!("{:#}", parse_jsonl(doc, "t.jsonl").unwrap_err());
        assert!(err.contains("both"), "{err}");
    }

    #[test]
    fn rejects_case_with_no_subject() {
        let doc = r#"{"id":"bad","assert":{}}"#;
        let err = format!("{:#}", parse_jsonl(doc, "t.jsonl").unwrap_err());
        assert!(err.contains("neither"), "{err}");
    }

    #[test]
    fn rejects_invalid_json_with_line_number() {
        let doc = "{not json}";
        let err = parse_jsonl(doc, "t.jsonl").unwrap_err().to_string();
        assert!(err.contains("t.jsonl:1"), "{err}");
    }

    #[test]
    fn empty_assert_defaults_are_all_empty() {
        let doc = r#"{"id":"f","file":"a.md"}"#;
        let cases = parse_jsonl(doc, "t.jsonl").unwrap();
        assert!(cases[0].assert.contains.is_empty());
        assert!(cases[0].assert.exit.is_none());
    }

    #[test]
    fn draft_case_is_exempt_from_subject_requirement() {
        // A draft has no file/cmd yet — must still parse (it is skipped at run).
        let doc = r#"{"id":"d","describe":"promote refresh-token flow","draft":true}"#;
        let cases = parse_jsonl(doc, "t.jsonl").unwrap();
        assert_eq!(cases.len(), 1);
        assert!(cases[0].draft);
        assert!(cases[0].file.is_none() && cases[0].cmd.is_none());
    }
}
