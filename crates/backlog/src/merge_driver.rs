//! `backlog merge-driver <base> <ours> <theirs>` — a git merge driver for the
//! task store files (`.backlog/tasks.toml`, `.backlog/tasks.done.toml`; both
//! are `[[task]]` arrays keyed by `id`). backlog 867dde4b.
//!
//! Contract (human-ruled), git's `%O %A %B` convention:
//!
//! - The merge is at task-id granularity: the union of ids; an id deleted on
//!   one side relative to base (and unchanged on the other) stays deleted; a
//!   row changed on only one side takes that side.
//! - The same id changed differently on both sides (edit/edit, edit/delete,
//!   add/add with different content) is a CONFLICT. No winner is picked: the
//!   driver exits non-zero and leaves `<ours>` untouched, so git marks the
//!   path unmerged for a human.
//! - Fail closed: any input that cannot be read or parsed, a row without a
//!   string `id`, a duplicate id inside one input, an unknown top-level key, a
//!   rendered result that does not re-parse to exactly the merged rows — all
//!   exit non-zero with `<ours>` untouched. Only a verified result is written,
//!   and it is written atomically (`store::write_atomic`).
//!
//! Rows are merged as generic TOML tables, not as [`Task`], so a field this
//! binary does not know (written by a newer binary) is carried through rather
//! than silently dropped. When every merged row round-trips through [`Task`]
//! losslessly, the output is rendered by `store::serialize_tasks` — the same
//! writer `backlog` itself uses — so the merged file looks like one `save`
//! wrote; otherwise the generic TOML rendering is used (keys in sorted order).
//!
//! A repo WITHOUT `merge.backlog.driver` configured ignores the
//! `.gitattributes` `merge=backlog` entry and falls back to git's line-based
//! text merge (see the README). `backlog merge-driver --install` configures it.

use std::path::Path;

use anyhow::{bail, Context, Result};
use toml::{Table, Value};

use crate::store;
use crate::task::Task;

/// One parsed store file: rows in file order, each with its `id`.
type Rows = Vec<(String, Table)>;

/// Parse the text of one store file into id-keyed rows. Every deviation from
/// the `[[task]]`-with-unique-string-id shape is an error, never an empty set.
fn parse_rows(text: &str, label: &str) -> Result<Rows> {
    let doc: Table =
        toml::from_str(text).with_context(|| format!("{label}: not parseable as TOML"))?;
    for key in doc.keys() {
        if key != "task" {
            bail!("{label}: unexpected top-level key `{key}` (only `task` can be merged)");
        }
    }
    let arr = match doc.get("task") {
        None => return Ok(Vec::new()),
        Some(Value::Array(a)) => a,
        Some(_) => bail!("{label}: `task` is not an array of tables"),
    };
    let mut rows: Rows = Vec::with_capacity(arr.len());
    for (i, v) in arr.iter().enumerate() {
        let Value::Table(t) = v else {
            bail!("{label}: task #{i} is not a table");
        };
        let id = match t.get("id") {
            Some(Value::String(s)) if !s.is_empty() => s.clone(),
            _ => bail!("{label}: task #{i} has no non-empty string `id`"),
        };
        if rows.iter().any(|(existing, _)| *existing == id) {
            bail!("{label}: duplicate id `{id}` (an id-level merge would be ambiguous)");
        }
        rows.push((id, t.clone()));
    }
    Ok(rows)
}

fn read_rows(path: &Path, label: &str) -> Result<Rows> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("{label}: failed to read {}", path.display()))?;
    parse_rows(&text, &format!("{label} ({})", path.display()))
}

fn find<'a>(rows: &'a Rows, id: &str) -> Option<&'a Table> {
    rows.iter().find(|(i, _)| i == id).map(|(_, t)| t)
}

