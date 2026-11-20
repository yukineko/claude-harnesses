//! The merge proof contract (backlog 7768aeaf, spec
//! `specs/condukt-merge-proof-contract-7768aeaf.toml` R1-R9; described for
//! users in `crates/condukt/README.md`, "Merge proof contract").
//!
//! A merge of a task whose decomposition `kind` is `fix` / `feature`
//! (`Task::requires_fp_oracle`) is refused unless the task's real `tdd`
//! RED/GREEN proofs satisfy seven items, each named on stderr by its token:
//!
//! * `merge-proof-contract(i)` — no proof file of the task at all;
//! * `merge-proof-contract(missing-green)` — a RED slot whose `cmd` no GREEN
//!   slot records (a GREEN slot without a RED one is not a refusal);
//! * `merge-proof-contract(f)` — on some GREEN slot, a name in
//!   `red_failed_tests` is not `"pass"` in `tests`;
//! * `merge-proof-contract(g)` — on some GREEN slot, `counts.determined` is
//!   true and `counts.passed` is 0;
//! * `merge-proof-contract(h)` — on some GREEN slot, `tree_identity.tree` is
//!   not the branch tip's commit tree with `.tdd` and tdd's effective proof
//!   dir removed (the same exclusion tdd applies when it computes that tree,
//!   so committing only proof files after GREEN does not trip it);
//! * `merge-proof-contract(undetermined-proof)` — any proof input that cannot
//!   be judged (a missing / null field the items read, `counts.determined`
//!   false, an unreadable or unparsable file, a failed listing, a tip tree
//!   that cannot be computed, worktree and archive copies that disagree,
//!   proofs in more than one proof dir of the worktree);
//! * `merge-proof-contract(undetermined-task)` — the task was named or
//!   resolved but its run state cannot be read, it is not in it, or its
//!   decomposition entry (or `kind`) cannot be read.
//!
//! The slot set is the one tdd's `slot_files` enumerates: the legacy
//! `<task>.<kind>.json` and every `<task>.<16 hex>.<kind>.json`. Proofs are
//! read from the branch's worktree and, when it holds none, from the proof
//! archive (`proof_archive::archive_dir`); the `.tdd/` committed at the
//! branch tip is never a proof source. A merge whose task is neither named
//! (`--run/--task`) nor recorded (`branch.<b>.conduktTask`) is outside the
//! contract.

use anyhow::{bail, Result};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::{
    abort_state_phrase, abort_trial_merge, git, git_env, parse_unmerged_paths,
    recorded_task_identity, run_git_bounded, ResolveOutcome,
};
use crate::config::Config;
use harness_core::verdict::Determination;

/// One contract item (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Item {
    I,
    MissingGreen,
    F,
    G,
    H,
    UndeterminedProof,
    UndeterminedTask,
}

impl Item {
    /// The stderr token naming this item.
    pub(super) fn token(self) -> &'static str {
        match self {
            Item::I => "merge-proof-contract(i)",
            Item::MissingGreen => "merge-proof-contract(missing-green)",
            Item::F => "merge-proof-contract(f)",
            Item::G => "merge-proof-contract(g)",
            Item::H => "merge-proof-contract(h)",
            Item::UndeterminedProof => "merge-proof-contract(undetermined-proof)",
            Item::UndeterminedTask => "merge-proof-contract(undetermined-task)",
        }
    }
}

/// One violated item with what was observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Finding {
    pub(super) item: Item,
    pub(super) detail: String,
}

fn finding(item: Item, detail: impl Into<String>) -> Finding {
    Finding {
        item,
        detail: detail.into(),
    }
}

