//! Shared lock for the process-global `PATH` environment variable.
//!
//! **Current state (backlog 7c798579): nothing takes the write side.** No
//! condukt code, tests included, mutates the process `PATH` any more — a test
//! that needs a custom PATH passes it to its own child via `Command::env`
//! through a seam (`oracle::spawn_oracle_with_env_path`,
//! `lock::RunLock::acquire_at_with_env_path`,
//! `calibrated_confidence_via_with_env_path` in main.rs), and
//! `oracle::tests::condukt_never_mutates_process_path` fails on any new
//! `set_var("PATH"` / `remove_var("PATH"` in this crate. The reason: most
//! spawning tests never took the read side, so a write-side swap still raced
//! them (ENOENT on a bare-name spawn, ~1/16 runs under concurrent load). The
//! remaining read guards (`worktree::run_git_bounded_with`, editgate's
//! `cargo check`) are therefore always uncontended; they are kept, not
//! removed, so this static keeps its users. The history below explains why
//! the lock was introduced.
//!
//! `cargo test` runs test functions on threads within one process, and env
//! vars are process-global — two tests touching `PATH` concurrently race, and
//! the race is nondeterministic (rerunning the same failing test twice can
//! produce different symptoms: "failed to spawn X" one time, "X exited
//! non-zero" the next). Before this module existed, `oracle.rs` had its own
//! module-local `ORACLE_PATH_ENV_LOCK` that only serialized oracle.rs's own
//! PATH-mutating tests against each other. That missed two other classes of
//! test:
//!
//! - `main.rs`'s `flag_supplied_but_probe_unusable_falls_back` also mutates
//!   `PATH` (to make `fugu-router` unresolvable), without taking any lock.
//! - Tests that never mutate `PATH` themselves but spawn a real `git` and
//!   depend on it being resolvable — e.g.
//!   `repo_commit::tests::first_unstaged_modification_is_not_misread_as_staged`,
//!   via `worktree::git`. If one of these runs while a mutator has PATH
//!   pointed at a directory that doesn't contain `git` (oracle.rs's
//!   `check_oracle_with_no_tdd_on_path`, or main.rs's filtered-PATH test),
//!   the spawn transiently fails for a reason that has nothing to do with
//!   what the test is actually checking. Reproduced empirically: a targeted
//!   loop of 15 full-suite runs under 16 test threads hit 2 distinct real
//!   failures this way (`oracle::tests::
//!   oracle_check_oracle_exit_zero_still_trusts_valid_verdict` and
//!   `repo_commit::tests::first_unstaged_modification_is_not_misread_as_staged`)
//!   — 3/15 runs flaked before this module existed.
//!
//! So this is an `RwLock`, not a `Mutex`: PATH *mutators* take the write
//! lock (exclusive — no other mutator or consumer may observe PATH mid-
//! mutation), and PATH *consumers* (real subprocess spawns that rely on PATH
//! resolution, e.g. `worktree::run_git_bounded_with`) take the read lock
//! (shared — many concurrent git spawns are fine, they just must not
//! interleave with a mutation window).
//!
//! Not gated to test-only compilation: the only mutators/consumers are test
//! code, but a bare cfg(test) attribute on a non-module item (rather than a
//! braced `mod tests` body) is indistinguishable, to this repo's
//! test-weakening scanner, from a cfg(test) module it can't find the body of
//! — it correctly refuses to guess and blocks as undetermined. An
//! always-compiled, always-uncontended `RwLock<()>` costs nothing in
//! production (nothing outside tests ever touches it), so leaving it
//! ungated sidesteps the ambiguity entirely rather than asking the scanner
//! to special-case it.
pub(crate) static PATH_ENV_LOCK: std::sync::RwLock<()> = std::sync::RwLock::new(());

