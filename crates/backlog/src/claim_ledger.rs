//! The machine-global, project-identity-keyed **claim ledger**: what makes
//! `backlog next --claim` mutually exclusive across CHECKOUTS, not merely
//! across processes sharing one store file (backlog 709ff549).
//!
//! # The defect this closes
//!
//! The store follows the checkout by design — `config::locate` walks up
//! to the nearest `.git` (a FILE in a linked worktree counts), so every
//! worktree owns `<worktree>/.backlog/tasks.toml`. That is deliberate and is
//! NOT changed here: CLAUDE.md §8 forbids a worktree writing the main tree's
//! tracked file, and the queue is meant to be an ordinary tracked file that
//! merges like any other.
//!
//! The claim's mutual exclusion, however, was keyed on the resolved STORE PATH
//! (`store::tasks_lock_path` = `<store>.lock`). Per-checkout store ⇒
//! per-checkout lock ⇒ two checkouts of the SAME project held two disjoint
//! locks over two disjoint (diverged) files, and both handed out the same task
//! id. The critical section was real; its SCOPE was wrong. Measured on this
//! machine 2026-08-10/11: 17 checkouts of `harness` holding
//! `.backlog/tasks.toml` at 10 distinct sizes.
//!
//! # The shape of the fix
//!
//! Identity, not location. A claim is now recorded in a ledger that lives
//! OUTSIDE every checkout — under `~/.backlog/claims/<project-slug>.json`,
//! where the slug is [`crate::lock::project_slug`], the SAME
//! project-identity hash the driver lock already uses, which normalizes a
//! linked worktree to its main working tree
//! (`store::canonical_project_id`). Two checkouts of one project therefore
//! read and write ONE ledger, whatever their stores say.
//!
//! ## Lock order (never invert this)
//!
//!   1. the project-scoped ledger lock `~/.backlog/claims/<slug>.lock`
//!      (WIDE: every checkout of this project), then
//!   2. the per-store tasks-file lock `<store>.lock`
//!      (NARROW: this checkout only, still needed — it is what protects the
//!      local file against this checkout's own `add`/`edit`/`mark_*`).
//!
//! Both are acquired in that order on every claim; nothing in this crate
//! acquires them the other way round. A new caller that needs both MUST take
//! the ledger lock first, or the two orders can deadlock against each other.
//!
//! ## Every undetermined condition REFUSES the claim
//!
//! Refusing is deliberately DISTINGUISHABLE from "the queue is empty": this
//! module returns `Determination<Option<Task>>`, and `main.rs` maps
//! `Undetermined` to a non-zero exit with the reason on stderr and nothing on
//! stdout (the same shape `guard_store_divergence` already uses), never to the
//! `no pending tasks` line. A driver that reads "nothing to do" when the truth
//! is "I could not tell whether this task is already being worked in another
//! checkout" is the exact fail-open CLAUDE.md §3 forbids. Undetermined here
//! covers: ledger directory not creatable, ledger lock not acquired
//! (contended/IO), ledger unreadable, ledger unparseable, ledger not writable,
//! and — one layer down, in `store::next_claim_with` — the tasks-file lock not
//! held. The project identity itself is resolved by `main.rs` BEFORE this
//! module is reached, and an unresolvable identity refuses there.
//!
//! ## What ages out, and why
//!
//! An entry stops excluding its task after [`crate::store::CLAIM_STALE_SECS`]
//! (1h) — the identical window in which `store`'s own claim-reclaim already
//! re-offers a `claimed` task whose claimant died. Keeping the ledger stricter
//! than the store would make such a task permanently unclaimable in EVERY
//! checkout, which is not "the restrictive side" but a deadlock. Entries are
//! kept (for a human reading the file) until [`LEDGER_RETENTION_SECS`], then
//! pruned on the next write so the file stays bounded.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Result;
use harness_core::config::base_dir;
use harness_core::verdict::Determination;
use serde::{Deserialize, Serialize};

use crate::store::{self, CLAIM_STALE_SECS};
use crate::task::{hashkey, Task};

/// How long a claim record is KEPT (not how long it excludes — that is
/// [`CLAIM_STALE_SECS`]). Long enough that a human can still see who took what
/// last week; short enough that the file cannot grow without bound.
const LEDGER_RETENTION_SECS: i64 = 7 * 24 * 3600;