/// The directory of the worktree that has `branch` checked out, found via
/// `git worktree list --porcelain`. `None` when no worktree has it. (Moved
/// here from `worktree.rs`, file-growth ratchet; also used by the store-diff
/// capture.)
pub(super) fn worktree_for_branch(repo: &Path, branch: &str) -> Result<Option<PathBuf>> {
    let want = if branch.starts_with("refs/") {
        branch.to_string()
    } else {
        format!("refs/heads/{branch}")
    };
    let listing = git(repo, &["worktree", "list", "--porcelain"])?;
    let mut cur: Option<PathBuf> = None;
    for line in listing.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            cur = Some(PathBuf::from(p));
        } else if let Some(b) = line.strip_prefix("branch ") {
            if b == want {
                return Ok(cur);
            }
        }
    }
    Ok(None)
}

/// Refuse (`Err`, nothing touched) when the contract is violated for the
/// merge of `branch` at the pinned `tip`. `cli_task` = the `--run/--task`
/// pair when given; otherwise the task comes from `branch.<b>.conduktTask`
/// (none recorded = task-less, not checked).
pub(super) fn enforce(
    cfg: &Config,
    repo: &Path,
    branch: &str,
    default_branch: &str,
    tip: &str,
    cli_task: Option<(&str, &str)>,
) -> Result<()> {
    match refusal(cfg, repo, branch, default_branch, tip, cli_task) {
        Some(msg) => bail!("{msg}"),
        None => Ok(()),
    }
}

/// The refusal message when the contract is violated, `None` when it holds
/// or does not apply.
pub(super) fn refusal(
    cfg: &Config,
    repo: &Path,
    branch: &str,
    default_branch: &str,
    tip: &str,
    cli_task: Option<(&str, &str)>,
) -> Option<String> {
    let (who, findings) = check(cfg, repo, branch, tip, cli_task);
    if findings.is_empty() {
        return None;
    }
    let lines: Vec<String> = findings
        .iter()
        .map(|f| format!("{}: {}", f.item.token(), f.detail))
        .collect();
    Some(format!(
        "the merge of '{branch}' ({who}) violates the merge proof contract (backlog 7768aeaf; \
         see crates/condukt/README.md \"Merge proof contract\"):\n  {}\nrefusing to merge \
         ('{default_branch}' untouched). Record real proofs in the task worktree with \
         `tdd red` / `tdd green` on the committed tree, then merge again.",
        lines.join("\n  ")
    ))
}

/// The task identity the contract is checked against and every finding.
fn check(
    cfg: &Config,
    repo: &Path,
    branch: &str,
    tip: &str,
    cli_task: Option<(&str, &str)>,
) -> (String, Vec<Finding>) {
    let short = branch.strip_prefix("refs/heads/").unwrap_or(branch);
    let (run, task) = match cli_task {
        Some((r, t)) => (r.to_string(), t.to_string()),
        None => match recorded_task_identity(repo, short) {
            Ok(None) => return ("no task".to_string(), Vec::new()),
            Ok(Some(id)) => match id.split_once('/') {
                Some((r, t)) if !r.is_empty() && !t.is_empty() => (r.to_string(), t.to_string()),
                _ => {
                    return (
                        format!("recorded task '{id}'"),
                        vec![finding(
                            Item::UndeterminedTask,
                            format!(
                                "the recorded task identity '{id}' of '{short}' is not \
                                 `<run>/<task>`"
                            ),
                        )],
                    )
                }
            },
            Err(e) => {
                return (
                    "task record unreadable".to_string(),
                    vec![finding(
                        Item::UndeterminedTask,
                        format!("the task record of '{short}' could not be read: {e:#}"),
                    )],
                )
            }
        },
    };
    let who = format!("task '{run}/{task}'");
    match task_requires_proofs(cfg, repo, &run, &task) {
        Ok(false) => return (who, Vec::new()),
        Ok(true) => {}
        Err(why) => return (who, vec![finding(Item::UndeterminedTask, why)]),
    }
    let source = match load_proofs(cfg, repo, branch, &run, &task) {
        Ok(s) => s,
        Err(why) => return (who, vec![finding(Item::UndeterminedProof, why)]),
    };
    let st = tdd_safe(&task);
    let has_green = source
        .files
        .keys()
        .any(|n| slot_kind(n, &st) == Some("green"));
    let tip_tree = if has_green {
        source
            .excludes
            .clone()
            .and_then(|ex| tip_tree_without(repo, tip, &ex))
    } else {
        Err("no GREEN slot".to_string())
    };
    let mut findings = evaluate(&st, &source.files, &tip_tree);
    for f in &mut findings {
        f.detail = format!(
            "{} (proofs read from {})",
            f.detail,
            source.origin.display()
        );
    }
    (who, findings)
}

