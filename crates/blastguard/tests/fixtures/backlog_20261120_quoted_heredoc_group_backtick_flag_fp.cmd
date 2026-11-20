cd @WT@ && F=crates/condukt/tests/backlog_20261120_testscope_task_crate_regression.rs && {
cat <<'EOF'
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, dead_code)]
//! Observed 2026-11-20 in condukt task worktree
//! /Users/yuki/.condukt/worktrees/run-20261120-170138-92681-t8cad (task t8cad,
//! changed file crates/blastguard/src/detect/payload.rs): running an existing
//! integration test of the same crate to check that the fix leaves pinned
//! asserts unchanged (e.g. `cargo test -p blastguard --test
//! substitution_stdin_exec`) was refused by `condukt testscope` with
//! "inside a condukt task worktree only the task's own tests may run".
//!
//! User ruling 2026-11-20 (verbatim): 「証明ができないというのであれば、それを
//! 証明するためのテストを実施できないのはtestscopeの不具合」.
//!
//! Mechanism: crates/condukt/src/testscope.rs `check_task_scope` allows a
//! `--test <name>` only when it is named in the task's reproduction_tests or is
//! a tests/<name>.rs the worktree added or changed; every other `--test` of the
//! same crate is denied with TASK_SCOPE_REASON.
//!
//! Expected: inside a task worktree, a single named `--test <file>` of an
//! existing integration test of a crate the task changed is not denied
//! (regression check). Unchanged and NOT asserted here beyond the control: the
//! full-suite refusal (rule 1) and the same-tree rerun refusal (rule 3).
//!
//! CONFLICT (stated, not hidden): this contradicts the pinned assert
//! testscope_enforcement_20261114.rs `c_unrelated_existing_test_in_task_worktree_is_denied`
//! (`--test old_t`, pre-existing test of the same single crate). A fix that
//! makes this RED green must re-anchor that assert per the 2026-11-20 ruling.
//!
//! Controls (GREEN at HEAD): the full suite `cargo test -p foo` in the task
//! worktree is denied; an existing test of a crate the task did NOT change
//! (`cargo test -p bar --test bar_t`) is denied. The second control encodes
//! the "task's own crate" boundary of the relayed expectation.
//!
//! DOES NOT PROVE: how a fix determines "the task's crate" (touched_files vs
//! worktree diff vs Cargo metadata); `--lib`/`--doc` selectors; tdd/`bash -c`
//! routes; rerun-ledger interaction.
//!
//! Helpers are copied from backlog_20261115_testscope_cd_prefix_rule_c.rs.
//!
//! RED written by an independent test author, not the implementer.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use tempfile::TempDir;
EOF
sed -n '/^\/\/ ── shared fixture/,/^fn repo() -> TempDir {/p' crates/condukt/tests/backlog_20261115_testscope_cd_prefix_rule_c.rs | sed '$d'
sed -n '/^\/\/\/ `condukt testscope` with a PreToolUse/,/^\/\/\/ Find `<run>.json`/p' crates/condukt/tests/backlog_20261115_testscope_cd_prefix_rule_c.rs | sed '$d'
sed -n '/^\/\/\/ `state init --run <run>`/,/^struct WtFx {/p' crates/condukt/tests/backlog_20261115_testscope_cd_prefix_rule_c.rs | sed '$d'
cat <<'EOF'
const SESSION: &str = "sess-r";
EOF
} > $F && grep -n "^fn \|^struct\|^const\|^impl" $F