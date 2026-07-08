//! Deterministic JSONL loader for SWE-bench [`Instance`] rows.
//!
//! `load_instances` reads a JSONL file (one JSON `Instance` per line) into a
//! `Vec<Instance>` in file order — pure and deterministic, no network, no
//! environment reads. Blank lines are skipped; a malformed line fails with a
//! clear, 1-based line-numbered error so a bad row is easy to locate.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

use crate::model::Instance;

/// Read a JSONL file into `Vec<Instance>`, preserving file order.
///
/// Blank / whitespace-only lines are ignored. A line that does not parse as an
/// [`Instance`] returns an error naming the file and the 1-based line number.
pub fn load_instances(path: impl AsRef<Path>) -> Result<Vec<Instance>> {
    let path = path.as_ref();
    let text = fs::read_to_string(path)
        .with_context(|| format!("reading instances JSONL: {}", path.display()))?;
    parse_instances(&text, &path.display().to_string())
}

/// Parse JSONL text into instances (the pure core, decoupled from the fs).
fn parse_instances(text: &str, source: &str) -> Result<Vec<Instance>> {
    let mut out = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let instance: Instance = serde_json::from_str(trimmed)
            .with_context(|| format!("malformed instance at {}:{}", source, idx + 1))?;
        out.push(instance);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_blank_lines_and_preserves_order() {
        let jsonl = r#"
{"instance_id":"a__a-1","repo":"a/a","base_commit":"c1","patch":"p1","test_patch":"t1","problem_statement":"s1","FAIL_TO_PASS":["x"],"PASS_TO_PASS":[]}

{"instance_id":"b__b-2","repo":"b/b","base_commit":"c2","patch":"p2","test_patch":"t2","problem_statement":"s2","FAIL_TO_PASS":[],"PASS_TO_PASS":["y"]}
"#;
        let got = parse_instances(jsonl, "inline").unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].instance_id, "a__a-1");
        assert_eq!(got[1].instance_id, "b__b-2");
        assert_eq!(got[0].fail_to_pass, vec!["x".to_string()]);
        assert_eq!(got[1].pass_to_pass, vec!["y".to_string()]);
    }

    #[test]
    fn malformed_line_reports_line_number() {
        let jsonl = "{\"instance_id\":\"ok\",\"repo\":\"a/a\",\"base_commit\":\"c\",\"patch\":\"p\",\"test_patch\":\"t\",\"problem_statement\":\"s\"}\nnot json\n";
        let err = parse_instances(jsonl, "inline").unwrap_err();
        assert!(err.to_string().contains("inline:2"), "got: {err}");
    }
}
