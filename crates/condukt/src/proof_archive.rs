//! Durable copies of a task worktree's `tdd` RED/GREEN proofs (backlog
//! ea6547c9).
//!
//! `tdd red` / `tdd green` write their proofs into `<worktree>/.tdd/` — inside
//! the very directory every condukt teardown path deletes. Before this module,
//! `condukt worktree remove` (and `discard`, `reap`, `cleanup --remove`) took
//! the proofs down with the worktree, and afterwards neither `tdd oracle` nor
//! `condukt state check-oracle` could see the F→P oracle any more.
//!
//! Contract:
//!
//! * [`persist_before_teardown`] copies every `<wt>/.tdd/*.red.json` /
//!   `*.green.json` into the condukt home BEFORE a teardown path deletes the
//!   worktree, keyed by run id + task id ([`archive_dir`]). Each copy is
//!   written to a temp file, fsynced, renamed into place, and READ BACK and
//!   compared byte-for-byte: an unobserved write is not an observation.
//! * Any failure (IO error, unwritable dir, unreadable `.tdd`, read-back
//!   mismatch) is an `Err`. Every caller treats that `Err` as "refuse the
//!   teardown": the worktree stays on disk, so the proofs are never lost.
//! * No proofs (no `.tdd` dir, or no proof files in it) = nothing to persist =
//!   `Ok` with zero files; the teardown proceeds.
//! * The run/task identity comes from the caller when it knows it (`discard`)
//!   or from the worktree's branch record `branch.<b>.conduktTask` (written by
//!   `worktree create --run`). A worktree with proofs but no readable identity
//!   is still persisted — under an `unattributed` key named after the
//!   directory — rather than lost; `check-oracle` cannot find such a copy by
//!   run/task, and the stderr note says where it went.
//! * [`oracle_dir`] is the read half: `check-oracle` and the `state set
//!   --status verified` gate run `tdd oracle` in the worktree when the
//!   worktree still holds the task's proofs, and otherwise in the archive dir
//!   when THAT holds them (its layout is `<dir>/.tdd/<task>.<kind>.json`, the
//!   same layout `tdd` reads from its cwd), so `tdd` itself judges the copies.

use crate::config::Config;
use anyhow::{bail, Context, Result};
use std::io::Write;
use std::path::{Path, PathBuf};

/// The proof-file directory name `tdd` uses by default (its `proof_dir`).
const PROOF_DIR: &str = ".tdd";

/// The durable proof archive for `run_id`/`task_id`:
/// `<run dir>/<run>.tdd-proofs/<task>`. Proof files live in its `.tdd/`
/// subdir so `tdd oracle` run with this dir as cwd reads them unchanged.
pub fn archive_dir(cfg: &Config, cwd: &Path, run_id: &str, task_id: &str) -> Result<PathBuf> {
    let safe = harness_core::store::safe_session;
    Ok(crate::state::run_dir(cfg, cwd, run_id)?
        .join(format!("{}.tdd-proofs", safe(run_id)))
        .join(safe(task_id)))
}

/// Archive for a worktree whose proofs could not be attributed to a run/task.
fn unattributed_dir(cfg: &Config, cwd: &Path, wt: &Path) -> Result<PathBuf> {
    let name = wt
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "worktree".to_string());
    Ok(crate::state::project_state_dir(cfg, cwd)?
        .join("tdd-proofs-unattributed")
        .join(format!(
            "{}-{}",
            harness_core::store::safe_session(&name),
            crate::state::now_secs()
        )))
}

/// The proof files (`*.red.json` / `*.green.json`) in `<wt>/.tdd/`, sorted.
/// No `.tdd` dir (or no worktree dir at all) is a real "none"; any other read
/// failure is an `Err` — "could not look" is not "nothing there".
pub fn proof_files(wt: &Path) -> Result<Vec<PathBuf>> {
    let dir = wt.join(PROOF_DIR);
    let rd = match std::fs::read_dir(&dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", dir.display())),
    };
    let mut out = Vec::new();
    for entry in rd {
        let entry = entry.with_context(|| format!("reading an entry of {}", dir.display()))?;
        let name = entry.file_name().to_string_lossy().to_string();
        if !(name.ends_with(".red.json") || name.ends_with(".green.json")) {
            continue;
        }
        let ft = entry
            .file_type()
            .with_context(|| format!("stat {}", entry.path().display()))?;
        if ft.is_file() {
            out.push(entry.path());
        }
    }
    out.sort();
    Ok(out)
}