/// Whether the task's `kind` puts it under the contract. `Err` = it cannot be
/// determined (undetermined-task).
fn task_requires_proofs(
    cfg: &Config,
    repo: &Path,
    run: &str,
    task: &str,
) -> std::result::Result<bool, String> {
    let rs = crate::state::RunState::load(cfg, repo, run)
        .map_err(|e| format!("the run state of '{run}' could not be read: {e:#}"))?;
    if !rs.tasks.iter().any(|t| t.id == task) {
        return Err(format!("task '{task}' is not in the run state of '{run}'"));
    }
    let raw = match crate::state::load_decomposition_determined(cfg, repo, run) {
        Determination::Known(Some(raw)) => raw,
        Determination::Known(None) => {
            return Err(format!(
                "run '{run}' has no decomposition, so the kind of '{task}' cannot be read"
            ))
        }
        Determination::Undetermined(why) => {
            return Err(format!(
                "the decomposition of run '{run}' could not be read: {why}"
            ))
        }
    };
    let dec: crate::model::Decomposition = serde_json::from_str(&raw)
        .map_err(|e| format!("the decomposition of run '{run}' is not parseable: {e}"))?;
    let t = dec
        .tasks
        .iter()
        .find(|t| t.id == task)
        .ok_or_else(|| format!("run '{run}' has no decomposition entry for task '{task}'"))?;
    if t.kind.as_deref().map(str::trim).unwrap_or("").is_empty() {
        return Err(format!(
            "task '{task}' of run '{run}' has no kind, so whether it needs proofs is unknown"
        ));
    }
    Ok(t.requires_fp_oracle())
}

/// The proof files the contract judges.
struct ProofSource {
    /// Slot file name -> bytes (both kinds) of the task.
    files: BTreeMap<String, Vec<u8>>,
    /// Where they were read from (for the message).
    origin: PathBuf,
    /// Repo-relative paths tdd excludes from `tree_identity.tree`.
    excludes: std::result::Result<Vec<String>, String>,
}

/// tdd's file-name sanitizer for a task id (`crates/tdd/src/proof.rs` `safe`).
fn tdd_safe(task: &str) -> String {
    task.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// `Some("red" | "green")` when `name` is a slot file of task `safe_task`
/// (legacy `<task>.<kind>.json` or `<task>.<16 lowercase hex>.<kind>.json`).
fn slot_kind(name: &str, safe_task: &str) -> Option<&'static str> {
    for kind in ["red", "green"] {
        if name == format!("{safe_task}.{kind}.json") {
            return Some(kind);
        }
        let mid = name
            .strip_prefix(&format!("{safe_task}."))
            .and_then(|n| n.strip_suffix(&format!(".{kind}.json")));
        if let Some(mid) = mid {
            if mid.len() == 16
                && mid
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Some(kind);
            }
        }
    }
    None
}

