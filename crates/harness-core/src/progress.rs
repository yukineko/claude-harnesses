//! harness-core::progress — the one reusable "is this holder actually
//! **progressing**, or merely **alive**?" engine, shared by every cross-session
//! staleness reaper in the harness (backlog's lock, condukt's claim registry).
//!
//! # Why this exists
//!
//! Liveness signals cannot tell "progressing" from "hung". A held lock's
//! recorded `pid` is a one-shot CLI's pid — dead by the next command, so it
//! proves nothing. A fresh `heartbeat_at` (or a transcript whose mtime was just
//! touched) can equally accompany a session that is *frozen* on a stuck
//! deliverable. Reaping on any of those alone is exactly how a **live** session
//! gets its work force-stolen (the memory scar this module closes).
//!
//! The fix is to judge **progress** — durable work actually advancing — across
//! **multiple signals** and **multiple samples**, three-valued, and let a reaper
//! fire ONLY on confirmed non-progress:
//!
//! 1. **Progress, not liveness.** A *signal* is something that advances when
//!    real work happens: the repo's git HEAD, a run's max `updated_at`, a
//!    session/agent transcript's `(size, mtime)`. `heartbeat_at`/`pid`/mtime-touch
//!    alone are NOT authoritative and are not fed here.
//! 2. **Multi-signal.** Signals are folded into one deterministic
//!    [`ProgressFingerprint`]. ANY signal advancing changes the fingerprint ⇒
//!    [`Liveness::Progressing`]. Only when the *whole* fingerprint is unchanged
//!    can a holder be Stalled — i.e. every fed signal is frozen at once.
//! 3. **Multi-sample.** A single observation is one point in time and MUST NOT
//!    yield Stalled — a first observation (no prior snapshot) is always
//!    [`Determination::Undetermined`]. [`classify`] persists a per-target
//!    [`Snapshot`] (fingerprint + first-seen time), and Stalled requires the
//!    fingerprint UNCHANGED across two or more samples spanning at least
//!    `window_secs` (default [`DEFAULT_WINDOW_SECS`]).
//! 4. **Three-valued, restrictive.** The verdict is
//!    `Determination<Liveness>` where [`Liveness`] is `Progressing`/`Stalled`.
//!    An unreadable/absent signal (the caller hands a
//!    [`Determination::Undetermined`] fingerprint), no prior snapshot, or a
//!    window that has not yet elapsed ⇒ `Undetermined`. **Undetermined and
//!    Progressing BOTH mean "do NOT reap"** — only a confirmed `Known(Stalled)`
//!    is reap-eligible. There is no `Default` and no `From<bool>`: a permissive
//!    answer cannot appear for free.
//!
//! The engine itself is signal-agnostic: each caller reads the signals it can
//! and builds a `Determination<ProgressFingerprint>` (Undetermined if it could
//! not read one), then calls [`sample`]. The reap decision belongs to the
//! caller, which reaps iff the verdict is `Known(Stalled)`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::hash::{fnv1a64, Fnv1a64};
use crate::verdict::Determination;

/// The three-valued liveness of durable work is expressed as
/// `Determination<Liveness>`; this is the *known* pair. It is deliberately NOT a
/// third-variant "Undetermined" enum member — "could not determine" lives in
/// [`Determination::Undetermined`] so it shares the harness-wide fail-closed
/// type and cannot be defaulted into a permissive value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    /// At least one durable signal advanced since the prior sample.
    Progressing,
    /// Every durable signal has been frozen across a full window spanning ≥2
    /// samples. The ONLY reap-eligible verdict.
    Stalled,
}

/// Default multi-sample window: a fingerprint must stay unchanged for at least
/// this many seconds (across ≥2 samples) before a holder can be judged Stalled.
/// Chosen well below the heartbeat stale-TTL (1800s) so a genuinely dead holder
/// still ages out promptly, while a live holder that merely paused for a moment
/// is never mistaken for stalled.
pub const DEFAULT_WINDOW_SECS: i64 = 90;

/// A deterministic digest over an ordered set of `(signal-name, opaque-value)`
/// entries. Two fingerprints are equal iff every signal has the identical value
/// in the identical order — so ANY signal advancing yields a different
/// fingerprint (⇒ Progressing), and only a wholesale freeze keeps it equal.
///
/// The value bytes are opaque to the engine: a caller may use a git SHA, a
/// `size:mtime` pair, an `updated_at` integer — anything that changes when work
/// advances. Length-prefixing each field makes the digest unambiguous (so
/// `("a","bc")` and `("ab","c")` never collide).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgressFingerprint(String);

