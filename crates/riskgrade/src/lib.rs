// テスト内の unwrap/expect は意図的な assert であって fail-open ではないので許可する。
// production 側は workspace の [workspace.lints.clippy] で deny のまま。
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
//! riskgrade: graded risk / reversibility of a planned action ([`classify`])
//! and diff-level semantic risk signals ([`diffrisk`]). Pure, deterministic,
//! no I/O.
//!
//! Moved out of blastguard on 2026-11-17 (user ruling: blastguard holds the
//! command-safety verdict only). The tier-2 "local destruction" rule still
//! reuses blastguard's detector ([`blastguard::detect::detect`]), so the
//! classifier's High/irreversible set cannot drift from the hook's deny set.
//! condukt is the consumer (force-gate, diff-risk record, review brief).

pub mod classify;
pub mod diffrisk;
