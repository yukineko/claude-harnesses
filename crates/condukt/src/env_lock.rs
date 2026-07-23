//! Shared lock for tests that mutate — or depend on — the process-global
//! `PATH` environment variable.
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
