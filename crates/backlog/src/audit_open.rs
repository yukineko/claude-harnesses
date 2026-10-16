//! `backlog audit-open`: does each OPEN queue row carry a recorded
//! reproducing test? (user ruling 2026-10-04:
//! 「完了のテストはみなくていい。pendingにテストがあるかだけ確認すればいいし2度確認しなくていい」).
//!
//! - Scope: stored status `pending` / `failed` (a `claimed`
//!   row is a leased pending row). Terminal rows are records only and are
//!   never audited here; their evidence was audited at close time by
//!   `done` / `cancel`.
//! - The result is read from the row's recorded `repro`; NO test is run.
//! - Each row is checked ONCE: a determined result (`has-test` / `no-test`) is
//!   written to an untracked ledger
//!   `~/.backlog/audit-open/<project-slug>.json` (the claim ledger's slug) and
//!   the row is skipped on later runs. `undetermined` is reported but never
//!   recorded as checked.
//! - An unreadable ledger is UNDETERMINED: the command fails, writes nothing,
//!   and nothing counts as checked.

use crate::store;
use crate::task::{Task, REPRO_NOT_REPRODUCED, REPRO_REPRODUCED, STATUS_FAILED, STATUS_PENDING};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const HAS_TEST: &str = "has-test";
pub const NO_TEST: &str = "no-test";
pub const UNDETERMINED: &str = "undetermined";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Check {
    pub result: String,
    pub checked_at: i64,
}

/// NO `#[serde(default)]` on `checked`: a file without it is not a ledger.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Ledger {
    pub checked: BTreeMap<String, Check>,
}

pub fn ledger_path(identity: &str) -> PathBuf {
    harness_core::config::base_dir("backlog")
        .join("audit-open")
        .join(format!("{}.json", crate::lock::project_slug(identity)))
}

fn load_ledger(path: &Path) -> Result<Ledger> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Ledger::default()),
        Err(e) => {
            return Err(anyhow!(e)).with_context(|| format!("cannot read {}", path.display()))
        }
    };
    serde_json::from_str(&text).with_context(|| format!("cannot parse {}", path.display()))
}

fn is_open(t: &Task) -> bool {
    matches!(t.status, STATUS_PENDING | STATUS_FAILED)
}

/// Classify from the RECORDED repro only.
pub fn classify(t: &Task) -> &'static str {
    match t.repro.as_ref().map(|r| r.outcome.as_str()) {
        None | Some(REPRO_NOT_REPRODUCED) => NO_TEST,
        Some(REPRO_REPRODUCED) => HAS_TEST,
        Some(_) => UNDETERMINED,
    }
}

pub fn run(tasks_path: &Path, identity: &str, as_json: bool, now: i64) -> Result<()> {
    let path = ledger_path(identity);
    let mut ledger = load_ledger(&path).map_err(|e| {
        anyhow!(
            "backlog audit-open: the check ledger {} is UNDETERMINED ({e:#}); nothing was \
             audited, written, or counted as checked",
            path.display()
        )
    })?;
    let tasks = store::load(tasks_path)?;
    let open: Vec<&Task> = tasks.iter().filter(|t| is_open(t)).collect();
    let mut checked_now = Vec::new();
    let mut no_test = Vec::new();
    let mut undetermined = Vec::new();
    let mut skipped = 0usize;
    for t in &open {
        if ledger.checked.contains_key(&t.id) {
            skipped += 1;
            continue;
        }
        let result = classify(t);
        checked_now.push(serde_json::json!({
            "id": t.id, "title": t.title, "status": t.status, "result": result,
        }));
        match result {
            NO_TEST => no_test.push(t.id.clone()),
            UNDETERMINED => {
                undetermined.push(t.id.clone());
                continue;
            }
            _ => {}
        }
        ledger.checked.insert(
            t.id.clone(),
            Check {
                result: result.to_string(),
                checked_at: now,
            },
        );
    }
    let still_open_no_test: Vec<String> = open
        .iter()
        .filter(|t| {
            ledger
                .checked
                .get(&t.id)
                .is_some_and(|c| c.result == NO_TEST)
        })
        .map(|t| t.id.clone())
        .collect();
    if !checked_now.is_empty() {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("cannot create {}", dir.display()))?;
        }
        store::write_atomic(&path, &serde_json::to_string(&ledger)?)?;
    }
    if as_json {
        println!(
            "{}",
            serde_json::json!({
                "checked_now": checked_now,
                "no_test": no_test,
                "undetermined": undetermined,
                "skipped_already_checked": skipped,
                "recorded_no_test_still_open": still_open_no_test,
            })
        );
        return Ok(());
    }
    if checked_now.is_empty() {
        println!("audit-open: no unchecked open rows (skipped (already checked): {skipped})");
    } else {
        for c in &checked_now {
            println!(
                "{:<10} {:<13} {}",
                c["id"].as_str().unwrap_or(""),
                c["result"].as_str().unwrap_or(""),
                c["title"].as_str().unwrap_or("")
            );
        }
        println!(
            "audit-open: checked {} open row(s) now — no-test: {}, undetermined: {} (re-examined \
             next run); skipped (already checked): {skipped}",
            checked_now.len(),
            no_test.len(),
            undetermined.len()
        );
    }
    println!(
        "recorded no-test rows still open: {}{}",
        still_open_no_test.len(),
        if still_open_no_test.is_empty() {
            String::new()
        } else {
            format!(" — {}", still_open_no_test.join(" "))
        }
    );
    Ok(())
}
