//! Append-only usage ledger: what was spent, never what was asked about.
//!
//! jev is metered, which makes it the one harness in this repo that can cost
//! money without anyone noticing. Every attempt is appended as one JSON object
//! per line so the spend is observable locally.
//!
//! # What is deliberately absent from a record
//!
//! The `state` — the text or object being evaluated — is **never** written.
//! Callers will hand jev diffs, tickets, prompts, transcript fragments and
//! command lines; a ledger that stored those would turn an accounting file
//! into a copy of everything the harness has ever looked at. Question *keys*
//! are recorded (they are the caller's own short labels), instructions and
//! state are not.
//!
//! Appends go through [`harness_core::append::append_line`], which writes the
//! body and its newline in **one** `write`. A `writeln!` here would let two
//! concurrent sessions interleave and produce a spliced, unparseable record.

use crate::config::Config;
use crate::wire::{RawResponse, Usage};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Input price in USD per million tokens, measured 2026-10-02 from
/// docs.typesafe.ai. Output tokens were free at that measurement; the field is
/// still carried so the ledger does not need back-filling if that changes.
pub const INPUT_USD_PER_MTOK: f64 = 0.042;

/// One attempt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    /// RFC3339 UTC.
    pub at: String,
    /// The model the *response* reported, or the requested route when there
    /// was no response. Prefixed `requested:` in the latter case so the two
    /// are never confused.
    pub model: String,
    /// Question keys only — see the module docs.
    pub questions: Vec<String>,
    /// HTTP status, when one was obtained.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub http_status: Option<u16>,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    pub duration_ms: u64,
    /// `answered` / `http-error` / `undetermined`.
    pub outcome: String,
    /// Redacted failure detail, when the attempt did not answer.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub detail: Option<String>,
}

impl Record {
    pub fn estimated_usd(&self) -> f64 {
        (self.input_tokens as f64) * INPUT_USD_PER_MTOK / 1_000_000.0
    }

    pub fn answered(
        model_from_response: &RawResponse,
        questions: Vec<String>,
        http_status: u16,
        duration_ms: u64,
    ) -> Self {
        let Usage {
            input_tokens,
            output_tokens,
        } = model_from_response.usage;
        Record {
            at: now_rfc3339(),
            model: model_from_response.model.clone(),
            questions,
            http_status: Some(http_status),
            input_tokens,
            output_tokens,
            duration_ms,
            outcome: "answered".to_string(),
            detail: None,
        }
    }

    pub fn failed(
        requested_model: &str,
        questions: Vec<String>,
        http_status: Option<u16>,
        duration_ms: u64,
        outcome: &str,
        detail: String,
    ) -> Self {
        Record {
            at: now_rfc3339(),
            // Marked so an aggregate never reports a route as the thing that
            // answered when nothing answered.
            model: format!("requested:{requested_model}"),
            questions,
            http_status,
            input_tokens: 0,
            output_tokens: 0,
            duration_ms,
            outcome: outcome.to_string(),
            detail: Some(detail),
        }
    }
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Append one record. Failure to write is reported to the caller; it is not a
/// reason to fail a jev call (the call already happened), but it must not be
/// swallowed either, or the spend silently stops being observable.
pub fn append(cfg: &Config, rec: &Record) -> Result<(), String> {
    let Some(path) = cfg.resolved_ledger_path() else {
        return Err("no ledger path (HOME is unset and JEV_LEDGER is not set)".to_string());
    };
    append_to(&path, rec)
}

/// Append to an explicit path.
pub fn append_to(path: &Path, rec: &Record) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("creating {}: {e}", parent.display()))?;
    }
    let line = serde_json::to_string(rec).map_err(|e| format!("encoding ledger record: {e}"))?;
    harness_core::append::append_line(path, &line)
        .map_err(|e| format!("appending to {}: {e}", path.display()))
}

/// A roll-up of a ledger file.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Summary {
    pub records: u64,
    pub answered: u64,
    pub unanswered: u64,
    /// Lines that did not parse. Reported rather than skipped: a ledger that
    /// quietly drops malformed lines under-reports spend.
    pub unparseable: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub estimated_usd: f64,
}

/// Summarize a ledger file. A missing file is an empty summary (nothing has
/// been spent yet); an unreadable file is an error.
pub fn summarize(path: &Path) -> Result<Summary, String> {
    if !path.exists() {
        return Ok(Summary::default());
    }
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let mut s = Summary::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        s.records += 1;
        match serde_json::from_str::<Record>(line) {
            Ok(r) => {
                if r.outcome == "answered" {
                    s.answered += 1;
                } else {
                    s.unanswered += 1;
                }
                s.input_tokens += r.input_tokens;
                s.output_tokens += r.output_tokens;
                s.estimated_usd += r.estimated_usd();
            }
            Err(_) => s.unparseable += 1,
        }
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::Answer;
    use std::collections::BTreeMap;

    fn resp(model: &str, input: u64) -> RawResponse {
        let mut answers = BTreeMap::new();
        answers.insert("q".to_string(), Answer::Noul { noul: 0.5 });
        RawResponse {
            model: model.to_string(),
            answers,
            usage: Usage {
                input_tokens: input,
                output_tokens: 0,
            },
        }
    }

    #[test]
    fn a_record_never_contains_the_state() {
        let r = Record::answered(&resp("jev-1.13.0", 100), vec!["q".into()], 200, 12);
        let json = serde_json::to_string(&r).unwrap();
        assert!(!json.contains("hunter2"));
        assert!(json.contains("\"questions\":[\"q\"]"));
        // No field named state exists at all.
        assert!(!json.contains("state"));
    }

    #[test]
    fn failed_records_mark_the_model_as_merely_requested() {
        let r = Record::failed(
            "jev-latest",
            vec!["q".into()],
            Some(529),
            8000,
            "http-error",
            "overloaded".into(),
        );
        assert_eq!(r.model, "requested:jev-latest");
        assert_eq!(r.outcome, "http-error");
        assert_eq!(r.input_tokens, 0);
    }

    #[test]
    fn roundtrip_and_summary() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub").join("usage.jsonl");
        append_to(
            &p,
            &Record::answered(&resp("m", 1_000_000), vec!["a".into()], 200, 5),
        )
        .unwrap();
        append_to(
            &p,
            &Record::failed(
                "m",
                vec!["b".into()],
                None,
                8000,
                "undetermined",
                "timeout".into(),
            ),
        )
        .unwrap();
        let s = summarize(&p).unwrap();
        assert_eq!(s.records, 2);
        assert_eq!(s.answered, 1);
        assert_eq!(s.unanswered, 1);
        assert_eq!(s.unparseable, 0);
        assert_eq!(s.input_tokens, 1_000_000);
        assert!(
            (s.estimated_usd - 0.042).abs() < 1e-9,
            "usd={}",
            s.estimated_usd
        );
    }

    #[test]
    fn malformed_lines_are_counted_not_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("usage.jsonl");
        std::fs::write(&p, "{not json}\n").unwrap();
        let s = summarize(&p).unwrap();
        assert_eq!(s.records, 1);
        assert_eq!(s.unparseable, 1);
    }

    #[test]
    fn a_missing_ledger_is_empty_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let s = summarize(&dir.path().join("nope.jsonl")).unwrap();
        assert_eq!(s, Summary::default());
    }
}
