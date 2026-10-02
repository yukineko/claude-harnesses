//! Foreign-file bridge: read the cwd repo's backlog (`.backlog/tasks.toml`)
//! for rows awaiting a human ruling into the overwatch review-queue as the
//! [`crate::review_queue::EntryKind::NeedsRuling`] source.
//!
//! A `needs-ruling` row is a backlog task whose closure (a judgment close or an
//! untestable item) is waiting on a human `backlog ruling approve`. It is
//! shown here so the wait is visible; it is NEVER bridged back into backlog
//! (the source already IS backlog — see `bridge::plan_entry_adds`).
//!
//! Like [`crate::review_escalation`], the read is three-valued on
//! [`harness_core::verdict::Determination`]:
//!
//! | situation | answer |
//! |---|---|
//! | `.backlog/tasks.toml` is not there | `Known(vec![])` — a real zero |
//! | it is there and parses | `Known(needs-ruling rows)` |
//! | it is there but cannot be read | `Undetermined` |
//! | it is there but does not parse | `Undetermined` |
//!
//! Absent is NOT undetermined, for the same reason as escalations: most
//! projects never use a repo-local backlog, and answering "undetermined" for
//! all of them would fire the queue's warning everywhere. An unreadable or
//! unparseable store, however, may hold rows awaiting a ruling, so reading it
//! as empty would report a waiting human decision as none.
//!
//! On-disk shape (flat on each `[[task]]`): `status = "needs-ruling"`,
//! `ruling_kind = "judgment" | "untestable"`, `rationale`, `untestable_reason`.

use harness_core::verdict::Determination;
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Minimal mirror of one backlog `[[task]]` row; unknown fields are ignored.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
pub struct NeedsRulingTask {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub ruling_kind: String,
    #[serde(default)]
    pub rationale: String,
    #[serde(default)]
    pub untestable_reason: String,
}

#[derive(Debug, Default, Deserialize)]
struct Store {
    #[serde(default)]
    task: Vec<NeedsRulingTask>,
}

/// Path of the repo-local backlog for the repo containing `cwd`.
pub fn tasks_path(cwd: &Path) -> PathBuf {
    harness_core::projkey::repo_root(cwd)
        .join(".backlog")
        .join("tasks.toml")
}

/// Parse `tasks.toml` text and keep only `status == "needs-ruling"` rows.
/// Text that does not parse is `Undetermined`, never an empty store.
pub fn parse_needs_ruling(txt: &str, source: &str) -> Determination<Vec<NeedsRulingTask>> {
    match toml::from_str::<Store>(txt) {
        Ok(s) => Determination::known(
            s.task
                .into_iter()
                .filter(|t| t.status == "needs-ruling")
                .collect(),
        ),
        Err(e) => Determination::undetermined(format!(
            "backlog store at {source} does not parse: {e} — reading it as empty would \
             report a row awaiting a human ruling as none"
        )),
    }
}

/// Read the needs-ruling rows of the cwd repo's backlog. See the module table.
pub fn scan_needs_ruling(cwd: &Path) -> Determination<Vec<NeedsRulingTask>> {
    let path = tasks_path(cwd);
    match harness_core::boundary::read_to_string(&path) {
        Determination::Known(None) => Determination::known(Vec::new()),
        Determination::Known(Some(txt)) => parse_needs_ruling(&txt, &path.display().to_string()),
        Determination::Undetermined(why) => Determination::Undetermined(why),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STORE: &str = r#"
[[task]]
id = "a"
title = "A"
status = "pending"
[[task]]
id = "b"
title = "B"
status = "needs-ruling"
ruling_kind = "judgment"
rationale = "why"
"#;

    #[test]
    fn keeps_only_needs_ruling_rows() {
        match parse_needs_ruling(STORE, "x") {
            Determination::Known(v) => {
                assert_eq!(v.len(), 1);
                assert_eq!(v[0].id, "b");
                assert_eq!(v[0].rationale, "why");
            }
            Determination::Undetermined(_) => unreachable!("store parses"),
        }
    }

    #[test]
    fn unparseable_is_undetermined_not_empty() {
        assert!(matches!(
            parse_needs_ruling("[[[ = =", "x"),
            Determination::Undetermined(_)
        ));
    }

    #[test]
    fn absent_file_is_known_empty() {
        let d = std::env::temp_dir().join(format!("ow-ruling-absent-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        assert!(matches!(
            scan_needs_ruling(&d),
            Determination::Known(v) if v.is_empty()
        ));
    }
}