/// The task's slot files directly in `dir`. A missing dir is "none"; any
/// other failure is an `Err`.
fn read_slots(
    dir: &Path,
    safe_task: &str,
) -> std::result::Result<BTreeMap<String, Vec<u8>>, String> {
    let mut out = BTreeMap::new();
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(format!("cannot list proof dir {}: {e}", dir.display())),
    };
    for entry in rd {
        let entry = entry.map_err(|e| format!("cannot read an entry of {}: {e}", dir.display()))?;
        let name = entry.file_name().to_string_lossy().to_string();
        if slot_kind(&name, safe_task).is_none() {
            continue;
        }
        let bytes = std::fs::read(entry.path())
            .map_err(|e| format!("cannot read proof {}: {e}", entry.path().display()))?;
        out.insert(name, bytes);
    }
    Ok(out)
}

/// Worktree proofs when the branch's worktree holds any, else the archive's.
fn load_proofs(
    cfg: &Config,
    repo: &Path,
    branch: &str,
    run: &str,
    task: &str,
) -> std::result::Result<ProofSource, String> {
    let st = tdd_safe(task);
    let wt = worktree_for_branch(repo, branch)
        .map_err(|e| format!("could not locate the worktree of '{branch}': {e:#}"))?;
    let mut wt_set = None;
    let mut excludes_root = repo.to_path_buf();
    if let Some(wt) = &wt {
        excludes_root = wt.clone();
        let (dirs, _) = crate::proof_archive::proof_dir_locations(wt).map_err(|e| {
            format!(
                "the proof dir of worktree {} could not be resolved: {e:#}",
                wt.display()
            )
        })?;
        let mut holders = Vec::new();
        for d in dirs {
            let set = read_slots(&d, &st)?;
            if !set.is_empty() {
                holders.push((d, set));
            }
        }
        if holders.len() > 1 {
            let names: Vec<String> = holders
                .iter()
                .map(|(d, _)| d.display().to_string())
                .collect();
            return Err(format!(
                "proofs of '{task}' are in more than one proof dir of the worktree ({}); \
                 which one tdd used cannot be determined",
                names.join(", ")
            ));
        }
        wt_set = holders.pop();
    }
    let arch = crate::proof_archive::archive_dir(cfg, repo, run, task)
        .map_err(|e| format!("the proof archive of '{run}/{task}' could not be located: {e:#}"))?;
    let arch_dirs = crate::proof_archive::archive_proof_dirs(&arch).map_err(|e| {
        format!(
            "the proof dirs of archive {} could not be resolved: {e:#}",
            arch.display()
        )
    })?;
    let mut arch_set: Option<(PathBuf, BTreeMap<String, Vec<u8>>)> = None;
    for d in arch_dirs {
        let set = read_slots(&d, &st)?;
        if set.is_empty() {
            continue;
        }
        match &arch_set {
            Some((first, s)) if *s != set => {
                return Err(format!(
                    "the archived proofs of '{task}' in {} and {} differ",
                    first.display(),
                    d.display()
                ))
            }
            Some(_) => {}
            None => arch_set = Some((d, set)),
        }
    }
    let excludes = crate::proof_archive::proof_dir_locations(&excludes_root)
        .map(|(_, rel)| {
            let mut ex = vec![".tdd".to_string()];
            ex.extend(rel.filter(|r| r != ".tdd"));
            ex
        })
        .map_err(|e| {
            format!(
                "tdd's proof dir for {} could not be resolved: {e:#}",
                excludes_root.display()
            )
        });
    let (origin, files) = match (wt_set, arch_set) {
        (Some((wd, w)), Some((ad, a))) if w != a => {
            return Err(format!(
                "the worktree proofs in {} and the archived proofs in {} of '{task}' differ",
                wd.display(),
                ad.display()
            ))
        }
        (Some(w), _) => w,
        (None, Some(a)) => a,
        (None, None) => (wt.clone().unwrap_or_else(|| arch.clone()), BTreeMap::new()),
    };
    Ok(ProofSource {
        files,
        origin,
        excludes,
    })
}