impl ProgressFingerprint {
    /// Build a fingerprint from ordered `(name, value)` signal entries. Order is
    /// significant and callers must feed signals in a stable order.
    #[must_use]
    pub fn from_entries<N: AsRef<str>, V: AsRef<[u8]>>(entries: &[(N, V)]) -> Self {
        let mut h = Fnv1a64::new();
        for (name, value) in entries {
            let name = name.as_ref().as_bytes();
            let value = value.as_ref();
            h.update(&(name.len() as u64).to_le_bytes());
            h.update(name);
            h.update(&(value.len() as u64).to_le_bytes());
            h.update(value);
        }
        ProgressFingerprint(format!("{:016x}", h.finish()))
    }

    /// The digest as a stable hex string (what is persisted in a [`Snapshot`]).
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The persisted per-target progress state: the last observed fingerprint, when
/// that fingerprint was *first* seen (the anchor the window measures from), and
/// when it was last refreshed. Stored as `<store_dir>/<hash(target_key)>.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    /// The fingerprint observed at `first_seen`/`last_seen`.
    pub fingerprint: String,
    /// Unix seconds when this fingerprint value was first observed. Preserved
    /// across samples while the fingerprint is unchanged — this is what the
    /// window is measured against, so a long freeze accrues toward Stalled.
    pub first_seen: i64,
    /// Unix seconds of the most recent sample.
    pub last_seen: i64,
}

/// Pure classification: given the prior snapshot (if any), the current
/// fingerprint observation, the current time, and the window, decide the
/// verdict AND what snapshot (if any) should be persisted next.
///
/// This is a pure function of its inputs — all IO (reading/writing the snapshot,
/// reading the signals) lives in [`sample`] and the callers — so the whole
/// truth table is unit-testable without a filesystem.
///
/// Rules (each maps "cannot determine" to `Undetermined`, never to a reap):
/// * `current` is `Undetermined` (a signal was unreadable) ⇒ `Undetermined`,
///   and **do not overwrite** the prior snapshot (a transient read failure must
///   not reset the freeze clock). Returns `None` for the next snapshot.
/// * `current` is `Known(fp)`:
///   * no prior snapshot ⇒ first observation ⇒ `Undetermined`; persist a fresh
///     snapshot anchored at `now`.
///   * prior fingerprint differs ⇒ something advanced ⇒ `Progressing`; persist a
///     fresh snapshot anchored at `now`.
///   * prior fingerprint equals `fp` (frozen):
///     * elapsed since `first_seen` ≥ `window_secs` ⇒ `Stalled`; persist,
///       preserving the original `first_seen`.
///     * else ⇒ window not yet elapsed ⇒ `Undetermined`; persist, preserving
///       the original `first_seen`.
pub fn classify(
    prev: Option<&Snapshot>,
    current: &Determination<ProgressFingerprint>,
    now: i64,
    window_secs: i64,
) -> (Determination<Liveness>, Option<Snapshot>) {
    let fp = match current {
        // A signal could not be read: cannot compare, cannot claim a freeze.
        // Keep the prior snapshot intact so a blip does not reset the clock.
        Determination::Undetermined(why) => {
            return (Determination::Undetermined(why.clone()), None);
        }
        Determination::Known(fp) => fp,
    };

    let fresh = Snapshot {
        fingerprint: fp.as_str().to_string(),
        first_seen: now,
        last_seen: now,
    };

    match prev {
        // First ever observation for this target: one sample, never Stalled.
        None => (
            Determination::undetermined("first progress observation (no prior snapshot)"),
            Some(fresh),
        ),
        Some(prev) if prev.fingerprint != fp.as_str() => {
            // A durable signal advanced ⇒ Progressing. Re-anchor the window.
            (Determination::Known(Liveness::Progressing), Some(fresh))
        }
        Some(prev) => {
            // Frozen fingerprint. Measure the freeze from the ORIGINAL first_seen.
            let elapsed = now.saturating_sub(prev.first_seen);
            let persisted = Snapshot {
                fingerprint: fp.as_str().to_string(),
                first_seen: prev.first_seen,
                last_seen: now,
            };
            if elapsed >= window_secs {
                (Determination::Known(Liveness::Stalled), Some(persisted))
            } else {
                (
                    Determination::undetermined(
                        "progress frozen but multi-sample window has not elapsed",
                    ),
                    Some(persisted),
                )
            }
        }
    }
}