/// What [`persist_before_teardown`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Persisted {
    /// Number of proof files copied (0 = nothing to persist).
    pub files: usize,
    /// Where they went (`None` when nothing was persisted).
    pub dir: Option<PathBuf>,
    /// Whether the copy is keyed by run + task (`false` = unattributed).
    pub attributed: bool,
}

impl Persisted {
    /// Name on stderr where the proofs went. Silent only when there were none.
    pub fn report(&self) {
        let Some(dir) = &self.dir else { return };
        if self.attributed {
            eprintln!(
                "condukt: persisted {} tdd proof file(s) to {}",
                self.files,
                dir.display()
            );
        } else {
            eprintln!(
                "condukt: persisted {} tdd proof file(s) to {} (UNATTRIBUTED: no run/task \
                 identity is recorded for this worktree's branch, so `state check-oracle` \
                 cannot find this copy by run/task)",
                self.files,
                dir.display()
            );
        }
    }
}

/// [`crate::worktree::discard`] preceded by [`persist_before_teardown`]: a
/// persist failure refuses the discard (worktree and branch left in place).
pub fn discard(
    cfg: &Config,
    cwd: &Path,
    repo: &Path,
    wt: &Path,
    branch: Option<&str>,
    identity: Option<(&str, &str)>,
) -> Result<()> {
    persist_before_teardown(cfg, cwd, repo, wt, identity)?.report();
    crate::worktree::discard(repo, wt, branch)
}

/// The `<run>/<task>` identity recorded for the worktree's checked-out branch,
/// or `None` when it cannot be read (detached HEAD, unregistered dir, no
/// record). Not an error: an unreadable identity only changes WHERE the proofs
/// are persisted, never WHETHER.
fn worktree_identity(repo: &Path, wt: &Path) -> Option<(String, String)> {
    let branch = crate::worktree::git(wt, &["symbolic-ref", "-q", "--short", "HEAD"]).ok()?;
    let id = crate::worktree::recorded_task_identity(repo, branch.trim()).ok()??;
    let (run, task) = id.split_once('/')?;
    if run.is_empty() || task.is_empty() {
        return None;
    }
    Some((run.to_string(), task.to_string()))
}

/// Copy `<wt>/.tdd`'s proofs durably into the condukt home before `wt` is torn
/// down (module docs). `identity` = `(run, task)` when the caller knows it;
/// otherwise it is read from the worktree's branch record. `Err` = the proofs
/// could not be persisted and the caller MUST NOT delete the worktree.
pub fn persist_before_teardown(
    cfg: &Config,
    cwd: &Path,
    repo: &Path,
    wt: &Path,
    identity: Option<(&str, &str)>,
) -> Result<Persisted> {
    let files = proof_files(wt).with_context(|| {
        format!(
            "refusing to tear down worktree {}: could not list its tdd proofs",
            wt.display()
        )
    })?;
    if files.is_empty() {
        return Ok(Persisted {
            files: 0,
            dir: None,
            attributed: false,
        });
    }
    let owned;
    let ident = match identity {
        Some((r, t)) => Some((r, t)),
        None => {
            owned = worktree_identity(repo, wt);
            owned.as_ref().map(|(r, t)| (r.as_str(), t.as_str()))
        }
    };
    let (dir, attributed) = match ident {
        Some((run, task)) => (archive_dir(cfg, cwd, run, task)?, true),
        None => (unattributed_dir(cfg, cwd, wt)?, false),
    };
    copy_verified(&files, &dir.join(PROOF_DIR)).with_context(|| {
        format!(
            "refusing to tear down worktree {}: its tdd proofs could not be persisted to {} \
             (the worktree is left in place so the proofs are not lost)",
            wt.display(),
            dir.display()
        )
    })?;
    Ok(Persisted {
        files: files.len(),
        dir: Some(dir),
        attributed,
    })
}