/// The tree of commit `tip` with `excludes` removed, via a throwaway index
/// (the real index is never touched).
fn tip_tree_without(
    repo: &Path,
    tip: &str,
    excludes: &[String],
) -> std::result::Result<String, String> {
    let run = || -> Result<String> {
        let tmp = tempfile::Builder::new()
            .prefix("condukt-merge-proof-index-")
            .tempdir()?;
        let index = tmp.path().join("index");
        let env = [("GIT_INDEX_FILE", index.as_path())];
        git_env(repo, &["read-tree", tip], &env)?;
        let specs: Vec<String> = excludes.iter().map(|e| format!(":(literal){e}")).collect();
        let mut args = vec!["rm", "-r", "-f", "--cached", "-q", "--ignore-unmatch", "--"];
        args.extend(specs.iter().map(String::as_str));
        git_env(repo, &args, &env)?;
        Ok(git_env(repo, &["write-tree"], &env)?.trim().to_string())
    };
    match run() {
        Ok(t) if !t.is_empty() && t.bytes().all(|b| b.is_ascii_hexdigit()) => Ok(t),
        Ok(t) => Err(format!("`git write-tree` printed {t:?}, not a tree id")),
        Err(e) => Err(format!(
            "the tree of the branch tip {tip} without the proof dir could not be computed: {e:#}"
        )),
    }
}

/// Judge the slot files `files` of task `safe_task` (pure; module docs).
fn evaluate(
    safe_task: &str,
    files: &BTreeMap<String, Vec<u8>>,
    tip_tree: &std::result::Result<String, String>,
) -> Vec<Finding> {
    use Item::*;
    let mut out = Vec::new();
    if files.is_empty() {
        out.push(finding(
            I,
            "no tdd proof file (*.red.json / *.green.json) of the task exists",
        ));
        return out;
    }
    let parse = |name: &str, bytes: &[u8]| -> std::result::Result<Value, String> {
        match serde_json::from_slice::<Value>(bytes) {
            Ok(v) if v.is_object() => Ok(v),
            Ok(_) => Err(format!("{name} is not a JSON object")),
            Err(e) => Err(format!("{name} is not parseable JSON: {e}")),
        }
    };
    let mut red_cmds = Vec::new();
    let mut green_cmds = Vec::new();
    let mut cmds_known = true;
    for (name, bytes) in files {
        let kind = slot_kind(name, safe_task);
        let v = match parse(name, bytes) {
            Ok(v) => v,
            Err(why) => {
                out.push(finding(UndeterminedProof, why));
                cmds_known = false;
                continue;
            }
        };
        let cmd = v.get("cmd").and_then(Value::as_str).map(str::to_string);
        match (kind, cmd) {
            (Some("red"), Some(c)) => red_cmds.push((name.clone(), c)),
            (_, Some(c)) => green_cmds.push(c),
            (_, None) => {
                out.push(finding(
                    UndeterminedProof,
                    format!("{name} records no `cmd`"),
                ));
                cmds_known = false;
            }
        }
        if kind == Some("green") {
            judge_green(name, &v, tip_tree, &mut out);
        }
    }
    if cmds_known {
        for (name, c) in &red_cmds {
            if !green_cmds.contains(c) {
                out.push(finding(
                    MissingGreen,
                    format!("RED slot {name} (cmd {c:?}) has no GREEN slot with the same cmd"),
                ));
            }
        }
    }
    out
}