/// A ledger lockfile older than this is treated as abandoned by a crashed
/// holder and reaped. It must comfortably exceed the WHOLE critical section,
/// whose worst case is dominated by the inner tasks-file lock's own bounded
/// acquire budget (`store::TASKS_LOCK_MAX_ATTEMPTS` × `TASKS_LOCK_SLEEP` ≈ 8s)
/// — otherwise a legitimately slow holder is reaped mid-claim and a second
/// claimer barges into the critical section, which is the very double-dispatch
/// this module exists to prevent.
const LEDGER_LOCK_STALE_SECS: u64 = 30;
/// Bounded blocking acquire: attempts × sleep ≈ 12s. Deliberately BELOW
/// [`LEDGER_LOCK_STALE_SECS`] so a waiter queued behind a legitimately slow
/// holder gives up (→ `Undetermined`, i.e. refuse and let the caller retry)
/// rather than concluding the holder crashed and reaping it.
const LEDGER_LOCK_MAX_ATTEMPTS: u32 = 2400;
const LEDGER_LOCK_SLEEP: std::time::Duration = std::time::Duration::from_millis(5);

fn now_unix() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        // A pre-epoch clock makes every entry look infinitely OLD, which would
        // stop excluding anything (fail-open). 0 makes them look infinitely
        // NEW, i.e. everything already claimed stays excluded — the
        // restrictive side.
        Err(_) => 0,
    }
}

/// One recorded claim. Enough for a human to find the work: which task, when,
/// and which checkout took it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ClaimRecord {
    /// Task id as it appears in the claiming checkout's store.
    pub(crate) id: String,
    /// `task::hashkey(title, project)` — the content identity condukt's
    /// cross-session claim registry keys on, so a reader can correlate the two
    /// even if ids diverge between checkouts.
    pub(crate) hashkey: String,
    pub(crate) title: String,
    pub(crate) project: String,
    pub(crate) claimed_at: i64,
    /// The cwd of the claiming process — i.e. WHICH CHECKOUT took it.
    pub(crate) checkout: String,
    /// The store file the claim was written into.
    pub(crate) store: String,
    /// The claiming process's pid. Observability only: `backlog` is a one-shot
    /// CLI, so this pid is dead by the time anyone reads the ledger (the same
    /// reason `lock::LockInfo::pid` is not used to judge staleness).
    pub(crate) pid: u32,
}

/// The ledger file's contents.
///
/// `entries` has NO `#[serde(default)]` on purpose: a file that does not carry
/// the field is not a ledger, and reading it as "no claims" would be the empty
/// set standing in for an unread one.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Ledger {
    entries: Vec<ClaimRecord>,
}

/// The claims directory. `override_dir` is for tests; production always uses
/// the machine-global `~/.backlog/claims`, which is the whole point — a ledger
/// inside any checkout would diverge exactly like the store does.
fn claims_dir(override_dir: Option<&Path>) -> PathBuf {
    match override_dir {
        Some(d) => d.to_path_buf(),
        None => base_dir("backlog").join("claims"),
    }
}

fn ledger_path(dir: &Path, identity: &str) -> PathBuf {
    dir.join(format!("{}.json", crate::lock::project_slug(identity)))
}

fn ledger_lock_path(dir: &Path, identity: &str) -> PathBuf {
    dir.join(format!("{}.lock", crate::lock::project_slug(identity)))
}

/// RAII: releases the ledger lock on every exit path (Ok, Err, unwind).
struct LedgerLockGuard {
    path: PathBuf,
}

impl Drop for LedgerLockGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Is the lockfile obviously abandoned (mtime older than the stale window)?
///
/// Both failure branches answer `false` = "not stale" = do NOT reap: a
/// lockfile we cannot stat, or a clock that reads its mtime as in the future,
/// is not evidence that its holder died. Refusing to reap keeps a live
/// holder's critical section intact (the restrictive side); the caller's
/// bounded budget still ends in `Undetermined` rather than waiting forever.
fn lock_is_stale(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = meta.modified() else {
        return false;
    };
    match modified.elapsed() {
        Ok(age) => age.as_secs() >= LEDGER_LOCK_STALE_SECS,
        Err(_) => false,
    }
}

