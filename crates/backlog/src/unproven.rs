//! The considered-unproven record: findings that were NOT filed because no
//! test reproduced them (user rulings 2026-10-04:
//! 「testがないなら起票してはならない」 and
//! 「…テストがかけない、証拠がない場合は起票はしてはいけない。ただし記録はのこし
//! 同一の内容を検討しないようにする」).
//!
//! `backlog add` writes a task only when its `--repro-test` REPRODUCES the
//! finding at HEAD. Every other attempt (no repro test, not reproduced,
//! undetermined) is appended here instead — `.backlog/unproven.toml`, the
//! sibling of the store's `tasks.toml` — so the finding is kept as a record
//! (never a queue row) and a later filing of the same content is told it was
//! already considered.
//!
//! Reading is three-valued: a missing file is a determined "no records"; an
//! unreadable or unparseable file is an `Err`, which every caller renders as
//! UNDETERMINED — never as "no match" (CLAUDE.md §3).

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Outcome label when `add` was given no `--repro-test` at all.
pub const OUTCOME_NO_REPRO_TEST: &str = "no-repro-test";

/// File name of the record, next to the store's `tasks.toml`.
pub const FILE_NAME: &str = "unproven.toml";

/// Title similarity at or above which a prior record is reported as a match.
/// Same scorer and threshold as the queue's near-duplicate scan.
pub const MATCH_THRESHOLD: f64 = crate::dedup::NEAR_DUPLICATE_THRESHOLD;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Record {
    pub id: String,
    pub title: String,
    pub project: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repro_test: Option<String>,
    pub outcome: String,
    #[serde(default)]
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rev: Option<String>,
    pub recorded_at: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    record: Vec<Record>,
}

/// `.backlog/unproven.toml` for the store whose live file is `tasks_path`.
pub fn path_for(tasks_path: &Path) -> PathBuf {
    tasks_path.with_file_name(FILE_NAME)
}

/// Load every record. A missing file is `Ok(vec![])` (nothing was ever
/// recorded); any other read error and any parse error is `Err` — the caller
/// must say UNDETERMINED, not "no records".
pub fn load(path: &Path) -> Result<Vec<Record>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(anyhow!(e)).with_context(|| format!("cannot read {}", path.display()))
        }
    };
    let f: File =
        toml::from_str(&text).with_context(|| format!("cannot parse {}", path.display()))?;
    Ok(f.record)
}

/// Append `rec` to the record at `path`. The whole read-modify-write runs
/// under the store's exclusive lock (`tasks_path`), and an unparseable existing
/// file is refused rather than overwritten (its bytes are left untouched).
/// Success is reported only after a fresh read shows the record.
pub fn append(tasks_path: &Path, rec: Record) -> Result<()> {
    let path = path_for(tasks_path);
    crate::store::with_store_lock_required(tasks_path, "add (unproven record)", || {
        let mut records = load(&path).context(
            "the existing unproven record is unreadable, so it was NOT rewritten (its bytes are \
             unchanged) and this finding was NOT recorded",
        )?;
        let id = rec.id.clone();
        records.push(rec);
        let text = toml::to_string(&File { record: records })
            .context("cannot serialize the unproven record")?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("cannot create {}", dir.display()))?;
        }
        crate::store::write_atomic(&path, &text)?;
        let back = load(&path).with_context(|| {
            format!(
                "wrote unproven record {id} to {} but could not re-read it to confirm",
                path.display()
            )
        })?;
        if !back.iter().any(|r| r.id == id) {
            return Err(anyhow!(
                "wrote unproven record {id} to {} but a fresh read does not contain it",
                path.display()
            ));
        }
        Ok(())
    })
}

/// New record id: `u-` + the same 8-hex FNV id the queue uses.
pub fn new_id(title: &str, now: i64) -> String {
    format!("u-{}", crate::task::new_id(title, now))
}

/// A prior record that matches a new finding.
#[derive(Debug, Clone, Serialize)]
pub struct Match {
    pub id: String,
    pub title: String,
    pub outcome: String,
    pub similarity: f64,
}

