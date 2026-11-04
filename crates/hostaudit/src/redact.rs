//! Secret removal (canon §8, CONTRACT §8).
//!
//! Three rules, applied to everything hostaudit writes (stdout, stderr,
//! `reports.jsonl`, the overwatch ledger):
//!
//! 1. a line containing `-----BEGIN` is dropped entirely;
//! 2. every run of >= 40 characters of `[A-Za-z0-9_/+=-]` is replaced, in
//!    place, by `<redacted>` (the rest of the line stays);
//! 3. registered literal secrets — AWS credential env values and the `env`
//!    values of Claude Code settings files — are replaced by `<redacted>`
//!    wherever they occur, whatever their length.
//!
//! Rules 1 and 2 are applied to free-text fields (evidence, reason, fix) when a
//! finding is built; rule 3 and the line drop of rule 1 are applied again to
//! every byte stream on the way out ([`for_output`]), so a secret that reaches
//! an output by some other path is still removed.

use std::sync::Mutex;

pub const REDACTED: &str = "<redacted>";
const BEGIN: &str = "-----BEGIN";
const MIN_TOKEN: usize = 40;
/// Evidence entries are capped at this many bytes (canon §8).
pub const EVIDENCE_MAX: usize = 512;

static SECRETS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Register a literal secret value that must never be written anywhere.
/// Empty values are ignored (an empty needle would match everywhere).
pub fn register_secret(value: &str) {
    let v = value.trim();
    if v.is_empty() {
        return;
    }
    let mut g = SECRETS.lock().unwrap_or_else(|p| p.into_inner());
    if !g.iter().any(|s| s == v) {
        g.push(v.to_string());
        // Longest first, so a secret that contains another is replaced whole.
        g.sort_by_key(|s| std::cmp::Reverse(s.len()));
    }
}

/// Register the AWS credential environment values of this process.
pub fn register_aws_env() {
    for k in [
        "AWS_ACCESS_KEY_ID",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_SESSION_TOKEN",
        "AWS_SECURITY_TOKEN",
    ] {
        if let Ok(v) = std::env::var(k) {
            register_secret(&v);
        }
    }
}

fn scrub_literals(s: &str) -> String {
    let g = SECRETS.lock().unwrap_or_else(|p| p.into_inner());
    let mut out = s.to_string();
    for secret in g.iter() {
        if out.contains(secret.as_str()) {
            out = out.replace(secret.as_str(), REDACTED);
        }
    }
    out
}

fn token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '/' | '+' | '=' | '-')
}

/// Rule 2 on one line: long token runs replaced in place.
pub fn redact_tokens(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| {
        if run.chars().count() >= MIN_TOKEN {
            out.push_str(REDACTED);
        } else {
            out.push_str(run);
        }
        run.clear();
    };
    for c in line.chars() {
        if token_char(c) {
            run.push(c);
        } else {
            flush(&mut run, &mut out);
            out.push(c);
        }
    }
    flush(&mut run, &mut out);
    out
}

/// One free-text line through rules 1–3. `None` = the line must be dropped.
pub fn clean_line(line: &str) -> Option<String> {
    if line.contains(BEGIN) {
        return None;
    }
    let scrubbed = scrub_literals(line);
    Some(redact_tokens(&scrubbed))
}

/// Multi-line free text through rules 1–3; dropped lines vanish, the remaining
/// lines are joined with `sep`.
pub fn clean_text(text: &str, sep: &str) -> String {
    text.lines()
        .filter_map(clean_line)
        .collect::<Vec<_>>()
        .join(sep)
}

/// Truncate to at most `max` bytes on a char boundary.
pub fn cap_bytes(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// Evidence entries: split into lines, cleaned, each capped at 512 bytes.
pub fn clean_evidence(items: Vec<String>) -> Vec<String> {
    items
        .iter()
        .flat_map(|e| e.lines().map(str::to_string).collect::<Vec<_>>())
        .filter_map(|l| clean_line(&l))
        .filter(|l| !l.trim().is_empty())
        .map(|l| cap_bytes(&l, EVIDENCE_MAX))
        .collect()
}

/// Final pass over an output stream: `-----BEGIN` lines dropped, registered
/// literal secrets replaced. (Long-token redaction is not re-applied here so
/// structured fields such as absolute file paths in `target` stay intact; the
/// free-text fields were already token-redacted when the finding was built.)
pub fn for_output(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        if line.contains(BEGIN) {
            continue;
        }
        out.push_str(&scrub_literals(line));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_run_redacted_in_place() {
        let t = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnop0123456789";
        assert_eq!(
            redact_tokens(&format!("key: {t} end")),
            "key: <redacted> end"
        );
        assert_eq!(redact_tokens("short-token here"), "short-token here");
    }

    #[test]
    fn begin_line_dropped() {
        assert_eq!(clean_line("-----BEGIN RSA PRIVATE KEY-----"), None);
        assert_eq!(clean_text("a\n-----BEGIN X\nb", "\n"), "a\nb");
        assert_eq!(for_output("a\n-----BEGIN X\nb\n"), "a\nb\n");
    }

    #[test]
    fn literal_secret_scrubbed() {
        register_secret("hunter2-unit-test-secret");
        assert_eq!(
            for_output("x hunter2-unit-test-secret y\n"),
            "x <redacted> y\n"
        );
    }

    #[test]
    fn cap_respects_char_boundary() {
        let s = "あ".repeat(300);
        let c = cap_bytes(&s, 512);
        assert!(c.len() <= 512);
        assert!(c.chars().all(|ch| ch == 'あ'));
    }
}