/// The snapshot file for `target_key` under `store_dir`. The key is hashed so it
/// never leaks path separators into the filename.
fn snapshot_path(store_dir: &Path, target_key: &str) -> PathBuf {
    store_dir.join(format!("{:016x}.json", fnv1a64(target_key.as_bytes())))
}

/// Read a persisted snapshot. A missing file is a legitimate "no prior sample"
/// (`None`); a present-but-corrupt file is also collapsed to `None` here — that
/// is the *protective* reading, because a `None` prior yields `Undetermined`
/// (never Stalled) from [`classify`], so a corrupt snapshot can never cause a
/// reap. It is simply re-anchored on the next write.
fn read_snapshot(path: &Path) -> Option<Snapshot> {
    let txt = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&txt).ok()
}

/// Atomically persist a snapshot (temp + rename), so a concurrent reader never
/// observes a half-written file (a torn read parses as corrupt ⇒ `None` ⇒
/// Undetermined, still protective). Best-effort: an IO failure is swallowed
/// because it only affects *future* samples (which fall back to "no prior" ⇒
/// Undetermined), never the verdict just computed from a snapshot that WAS read.
fn write_snapshot(path: &Path, snap: &Snapshot) {
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    let Ok(json) = serde_json::to_string_pretty(snap) else {
        return;
    };
    let tmp = path.with_extension(format!("tmp.{}.{}", std::process::id(), now_unix_nanos()));
    if std::fs::write(&tmp, &json).is_err() {
        return;
    }
    if std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

fn now_unix_nanos() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// Read the prior snapshot for `target_key`, [`classify`] the `current`
/// observation against it, persist the next snapshot, and return the verdict.
///
/// This is the one call a reaper makes per probe: it advances the multi-sample
/// state machine by exactly one step. The verdict is `Known(Stalled)` only when
/// the fingerprint has been frozen across the window; a caller reaps **iff** it
/// sees `Known(Stalled)` — `Progressing` and `Undetermined` both mean keep.
pub fn sample(
    store_dir: &Path,
    target_key: &str,
    current: Determination<ProgressFingerprint>,
    now: i64,
    window_secs: i64,
) -> Determination<Liveness> {
    let path = snapshot_path(store_dir, target_key);
    let prev = read_snapshot(&path);
    let (verdict, next) = classify(prev.as_ref(), &current, now, window_secs);
    if let Some(snap) = next {
        write_snapshot(&path, &snap);
    }
    verdict
}

// ---------------------------------------------------------------------------
// Signal readers (convenience) — harness-specific durable signals that callers
// fold into a fingerprint. These are the ONLY IO the progress engine ships; the
// core ([`classify`]/[`sample`]/[`ProgressFingerprint`]) stays signal-agnostic.
// Each returns `Determination<Vec<u8>>`: a real opaque value when the signal
// could be read, `Undetermined` when it could not — so an unreadable OR absent
// signal propagates to an `Undetermined` fingerprint (never a silent freeze).
// ---------------------------------------------------------------------------

/// The repo's git HEAD as a durable progress signal: a new commit advances it.
/// `Undetermined` when the directory is not a git repo, git is absent, the
/// command failed, or the output was empty — none of which is "HEAD is frozen".
pub fn git_head_signal(repo: &Path) -> Determination<Vec<u8>> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "HEAD"])
        .output();
    match output {
        Ok(o) if o.status.success() => {
            let sha = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if sha.is_empty() {
                Determination::undetermined(format!("git HEAD empty in {}", repo.display()))
            } else {
                Determination::Known(sha.into_bytes())
            }
        }
        Ok(o) => Determination::undetermined(format!(
            "git rev-parse HEAD failed in {} (exit {:?})",
            repo.display(),
            o.status.code()
        )),
        Err(e) => Determination::undetermined(format!(
            "git rev-parse HEAD could not be spawned in {}: {e}",
            repo.display()
        )),
    }
}

/// A file's `(size, mtime)` as a durable progress signal: durable work grows or
/// rewrites the file. `Undetermined` when the file is absent or its metadata is
/// unreadable — an absent transcript is "cannot judge progress", never a freeze.
pub fn file_growth_signal(path: &Path) -> Determination<Vec<u8>> {
    match std::fs::metadata(path) {
        Ok(meta) => {
            let mtime_nanos = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            Determination::Known(format!("{}:{}", meta.len(), mtime_nanos).into_bytes())
        }
        Err(e) => Determination::undetermined(format!(
            "file signal unreadable at {}: {e}",
            path.display()
        )),
    }
}