/// Items f, g, h (and their undetermined inputs) on one GREEN slot.
fn judge_green(
    name: &str,
    v: &Value,
    tip_tree: &std::result::Result<String, String>,
    out: &mut Vec<Finding>,
) {
    use Item::*;
    // (f)
    let red_failed = v.get("red_failed_tests").and_then(Value::as_array);
    let tests = v.get("tests").and_then(Value::as_object);
    match (red_failed, tests) {
        (Some(rf), Some(tests)) => {
            let mut lost = Vec::new();
            let mut bad = false;
            for n in rf {
                match n.as_str() {
                    Some(n) if tests.get(n).and_then(Value::as_str) == Some("pass") => {}
                    Some(n) => lost.push(n.to_string()),
                    None => bad = true,
                }
            }
            if bad {
                out.push(finding(
                    UndeterminedProof,
                    format!("{name}: red_failed_tests holds a non-string entry"),
                ));
            }
            if !lost.is_empty() {
                out.push(finding(
                    F,
                    format!(
                        "{name}: failed at RED but not \"pass\" at GREEN: {}",
                        lost.join(", ")
                    ),
                ));
            }
        }
        (None, _) => out.push(finding(
            UndeterminedProof,
            format!("{name}: red_failed_tests is missing or null (RED's failing set unknown)"),
        )),
        (_, None) => out.push(finding(
            UndeterminedProof,
            format!("{name}: tests is missing or not an object"),
        )),
    }
    // (g)
    let counts = v.get("counts").and_then(Value::as_object);
    match counts.map(|c| {
        (
            c.get("determined").and_then(Value::as_bool),
            c.get("passed"),
        )
    }) {
        Some((Some(true), Some(p))) => match p.as_u64() {
            Some(0) => out.push(finding(G, format!("{name}: GREEN passed 0 tests"))),
            Some(_) => {}
            None => out.push(finding(
                UndeterminedProof,
                format!("{name}: counts.passed is not a count"),
            )),
        },
        Some((Some(true), None)) => out.push(finding(
            UndeterminedProof,
            format!("{name}: counts.passed is missing"),
        )),
        Some(_) => out.push(finding(
            UndeterminedProof,
            format!("{name}: counts.determined is not true (the pass count is unknown)"),
        )),
        None => out.push(finding(
            UndeterminedProof,
            format!("{name}: counts is missing"),
        )),
    }
    // (h)
    let tree = v
        .get("tree_identity")
        .and_then(Value::as_object)
        .and_then(|t| t.get("tree"))
        .and_then(Value::as_str);
    match (tree, tip_tree) {
        (None, _) => out.push(finding(
            UndeterminedProof,
            format!("{name}: tree_identity.tree is missing or null"),
        )),
        (Some(_), Err(why)) => out.push(finding(UndeterminedProof, format!("{name}: {why}"))),
        (Some(t), Ok(tip)) if t != tip => out.push(finding(
            H,
            format!(
                "{name}: GREEN was taken on tree {t}, the branch tip's tree without the proof \
                 dir is {tip}"
            ),
        )),
        (Some(_), Ok(_)) => {}
    }
}

/// `resolve-merge` Manual when the contract is violated (`refusal`): try the
/// merge without committing. A clean result is aborted and refused (nothing
/// committed, no merge in progress); a conflicting one leaves its markers as
/// [`ResolveOutcome::ManualPending`] (marker-only Manual is outside the
/// contract). Any other outcome (a failed merge with no conflicts, a timeout,
/// an unlistable index) is aborted and refused.
pub(super) fn manual_with_violation(
    repo: &Path,
    msg: &str,
    tip: &str,
    refusal: String,
) -> Result<ResolveOutcome> {
    let args = ["merge", "--no-commit", "--no-ff", "-m", msg, tip];
    let out = run_git_bounded(repo, &args)?;
    let success = !out.timed_out && out.status.map(|s| s.success()).unwrap_or(false);
    let pending = if out.timed_out {
        None
    } else {
        git(repo, &["ls-files", "--unmerged"])
            .ok()
            .map(|u| parse_unmerged_paths(&u))
    };
    match pending {
        Some(p) if !p.is_empty() => Ok(ResolveOutcome::ManualPending(p)),
        _ => {
            let aborted = abort_state_phrase(&abort_trial_merge(repo));
            let how = if success {
                "it merges cleanly, so it would be committed"
            } else {
                "the no-commit merge did not leave conflict markers"
            };
            bail!("{refusal}\n(resolve-merge manual: {how}; nothing was committed. {aborted})")
        }
    }
}