/// The same problem for `$HOME`, which several stores key off
/// (`harness_core::config::home()` → `~/.harness/trust.toml`,
/// `~/.overwatch/…`). It used to be a module-local `HOME_LOCK` inside
/// `worktree::worktree_remove_tests`, which serialized only that module's
/// merge tests against each other. `worktree::worktree_trust_tests` swaps
/// `$HOME` too (to keep workspace-trust registration off the real
/// `~/.harness/trust.toml`), and a module-local lock cannot exclude a swap
/// happening in a *different* module — the exact shape of miss that
/// `PATH_ENV_LOCK`'s doc comment describes. Hoisted here so every `$HOME`
/// mutator in this crate takes ONE lock.
///
/// A plain `Mutex` underneath, not an `RwLock`: unlike PATH there is no set of
/// pure *consumers* that would benefit from shared access — every user of this
/// lock mutates `$HOME`.
///
/// Acquiring it also pins [`toolchain_env_snapshot`] BEFORE the guard is handed
/// out (see [`HomeEnvLock::lock`]). That is what makes the snapshot pristine:
/// every `$HOME` mutation in this crate happens under a guard, and no guard
/// exists before the snapshot does.
///
/// Ungated (no cfg(test) attribute) for the same reason as `PATH_ENV_LOCK`: an
/// uncontended static lock costs nothing outside tests, and a bare cfg(test) on
/// a non-module item is the shape this repo's test-weakening scanner correctly
/// refuses to guess about. Every user of the lock itself is a test, so the
/// non-test build sees it as dead. The allow below is narrow and suppresses
/// only that: it is not a lint waiver on any judging code path.
#[allow(dead_code)]
pub(crate) static HOME_ENV_LOCK: HomeEnvLock = HomeEnvLock(std::sync::Mutex::new(()));

/// See [`HOME_ENV_LOCK`]. A thin wrapper so that acquisition can pin the
/// toolchain-env snapshot first; `lock()` keeps `Mutex::lock`'s signature so
/// every existing `HOME_ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner)`
/// call site is unchanged.
pub(crate) struct HomeEnvLock(std::sync::Mutex<()>);

impl HomeEnvLock {
    #[allow(dead_code)]
    pub(crate) fn lock(&self) -> std::sync::LockResult<std::sync::MutexGuard<'_, ()>> {
        // Pin first. If a guard is currently held by someone else (HOME
        // possibly swapped), that holder's own `lock()` already pinned the
        // snapshot, so this is a no-op read of the pinned value — it can never
        // be the call that observes a swapped HOME.
        let _ = toolchain_env_snapshot();
        self.0.lock()
    }
}

/// The env vars that decide which Rust toolchain a spawned `cargo` (a rustup
/// proxy on a rustup-managed machine) runs with, and where it reads its config.
pub(crate) const TOOLCHAIN_ENV_KEYS: [&str; 4] =
    ["HOME", "RUSTUP_HOME", "CARGO_HOME", "RUSTUP_TOOLCHAIN"];

/// The process's toolchain env ([`TOOLCHAIN_ENV_KEYS`]), read ONCE — at the
/// first of: the first call to this function, or the first acquisition of
/// [`HOME_ENV_LOCK`]. `None` means the var was unset at that point.
///
/// Production consumer: `editgate::check_edit` pins these on its `cargo check`
/// child instead of letting the child inherit whatever the process env holds at
/// spawn time. In production (the one-shot `condukt editgate` hook process)
/// nothing ever mutates them, so the snapshot equals what the child would have
/// inherited anyway. In the test binary, sibling tests swap `$HOME` under
/// `HOME_ENV_LOCK`; the snapshot is pinned before any such swap can exist, so a
/// `cargo check` spawned mid-swap still resolves the real toolchain
/// (backlog f7a0cadc).
pub(crate) fn toolchain_env_snapshot() -> &'static [(&'static str, Option<std::ffi::OsString>)] {
    static SNAPSHOT: std::sync::OnceLock<Vec<(&'static str, Option<std::ffi::OsString>)>> =
        std::sync::OnceLock::new();
    SNAPSHOT.get_or_init(|| {
        TOOLCHAIN_ENV_KEYS
            .iter()
            .map(|k| (*k, std::env::var_os(k)))
            .collect()
    })
}
