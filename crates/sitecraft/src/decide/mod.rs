//! `sitecraft decide …` — the decision record and its CLI.
//!
//! Every `cmd_*` function is the full body of one subcommand: it prints its
//! output (JSON on stdout where noted, diagnostics with the fixing `decide`
//! command on stderr) and returns the exit code (0 clean/success,
//! 1 violation, 2 undetermined / invalid input / unreadable record).

pub mod catalog;
pub mod record;

use std::collections::BTreeSet;
use std::path::Path;

use crate::Determination;

/// Plugin names listed in the `marketplace.json` at `path` (its
/// `plugins[].name`).
///
/// Contract: unreadable file, invalid JSON, or no `plugins` array →
/// `Undetermined`; otherwise `Known` (possibly empty only if the array is
/// genuinely empty).
pub fn marketplace_plugins(path: &Path) -> Determination<BTreeSet<String>> {
    Determination::undetermined(format!(
        "sitecraft: reading marketplace {} is not implemented yet",
        path.display()
    ))
}

/// `decide next`: print the next question as [`record::Next`] JSON on stdout.
/// Exit 0 when a question or `done` was printed. An absent record means
/// nothing is decided yet (the first question is printed, exit 0); an
/// unreadable or invalid record, or an unavailable catalog, is exit 2.
pub fn cmd_next(root: &Path) -> i32 {
    stub("decide next", root)
}

/// `decide record --area A --question Q --answer V`: load (absent = empty),
/// [`record::record_answer`], save. Exit 0 on success, 2 otherwise.
pub fn cmd_record(root: &Path, area: &str, question: &str, answer: &str) -> i32 {
    let _ = (area, question, answer);
    stub("decide record", root)
}

/// `decide check`: [`record::check_root`] mapped through
/// [`crate::exit_code`]; exit 1 / 2 diagnostics name the `decide` command
/// that fixes them.
pub fn cmd_check(root: &Path) -> i32 {
    stub("decide check", root)
}

/// `decide add-question --id I --text T [--choice C]...`: load (absent =
/// empty), [`record::add_question`], save. Exit 0 / 2.
pub fn cmd_add_question(root: &Path, id: &str, text: &str, choices: Vec<String>) -> i32 {
    let _ = (id, text, choices);
    stub("decide add-question", root)
}

/// `decide classify --feature F --kind simple|dedicated-harness [--harness P]`:
/// record the classification and print JSON
/// `{"feature":F,"kind":…,"harness":P|null,"existing":bool|null}` where
/// `existing` says whether a plugin named `harness` (default `F`) is in the
/// marketplace at `$SITECRAFT_MARKETPLACE` (only for `dedicated-harness`).
/// Unknown kind, unset/unreadable marketplace for a dedicated harness, or
/// record failure → exit 2.
pub fn cmd_classify(root: &Path, feature: &str, kind: &str, harness: Option<String>) -> i32 {
    let _ = (feature, kind, harness);
    stub("decide classify", root)
}

fn stub(what: &str, root: &Path) -> i32 {
    eprintln!(
        "sitecraft {what}: UNDETERMINED - not implemented yet (root {})",
        root.display()
    );
    crate::EXIT_UNDETERMINED
}
