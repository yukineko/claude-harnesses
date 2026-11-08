//! Step 0 of [`crate::gate::evaluate`]: the config states that are a refusal
//! to judge, checked before anything else. Each is undetermined, never "no
//! criteria" and never a mode choice, so each blocks (CLAUDE.md §3). None has a
//! give-up: retrying cannot fix them, the operator can (`propguard skip` /
//! PROPGUARD_DISABLE and the repeat-ledger waiver in
//! `harness_core::repeat::emit_stop_block` are the bounded exits).
//!
//! Split out of `gate.rs` (file-growth cap) when `config-untrusted` was added.

use std::path::Path;

use crate::config::Config;
use crate::gate::{config_invalid_reason, config_unreadable_reason};

/// `(reason, tag)` of the first refusal that applies, in this order:
///
/// * `config-unreadable` (step 0) — a config file that exists but could not
///   be read/parsed: every knob (including `done_criteria`) would be a
///   built-in default standing in for an operator config we could not see
///   (CA-propguard-03).
/// * `config-untrusted` (step 0a) — a project propguard.toml exists but the
///   root is untrusted: it is not honored, and that is a refusal to judge,
///   never "no criteria" (trust inheritance removed 2026-11-08).
///   `propguard trust` is the remedy.
/// * `config-invalid` (step 0b) — a value we read but cannot interpret
///   (unknown `mode`): not mapped to either mode.
pub(crate) fn check(cfg: &Config) -> Option<(String, &'static str)> {
    if let Some(why) = &cfg.load_error {
        return Some((config_unreadable_reason(why), "config-unreadable"));
    }
    if let Some(p) = &cfg.untrusted_project {
        return Some((config_untrusted_reason(p), "config-untrusted"));
    }
    if let Some(why) = &cfg.config_invalid {
        return Some((config_invalid_reason(why), "config-invalid"));
    }
    None
}

fn config_untrusted_reason(path: &Path) -> String {
    format!(
        "🚧 propguard: config-untrusted — REFUSING TO JUDGE. {} exists, but this project root is \
         not on the workspace-trust list, so its settings (done_criteria, checker_cmd, mode) are \
         not honored. That is a refusal, not a pass: built-in defaults would silently skip what \
         this project declared.\n\n\
         Remedy: run `propguard trust` in this root, or create the worktree through the session \
         init (scripts/session-worktree-init.py) or condukt, which register it at creation. \
         Trust is not inherited from the main working tree.\n\
         One stop only: `propguard skip --reason ...`.",
        path.display()
    )
}