/// Locate a session's transcript under `projects_dir` (`<projects>/*/<id>.jsonl`,
/// newest match wins) and return its `(size, mtime)` growth signal. Split from
/// [`session_transcript_signal`] so it is testable against a fixture dir without
/// mutating `$HOME`. Any failure to find and stat a transcript file — an absent
/// or unreadable `projects_dir`, no matching session file — is `Undetermined`
/// (an absent transcript cannot prove the holder is frozen).
pub fn transcript_signal_in(projects_dir: &Path, session_id: &str) -> Determination<Vec<u8>> {
    if session_id.is_empty() {
        return Determination::undetermined("empty session id: no transcript to locate");
    }
    let target = format!("{session_id}.jsonl");
    let entries = match std::fs::read_dir(projects_dir) {
        Ok(e) => e,
        Err(e) => {
            return Determination::undetermined(format!(
                "projects dir {} unreadable: {e}",
                projects_dir.display()
            ))
        }
    };
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in entries.flatten() {
        let candidate = entry.path().join(&target);
        if let Ok(meta) = std::fs::metadata(&candidate) {
            let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
            if best.as_ref().is_none_or(|(t, _)| mtime > *t) {
                best = Some((mtime, candidate));
            }
        }
    }
    match best {
        Some((_, path)) => file_growth_signal(&path),
        None => Determination::undetermined(format!(
            "no transcript for session {session_id} under {}",
            projects_dir.display()
        )),
    }
}