/// Acquire the project-scoped ledger lock, BLOCKING (bounded). `create_new`
/// (O_EXCL) is the atomic acquire: exactly one racer creates the file.
///
/// Failure to acquire is `Undetermined`, never "no claims": we did not get to
/// look at the ledger at all.
fn acquire_ledger_lock(path: &Path) -> Determination<LedgerLockGuard> {
    for _ in 0..LEDGER_LOCK_MAX_ATTEMPTS {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
        {
            Ok(_f) => {
                return Determination::known(LedgerLockGuard {
                    path: path.to_path_buf(),
                })
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if lock_is_stale(path) {
                    let _ = std::fs::remove_file(path);
                    continue;
                }
                std::thread::sleep(LEDGER_LOCK_SLEEP);
                continue;
            }
            Err(e) => {
                return Determination::undetermined(format!(
                    "the project-wide claim ledger lock {} could not be created: {e}",
                    path.display()
                ))
            }
        }
    }
    Determination::undetermined(format!(
        "the project-wide claim ledger lock {} stayed held for the whole ~{}s acquire budget; \
         another checkout is claiming right now",
        path.display(),
        (u64::from(LEDGER_LOCK_MAX_ATTEMPTS) * LEDGER_LOCK_SLEEP.as_millis() as u64) / 1000
    ))
}