/// Copy each of `files` into `dest` (created), each durably and verified.
fn copy_verified(files: &[PathBuf], dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest).with_context(|| format!("creating {}", dest.display()))?;
    for src in files {
        let name = src
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("proof path {} has no file name", src.display()))?;
        let bytes = std::fs::read(src).with_context(|| format!("reading {}", src.display()))?;
        let target = dest.join(name);
        let tmp = dest.join(format!(
            ".{}.tmp-{}",
            name.to_string_lossy(),
            std::process::id()
        ));
        {
            let mut f = std::fs::File::create(&tmp)
                .with_context(|| format!("creating {}", tmp.display()))?;
            f.write_all(&bytes)
                .with_context(|| format!("writing {}", tmp.display()))?;
            f.sync_all()
                .with_context(|| format!("syncing {}", tmp.display()))?;
        }
        if let Err(e) = std::fs::rename(&tmp, &target) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e)
                .with_context(|| format!("renaming {} -> {}", tmp.display(), target.display()));
        }
        let back =
            std::fs::read(&target).with_context(|| format!("reading back {}", target.display()))?;
        if back != bytes {
            bail!(
                "read-back of {} does not match {} ({} vs {} bytes)",
                target.display(),
                src.display(),
                back.len(),
                bytes.len()
            );
        }
    }
    std::fs::File::open(dest)
        .and_then(|d| d.sync_all())
        .with_context(|| format!("syncing directory {}", dest.display()))?;
    Ok(())
}

/// Whether `dir/.tdd` holds a RED or GREEN proof for `task`. A probe that
/// errors counts as "holds" for the PRIMARY dir (see [`oracle_dir`]): the
/// caller then keeps judging there, which is the behaviour before archives
/// existed — never a switch to a different set of proofs on an IO error.
fn holds_proof(dir: &Path, task: &str) -> std::io::Result<bool> {
    let base = dir.join(PROOF_DIR);
    for kind in ["red", "green"] {
        match std::fs::symlink_metadata(base.join(format!("{task}.{kind}.json"))) {
            Ok(_) => return Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(false)
}

/// The directory `tdd oracle` must run in for `task`: `primary` (the recorded
/// worktree, or the cwd fallback) when it still holds the task's proofs or
/// when no archive holds them; otherwise the durable archive. Proofs in a live
/// worktree always win over the archived copy. Archive lookup errors (state
/// dir unresolvable) fall back to `primary`, where `tdd` then reports the
/// oracle it can (or cannot) see — this function never fabricates a verdict.
pub fn oracle_dir(cfg: &Config, cwd: &Path, run: &str, task: &str, primary: &Path) -> PathBuf {
    if !matches!(holds_proof(primary, task), Ok(false)) {
        return primary.to_path_buf();
    }
    match archive_dir(cfg, cwd, run, task) {
        Ok(a) if matches!(holds_proof(&a, task), Ok(true)) => a,
        _ => primary.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(p: &Path, s: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, s).unwrap();
    }

    #[test]
    fn proof_files_absent_dir_is_empty_and_filters_names() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(proof_files(tmp.path()).unwrap().is_empty());
        write(&tmp.path().join(".tdd/t1.red.json"), "{}");
        write(&tmp.path().join(".tdd/t1.green.json"), "{}");
        write(&tmp.path().join(".tdd/other.txt"), "x");
        let got = proof_files(tmp.path()).unwrap();
        assert_eq!(got.len(), 2, "{got:?}");
    }

    #[test]
    fn copy_verified_round_trips_and_fails_on_unwritable_dest() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("wt/.tdd/t1.red.json");
        write(&src, "{\"passed\":false}");
        let dest = tmp.path().join("arch/.tdd");
        copy_verified(std::slice::from_ref(&src), &dest).unwrap();
        assert_eq!(
            std::fs::read_to_string(dest.join("t1.red.json")).unwrap(),
            "{\"passed\":false}"
        );

        let locked = tmp.path().join("locked");
        std::fs::create_dir_all(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();
        let r = copy_verified(std::slice::from_ref(&src), &locked.join("a/.tdd"));
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(r.is_err(), "an unwritable archive must be an Err");
    }

    #[test]
    fn holds_proof_sees_either_kind() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!holds_proof(tmp.path(), "t1").unwrap());
        write(&tmp.path().join(".tdd/t1.green.json"), "{}");
        assert!(holds_proof(tmp.path(), "t1").unwrap());
        assert!(!holds_proof(tmp.path(), "t2").unwrap());
    }
}