/// The live transcript growth signal for a Claude session, resolved under
/// `~/.claude/projects/*/<session_id>.jsonl`. See [`transcript_signal_in`].
pub fn session_transcript_signal(session_id: &str) -> Determination<Vec<u8>> {
    let Some(home) = std::env::var_os("HOME") else {
        return Determination::undetermined("HOME unset: cannot locate session transcript");
    };
    let projects = PathBuf::from(home).join(".claude").join("projects");
    transcript_signal_in(&projects, session_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp(entries: &[(&str, &str)]) -> ProgressFingerprint {
        let owned: Vec<(&str, &[u8])> = entries.iter().map(|(n, v)| (*n, v.as_bytes())).collect();
        ProgressFingerprint::from_entries(&owned)
    }

    fn known(fp: ProgressFingerprint) -> Determination<ProgressFingerprint> {
        Determination::Known(fp)
    }

    // --- fingerprint determinism / sensitivity ----------------------------

    #[test]
    fn fingerprint_is_deterministic_and_order_sensitive() {
        assert_eq!(fp(&[("head", "aaa"), ("tx", "10")]).as_str().len(), 16);
        // Same entries, same order ⇒ equal.
        assert_eq!(fp(&[("head", "aaa")]), fp(&[("head", "aaa")]));
        // Any value change ⇒ different fingerprint (a signal advanced).
        assert_ne!(fp(&[("head", "aaa")]), fp(&[("head", "bbb")]));
        // Order matters.
        assert_ne!(fp(&[("a", "1"), ("b", "2")]), fp(&[("b", "2"), ("a", "1")]));
        // Length-prefixing prevents field-boundary collisions.
        assert_ne!(fp(&[("a", "bc")]), fp(&[("ab", "c")]));
    }

    // --- classify truth table (pure) --------------------------------------

    #[test]
    fn unreadable_signal_is_undetermined_and_keeps_prior_snapshot() {
        let prev = Snapshot {
            fingerprint: "frozen".into(),
            first_seen: 0,
            last_seen: 0,
        };
        let (verdict, next) = classify(
            Some(&prev),
            &Determination::undetermined("git HEAD unreadable"),
            10_000, // well past any window
            DEFAULT_WINDOW_SECS,
        );
        assert!(matches!(verdict, Determination::Undetermined(_)));
        // An unreadable signal must NOT overwrite (reset) the freeze clock.
        assert!(
            next.is_none(),
            "unreadable signal must not persist a snapshot"
        );
    }

    #[test]
    fn first_observation_is_undetermined_never_stalled() {
        let (verdict, next) = classify(
            None,
            &known(fp(&[("head", "a")])),
            1_000,
            DEFAULT_WINDOW_SECS,
        );
        assert!(
            matches!(verdict, Determination::Undetermined(_)),
            "a single sample must never be Stalled"
        );
        let snap = next.expect("first observation persists an anchor snapshot");
        assert_eq!(snap.first_seen, 1_000);
    }

    #[test]
    fn any_signal_advancing_is_progressing_even_with_ancient_heartbeat() {
        // The prior fingerprint was anchored ages ago; heartbeat is irrelevant
        // here — the ENGINE only sees fingerprints. One signal advanced.
        let prev = Snapshot {
            fingerprint: fp(&[("head", "a"), ("tx", "10")]).as_str().to_string(),
            first_seen: 0,
            last_seen: 0,
        };
        let now = 10_000; // far past the window
        let (verdict, next) = classify(
            Some(&prev),
            &known(fp(&[("head", "a"), ("tx", "11")])), // tx advanced
            now,
            DEFAULT_WINDOW_SECS,
        );
        assert_eq!(verdict, Determination::Known(Liveness::Progressing));
        // Progress re-anchors the window at `now`.
        assert_eq!(next.unwrap().first_seen, now);
    }

    #[test]
    fn frozen_across_window_is_stalled() {
        let anchored = fp(&[("head", "a"), ("tx", "10")]);
        let prev = Snapshot {
            fingerprint: anchored.as_str().to_string(),
            first_seen: 1_000,
            last_seen: 1_000,
        };
        let (verdict, next) = classify(
            Some(&prev),
            &known(anchored.clone()),
            1_000 + DEFAULT_WINDOW_SECS, // exactly the window
            DEFAULT_WINDOW_SECS,
        );
        assert_eq!(verdict, Determination::Known(Liveness::Stalled));
        // first_seen preserved so it STAYS stalled on later samples.
        assert_eq!(next.unwrap().first_seen, 1_000);
    }

    #[test]
    fn frozen_but_window_not_elapsed_is_undetermined() {
        let anchored = fp(&[("head", "a")]);
        let prev = Snapshot {
            fingerprint: anchored.as_str().to_string(),
            first_seen: 1_000,
            last_seen: 1_000,
        };
        let (verdict, next) = classify(
            Some(&prev),
            &known(anchored.clone()),
            1_000 + DEFAULT_WINDOW_SECS - 1, // one second short
            DEFAULT_WINDOW_SECS,
        );
        assert!(
            matches!(verdict, Determination::Undetermined(_)),
            "frozen but window-not-elapsed must be Undetermined, never Stalled"
        );
        // Keeps accruing against the original anchor.
        assert_eq!(next.unwrap().first_seen, 1_000);
    }

    // --- sample() end-to-end over the store (multi-invocation) ------------

    #[test]
    fn sample_two_frozen_samples_across_window_reach_stalled() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path();
        let frozen = || known(fp(&[("head", "a"), ("tx", "10")]));
        // Sample 1: first observation ⇒ Undetermined (anchors at t=0).
        let v1 = sample(store, "target-1", frozen(), 0, DEFAULT_WINDOW_SECS);
        assert!(matches!(v1, Determination::Undetermined(_)));
        // Sample 2 within the window ⇒ still Undetermined.
        let v2 = sample(
            store,
            "target-1",
            frozen(),
            DEFAULT_WINDOW_SECS - 1,
            DEFAULT_WINDOW_SECS,
        );
        assert!(matches!(v2, Determination::Undetermined(_)));
        // Sample 3 past the window, still frozen ⇒ Stalled.
        let v3 = sample(
            store,
            "target-1",
            frozen(),
            DEFAULT_WINDOW_SECS,
            DEFAULT_WINDOW_SECS,
        );
        assert_eq!(v3, Determination::Known(Liveness::Stalled));
    }

    #[test]
    fn sample_advancing_signal_stays_progressing_never_stalled() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path();
        // Anchor at a distinct baseline value so the first loop iteration is a
        // genuine advance (not an accidental freeze).
        let _ = sample(
            store,
            "t",
            known(fp(&[("tx", "0")])),
            0,
            DEFAULT_WINDOW_SECS,
        );
        // Advance the signal every sample, each spaced past the window: must
        // stay Progressing forever, NEVER Stalled.
        for i in 1..5 {
            let v = sample(
                store,
                "t",
                known(fp(&[("tx", &i.to_string())])),
                i * (DEFAULT_WINDOW_SECS + 10),
                DEFAULT_WINDOW_SECS,
            );
            assert_eq!(
                v,
                Determination::Known(Liveness::Progressing),
                "an advancing signal must never be reaped, sample {i}"
            );
        }
    }

    #[test]
    fn sample_undetermined_signal_does_not_reset_the_freeze_clock() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path();
        let frozen = || known(fp(&[("head", "a")]));
        // Anchor at t=0.
        let _ = sample(store, "t", frozen(), 0, DEFAULT_WINDOW_SECS);
        // A transient unreadable signal midway must NOT re-anchor.
        let mid = sample(
            store,
            "t",
            Determination::undetermined("transient git failure"),
            DEFAULT_WINDOW_SECS / 2,
            DEFAULT_WINDOW_SECS,
        );
        assert!(matches!(mid, Determination::Undetermined(_)));
        // Frozen again past the ORIGINAL window ⇒ Stalled (clock was not reset).
        let v = sample(
            store,
            "t",
            frozen(),
            DEFAULT_WINDOW_SECS,
            DEFAULT_WINDOW_SECS,
        );
        assert_eq!(
            v,
            Determination::Known(Liveness::Stalled),
            "a transient unreadable sample must not delay a genuine stall"
        );
    }

    #[test]
    fn sample_corrupt_prior_snapshot_is_undetermined_not_stalled() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path();
        let path = snapshot_path(store, "t");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"{ not json").unwrap();
        // Corrupt prior ⇒ read as no-prior ⇒ Undetermined (re-anchors), never a
        // reap off garbage.
        let v = sample(
            store,
            "t",
            known(fp(&[("head", "a")])),
            10_000,
            DEFAULT_WINDOW_SECS,
        );
        assert!(matches!(v, Determination::Undetermined(_)));
    }

    // --- signal readers (real git / filesystem) ---------------------------

    fn git(dir: &Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?} failed");
    }

    #[test]
    fn git_head_signal_is_known_and_advances_on_commit() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        git(d, &["init", "-q"]);
        std::fs::write(d.join("a.txt"), "1").unwrap();
        git(d, &["add", "."]);
        git(d, &["commit", "-qm", "one"]);
        let s1 = git_head_signal(d);
        let head1 = match &s1 {
            Determination::Known(v) => v.clone(),
            other => panic!("expected Known HEAD, got {other:?}"),
        };
        // A new commit advances HEAD ⇒ a different signal value.
        std::fs::write(d.join("a.txt"), "2").unwrap();
        git(d, &["commit", "-aqm", "two"]);
        match git_head_signal(d) {
            Determination::Known(v) => assert_ne!(v, head1, "HEAD must advance on a new commit"),
            other => panic!("expected Known HEAD, got {other:?}"),
        }
    }

    #[test]
    fn git_head_signal_undetermined_outside_a_repo() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            git_head_signal(dir.path()),
            Determination::Undetermined(_)
        ));
    }

    #[test]
    fn file_growth_signal_known_when_present_undetermined_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("t.jsonl");
        std::fs::write(&f, "line1\n").unwrap();
        let s1 = match file_growth_signal(&f) {
            Determination::Known(v) => v,
            other => panic!("expected Known, got {other:?}"),
        };
        // Growth changes the (size, mtime) value.
        std::fs::write(&f, "line1\nline2\n").unwrap();
        match file_growth_signal(&f) {
            Determination::Known(v) => assert_ne!(v, s1, "a grown file must yield a new signal"),
            other => panic!("expected Known, got {other:?}"),
        }
        assert!(matches!(
            file_growth_signal(&dir.path().join("missing.jsonl")),
            Determination::Undetermined(_)
        ));
    }

    #[test]
    fn transcript_signal_finds_session_and_is_undetermined_when_absent() {
        // Fixture: <projects>/<enc-cwd>/<session>.jsonl
        let dir = tempfile::tempdir().unwrap();
        let projects = dir.path();
        let proj = projects.join("-Users-me-repo");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join("sess-1.jsonl"), "{}\n").unwrap();
        assert!(matches!(
            transcript_signal_in(projects, "sess-1"),
            Determination::Known(_)
        ));
        // No such session ⇒ Undetermined (cannot judge, never "frozen").
        assert!(matches!(
            transcript_signal_in(projects, "nope"),
            Determination::Undetermined(_)
        ));
        // Absent projects dir ⇒ Undetermined.
        assert!(matches!(
            transcript_signal_in(&projects.join("does-not-exist"), "sess-1"),
            Determination::Undetermined(_)
        ));
        // Empty session id ⇒ Undetermined.
        assert!(matches!(
            transcript_signal_in(projects, ""),
            Determination::Undetermined(_)
        ));
    }
}