/// Prior records of the same project whose title is the same content
/// (normalized hashkey) or scores `>= MATCH_THRESHOLD`, strongest first.
pub fn matches_for(title: &str, project: &str, records: &[Record]) -> Vec<Match> {
    let key = crate::task::hashkey(title, project);
    let mut out: Vec<Match> = records
        .iter()
        .filter(|r| r.project == project)
        .filter_map(|r| {
            let similarity = if crate::task::hashkey(&r.title, &r.project) == key {
                1.0
            } else {
                harness_core::lessons::text_similarity(title, &r.title)
            };
            (similarity >= MATCH_THRESHOLD).then(|| Match {
                id: r.id.clone(),
                title: r.title.clone(),
                outcome: r.outcome.clone(),
                similarity,
            })
        })
        .collect();
    sort_matches(&mut out);
    out
}

fn sort_matches(v: &mut [Match]) {
    v.sort_by(|a, b| {
        b.similarity
            .partial_cmp(&a.similarity)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.id.cmp(&b.id))
    });
}

/// `unproven search TEXT`: a case-insensitive substring of title or notes
/// (works on Japanese text, where the lexical scorer is inert), or a title
/// similarity `>= MATCH_THRESHOLD`.
pub fn search<'a>(text: &str, records: &'a [Record]) -> Vec<&'a Record> {
    let needle = text.to_lowercase();
    records
        .iter()
        .filter(|r| {
            (!needle.is_empty()
                && (r.title.to_lowercase().contains(&needle)
                    || r.notes.to_lowercase().contains(&needle)))
                || harness_core::lessons::text_similarity(text, &r.title) >= MATCH_THRESHOLD
        })
        .collect()
}

/// The stderr render of the re-examination scan on `add`. Never empty: no
/// match is said explicitly, and an unreadable record is UNDETERMINED.
pub fn scan_report(scan: &Result<Vec<Match>>, path: &Path) -> String {
    match scan {
        Err(e) => format!(
            "unproven scan UNDETERMINED: {} could not be read ({e:#}) — this is NOT \"no prior \
             record\"; check it by hand before re-investigating this finding",
            path.display()
        ),
        Ok(m) if m.is_empty() => format!(
            "unproven scan: no prior unproven record in {} matches this finding (the scorer is \
             lexical; on Japanese titles also try `backlog unproven search TEXT`)",
            path.display()
        ),
        Ok(m) => {
            let mut s = format!(
                "unproven scan: {} prior unproven record(s) match — this content was already \
                 considered and NOT filed:",
                m.len()
            );
            for x in m {
                s.push_str(&format!(
                    "\nunproven-match: {}  similarity {:.3}  [{}]  {}",
                    x.id, x.similarity, x.outcome, x.title
                ));
            }
            s
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(id: &str, title: &str, project: &str) -> Record {
        Record {
            id: id.into(),
            title: title.into(),
            project: project.into(),
            notes: String::new(),
            tags: vec![],
            repro_test: None,
            outcome: OUTCOME_NO_REPRO_TEST.into(),
            detail: String::new(),
            rev: None,
            recorded_at: 1,
        }
    }

    #[test]
    fn matches_require_same_project() {
        let rs = vec![rec("u-1", "lock leaks on panic", "/a")];
        assert_eq!(matches_for("lock leaks on panic", "/a", &rs).len(), 1);
        assert!(matches_for("lock leaks on panic", "/b", &rs).is_empty());
    }

    #[test]
    fn missing_file_is_empty_but_garbage_is_err() {
        let d = tempfile::tempdir().unwrap();
        assert!(load(&d.path().join("nope.toml")).unwrap().is_empty());
        let p = d.path().join("bad.toml");
        std::fs::write(&p, "[[[ not toml").unwrap();
        assert!(load(&p).is_err());
    }

    #[test]
    fn undetermined_report_never_says_no_prior() {
        let r = scan_report(&Err(anyhow!("boom")), Path::new("/x"));
        assert!(r.contains("UNDETERMINED"));
        assert!(!r.contains("no prior unproven record"));
    }
}