/// The id-level three-way merge. `Ok(rows)` is the merged store; `Err(ids)`
/// lists every id changed incompatibly on both sides.
fn merge_rows(base: &Rows, ours: &Rows, theirs: &Rows) -> std::result::Result<Rows, Vec<String>> {
    // Output order: ours' order, then ids only theirs has, in theirs' order.
    let mut order: Vec<&str> = ours.iter().map(|(i, _)| i.as_str()).collect();
    for (i, _) in theirs {
        if !order.contains(&i.as_str()) {
            order.push(i);
        }
    }
    let mut merged: Rows = Vec::new();
    let mut conflicts: Vec<String> = Vec::new();
    for id in order {
        let b = find(base, id);
        let o = find(ours, id);
        let t = find(theirs, id);
        let pick: Option<&Table> = match (b, o, t) {
            (_, Some(o), Some(t)) if o == t => Some(o),
            (Some(b), Some(o), Some(t)) if o == b => Some(t),
            (Some(b), Some(o), Some(t)) if t == b => Some(o),
            // Deleted on one side: honoured only when the other side left it
            // exactly as base had it.
            (Some(b), Some(o), None) if o == b => None,
            (Some(b), None, Some(t)) if t == b => None,
            // Added on one side only.
            (None, Some(o), None) => Some(o),
            (None, None, Some(t)) => Some(t),
            // edit/edit, edit/delete, add/add with different content.
            _ => {
                conflicts.push(id.to_string());
                continue;
            }
        };
        if let Some(row) = pick {
            merged.push((id.to_string(), row.clone()));
        }
    }
    if conflicts.is_empty() {
        Ok(merged)
    } else {
        Err(conflicts)
    }
}

/// `Some(task)` when `row` deserializes as a [`Task`] AND serializing that
/// task back yields exactly `row` (no field dropped, none defaulted in).
fn lossless_task(row: &Table) -> Option<Task> {
    let task: Task = Value::Table(row.clone()).try_into().ok()?;
    let back = Value::try_from(&task).ok()?;
    (back == Value::Table(row.clone())).then_some(task)
}

fn render(rows: &Rows) -> Result<String> {
    let typed: Option<Vec<Task>> = rows.iter().map(|(_, r)| lossless_task(r)).collect();
    match typed {
        Some(tasks) => store::serialize_tasks(&tasks),
        None => {
            let mut doc = Table::new();
            doc.insert(
                "task".to_string(),
                Value::Array(rows.iter().map(|(_, r)| Value::Table(r.clone())).collect()),
            );
            toml::to_string_pretty(&doc).context("failed to serialize merged tasks to TOML")
        }
    }
}

/// Merge `base`/`ours`/`theirs` and, only on a clean and verified result,
/// atomically replace `ours` with it. Any `Err` (conflict included) leaves
/// `ours` byte-for-byte untouched; the caller exits non-zero.
pub fn run(base: &Path, ours: &Path, theirs: &Path) -> Result<()> {
    let b = read_rows(base, "base")?;
    let o = read_rows(ours, "ours")?;
    let t = read_rows(theirs, "theirs")?;
    let merged = match merge_rows(&b, &o, &t) {
        Ok(m) => m,
        Err(ids) => bail!(
            "conflict: task id(s) changed differently on both sides: {} \
             (no side picked; resolve by hand)",
            ids.join(", ")
        ),
    };
    let text = render(&merged)?;
    // Verify the exact bytes about to be written: they must re-parse to the
    // merged rows (same ids, same order, same content).
    let reparsed = parse_rows(&text, "merged result")?;
    if reparsed != merged {
        let got: Vec<&str> = reparsed.iter().map(|(i, _)| i.as_str()).collect();
        let want: Vec<&str> = merged.iter().map(|(i, _)| i.as_str()).collect();
        bail!(
            "merged result does not re-parse to the merged rows (ids {got:?} vs expected {want:?})"
        );
    }
    store::write_atomic(ours, &text)
        .with_context(|| format!("failed to write merged result to {}", ours.display()))
}

/// Single-quote `s` for the POSIX shell git runs the driver command through.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// `backlog merge-driver --install`: configure `merge.backlog.{name,driver}`
/// in the repository of the current directory (`git config`, local scope).
/// Pairs with the `.gitattributes` `merge=backlog` entries.
pub fn install() -> Result<()> {
    let exe = std::env::current_exe().context("cannot resolve the backlog executable path")?;
    let exe = exe
        .to_str()
        .context("backlog executable path is not valid UTF-8")?;
    let driver = format!("{} merge-driver %O %A %B", shell_quote(exe));
    for (key, value) in [
        (
            "merge.backlog.name",
            "backlog task store (id-level 3-way merge)",
        ),
        ("merge.backlog.driver", driver.as_str()),
    ] {
        let out = std::process::Command::new("git")
            .args(["config", key, value])
            .output()
            .context("failed to run git config")?;
        if !out.status.success() {
            bail!(
                "git config {key} failed ({}): {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
    }
    println!("configured merge.backlog.driver = {driver}");
    Ok(())
}