/// Read the ledger.
///
/// An ABSENT file is a real observation — no checkout of this project has ever
/// claimed anything, so the set of live claims is genuinely empty (the same
/// `Absent` ≠ `Unreadable` distinction `divergence::LegacyStore` draws). Every
/// other outcome (unreadable, unparseable, a directory where a file belongs)
/// is `Undetermined`.
fn read_ledger(path: &Path) -> Determination<Ledger> {
    match std::fs::read_to_string(path) {
        Ok(txt) => match serde_json::from_str::<Ledger>(&txt) {
            Ok(l) => Determination::known(l),
            Err(e) => Determination::undetermined(format!(
                "the project-wide claim ledger {} exists but could not be parsed ({e}); which \
                 tasks other checkouts already claimed is UNKNOWN, not known to be none",
                path.display()
            )),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Determination::known(Ledger {
            entries: Vec::new(),
        }),
        Err(e) => Determination::undetermined(format!(
            "the project-wide claim ledger {} exists but could not be read ({e}); which tasks \
             other checkouts already claimed is UNKNOWN, not known to be none",
            path.display()
        )),
    }
}

/// Publish the ledger atomically (temp file + rename), so a concurrent reader
/// never observes a partially-written ledger and mistakes it for a corrupt one.
fn write_ledger(path: &Path, ledger: &Ledger) -> std::result::Result<(), String> {
    let json = serde_json::to_string_pretty(ledger)
        .map_err(|e| format!("serializing the claim ledger: {e}"))?;
    let tmp = path.with_extension(format!(
        "tmp.{}.{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::write(&tmp, json.as_bytes()).map_err(|e| format!("writing {}: {e}", tmp.display()))?;
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(format!("publishing {}: {e}", path.display()))
        }
    }
}

/// Claim the next eligible task for `identity`'s project, excluding whatever
/// ANY checkout of that project has already claimed.
///
/// The whole critical section, in order (see the module docs on lock order):
/// acquire the project-scoped ledger lock → read the ledger → select a
/// candidate from THIS checkout's store that is not already claimed
/// project-wide → record it in the ledger → write `claimed` into the local
/// store → release (guards drop).
///
/// The reservation is written BEFORE the local store, and a failed reservation
/// aborts the claim without touching the local store: a claim that is visible
/// only in this checkout is exactly the invisible double-dispatch this exists
/// to prevent.
///
/// `Err` is reserved for a genuine store IO failure (already a non-zero exit
/// via `main`); `Determination::Undetermined` is a deliberate REFUSAL to
/// claim. Both resolve to the restrictive side — no task, non-zero exit — but
/// they are kept apart so a refusal never reads as a crash, or vice versa.
pub(crate) fn claim_next(
    store_path: &Path,
    tag_filter: Option<&str>,
    project_filter: Option<&str>,
    identity: &str,
    override_dir: Option<&Path>,
) -> Result<Determination<Option<Task>>> {
    let dir = claims_dir(override_dir);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return Ok(Determination::undetermined(format!(
            "the project-wide claim ledger directory {} could not be created ({e}); a claim that \
             cannot be recorded project-wide would be invisible to every other checkout",
            dir.display()
        )));
    }

    // 1. WIDE lock first: every checkout of this project serializes here.
    let _ledger_guard = match acquire_ledger_lock(&ledger_lock_path(&dir, identity)) {
        Determination::Known(g) => g,
        // Forwarded, not re-minted: the origin already recorded this give-up.
        blocked @ Determination::Undetermined(_) => return Ok(blocked.map(|_| None)),
    };

    let path = ledger_path(&dir, identity);
    let mut ledger = match read_ledger(&path) {
        Determination::Known(l) => l,
        blocked @ Determination::Undetermined(_) => return Ok(blocked.map(|_| None)),
    };

    let now = now_unix();
    // Only LIVE claims exclude. See the module docs: mirroring the store's own
    // stale-claim reclaim window is what keeps a dead claimant from making a
    // task permanently unclaimable in every checkout.
    let excluded: HashSet<String> = ledger
        .entries
        .iter()
        .filter(|e| now.saturating_sub(e.claimed_at) < CLAIM_STALE_SECS)
        .map(|e| e.id.clone())
        .collect();

    let checkout = match std::env::current_dir() {
        Ok(d) => d.to_string_lossy().into_owned(),
        // Observability field only — it names who claimed, it does not gate
        // anything — so a failure to read the cwd is recorded verbatim rather
        // than refusing the claim.
        Err(e) => format!("<cwd unavailable: {e}>"),
    };
    let store_display = store_path.display().to_string();

    let mut reserve = |t: &Task| -> std::result::Result<(), String> {
        ledger.entries.push(ClaimRecord {
            id: t.id.clone(),
            hashkey: hashkey(&t.title, &t.project),
            title: t.title.clone(),
            project: t.project.clone(),
            claimed_at: now,
            checkout: checkout.clone(),
            store: store_display.clone(),
            pid: std::process::id(),
        });
        ledger
            .entries
            .retain(|e| now.saturating_sub(e.claimed_at) < LEDGER_RETENTION_SECS);
        write_ledger(&path, &ledger)
    };

    // 2. NARROW lock inside: `next_claim_with` takes the tasks-file lock and
    //    fails closed (Undetermined) if it cannot hold it.
    store::next_claim_with(
        store_path,
        tag_filter,
        project_filter,
        &excluded,
        &mut reserve,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_core::verdict::Required;

    /// A directory no other test can land in. The nanosecond clock ALONE is
    /// not enough: two of these tests running on parallel `cargo test` threads
    /// read the same timestamp (observed — one test then added its task to the
    /// other's store and was rejected as a duplicate), so a process-global
    /// counter disambiguates.
    fn tmp() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let d = std::env::temp_dir().join(format!(
            "backlog-ledger-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// One pending task, with a title unique to `dir` so the content-hashkey
    /// duplicate guard (which also consults the machine-global cross-session
    /// claim registry) can never make one test's fixture collide with
    /// another's.
    fn store_with_one_task(dir: &Path) -> PathBuf {
        let p = dir.join("tasks.toml");
        let title = format!("A task {}", dir.display());
        crate::store::add(&p, &title, "/proj", vec![], "", 100).unwrap();
        p
    }

    /// An unreadable ledger must REFUSE, and the refusal must not be
    /// expressible as "no task" — `Known(None)` would be read by a driver as an
    /// empty queue.
    #[test]
    fn unparseable_ledger_refuses_the_claim() {
        let dir = tmp();
        let store_path = store_with_one_task(&dir);
        let claims = dir.join("claims");
        std::fs::create_dir_all(&claims).unwrap();
        std::fs::write(ledger_path(&claims, "/proj"), "{ not json").unwrap();

        let out = claim_next(&store_path, None, None, "/proj", Some(&claims)).unwrap();
        match out.require() {
            Required::Determined(t) => panic!("expected a refusal, got {t:?}"),
            Required::Blocked(v) => match v.reason() {
                Some(r) => assert!(
                    r.as_str().contains("ledger"),
                    "the reason must name the ledger: {r}"
                ),
                None => panic!("a refusal must carry a reason: {v:?}"),
            },
        }
        // ...and the local store must be untouched: nothing was claimed.
        assert!(crate::store::load(&store_path).unwrap()[0].is_pending());
    }

    /// ANTI-VACUITY for the test above: a readable (absent) ledger claims.
    #[test]
    fn readable_ledger_claims_and_records() {
        let dir = tmp();
        let store_path = store_with_one_task(&dir);
        let claims = dir.join("claims");

        let out = claim_next(&store_path, None, None, "/proj", Some(&claims)).unwrap();
        let task = match out.require() {
            Required::Determined(t) => t.expect("a pending task must be claimed"),
            Required::Blocked(v) => panic!("unexpected refusal: {v:?}"),
        };
        assert_eq!(task.status, "claimed");

        let ledger = match read_ledger(&ledger_path(&claims, "/proj")).require() {
            Required::Determined(l) => l,
            Required::Blocked(v) => panic!("ledger must be readable: {v:?}"),
        };
        assert_eq!(ledger.entries.len(), 1);
        assert_eq!(ledger.entries[0].id, task.id);
        assert!(!ledger.entries[0].hashkey.is_empty());
    }

    /// The cross-checkout core, in-process: a task already recorded in the
    /// ledger is NOT handed out again, even though it is still `pending` in
    /// THIS checkout's own store (which is exactly the diverged state).
    #[test]
    fn a_task_already_claimed_elsewhere_is_not_handed_out_from_a_second_store() {
        let dir = tmp();
        let store_a = dir.join("a");
        let store_b = dir.join("b");
        std::fs::create_dir_all(&store_a).unwrap();
        std::fs::create_dir_all(&store_b).unwrap();
        let a = store_with_one_task(&store_a);
        // Checkout B's store is a byte copy of A's: same id, still pending.
        let b = store_b.join("tasks.toml");
        std::fs::copy(&a, &b).unwrap();
        let claims = dir.join("claims");

        let first = match claim_next(&a, None, None, "/proj", Some(&claims))
            .unwrap()
            .require()
        {
            Required::Determined(t) => t.expect("first claim"),
            Required::Blocked(v) => panic!("unexpected refusal: {v:?}"),
        };

        let second = match claim_next(&b, None, None, "/proj", Some(&claims))
            .unwrap()
            .require()
        {
            Required::Determined(t) => t,
            Required::Blocked(v) => panic!("unexpected refusal: {v:?}"),
        };
        assert!(
            second.is_none(),
            "the second checkout was handed {second:?}, already claimed as {} elsewhere",
            first.id
        );
        // B's own store must be untouched (nothing was claimed there).
        assert!(crate::store::load(&b).unwrap()[0].is_pending());
    }

    /// A ledger entry older than the store's own reclaim window stops
    /// excluding, or a dead claimant would strand the task in EVERY checkout.
    #[test]
    fn a_stale_ledger_entry_stops_excluding() {
        let dir = tmp();
        let store_path = store_with_one_task(&dir);
        let claims = dir.join("claims");
        std::fs::create_dir_all(&claims).unwrap();
        let id = crate::store::load(&store_path).unwrap()[0].id.clone();
        let stale = Ledger {
            entries: vec![ClaimRecord {
                id: id.clone(),
                hashkey: "0".to_string(),
                title: "A task".to_string(),
                project: "/proj".to_string(),
                claimed_at: now_unix() - CLAIM_STALE_SECS - 1,
                checkout: "/elsewhere".to_string(),
                store: "/elsewhere/.backlog/tasks.toml".to_string(),
                pid: 1,
            }],
        };
        write_ledger(&ledger_path(&claims, "/proj"), &stale).unwrap();

        match claim_next(&store_path, None, None, "/proj", Some(&claims))
            .unwrap()
            .require()
        {
            Required::Determined(t) => {
                assert_eq!(t.expect("a stale claim must be reclaimable").id, id)
            }
            Required::Blocked(v) => panic!("unexpected refusal: {v:?}"),
        }
    }

    /// A held ledger lock must REFUSE (undetermined), never claim unprotected
    /// and never answer "no task".
    #[test]
    fn a_held_ledger_lock_refuses_rather_than_claiming_unprotected() {
        let dir = tmp();
        let store_path = store_with_one_task(&dir);
        let claims = dir.join("claims");
        std::fs::create_dir_all(&claims).unwrap();
        // Hold the lock, fresh mtime, so it is neither reapable nor free. The
        // acquire budget is bounded, so this returns rather than hanging.
        let held = ledger_lock_path(&claims, "/proj");
        std::fs::write(&held, "").unwrap();

        let out = claim_next(&store_path, None, None, "/proj", Some(&claims)).unwrap();
        match out.require() {
            Required::Determined(t) => panic!("expected a refusal, got {t:?}"),
            Required::Blocked(v) => match v.reason() {
                Some(r) => assert!(
                    r.as_str().contains("lock"),
                    "the reason must name the lock: {r}"
                ),
                None => panic!("a refusal must carry a reason: {v:?}"),
            },
        }
        assert!(crate::store::load(&store_path).unwrap()[0].is_pending());
    }
}
