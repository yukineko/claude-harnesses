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

use serde::Serialize;

use crate::{Determination, Verdict, ENV_MARKETPLACE, EXIT_CLEAN, EXIT_UNDETERMINED};
use record::{Decisions, FeatureKind};

/// How to recover from an unreadable / invalid record (printed with exit 2).
const BROKEN_RECORD_HINT: &str = "fix: correct or delete sitecraft/decisions.toml (it must match the schema documented in `sitecraft::decide::record`), then run `sitecraft decide next` and answer each question with `sitecraft decide record --area <area> --question <id> --answer <value>`";

/// Plugin names listed in the `marketplace.json` at `path` (its
/// `plugins[].name`).
///
/// Contract: unreadable file, invalid JSON, no `plugins` array, or an entry
/// without a string `name` → `Undetermined`; otherwise `Known` (possibly
/// empty only if the array is genuinely empty).
pub fn marketplace_plugins(path: &Path) -> Determination<BTreeSet<String>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            return Determination::undetermined(format!(
                "cannot read marketplace {}: {e}",
                path.display()
            ))
        }
    };
    let v: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            return Determination::undetermined(format!(
                "marketplace {} is not valid JSON: {e}",
                path.display()
            ))
        }
    };
    let Some(plugins) = v.get("plugins").and_then(serde_json::Value::as_array) else {
        return Determination::undetermined(format!(
            "marketplace {} has no `plugins` array",
            path.display()
        ));
    };
    let mut names = BTreeSet::new();
    for (i, p) in plugins.iter().enumerate() {
        match p.get("name").and_then(serde_json::Value::as_str) {
            Some(n) if !n.trim().is_empty() => {
                names.insert(n.to_string());
            }
            _ => {
                return Determination::undetermined(format!(
                    "marketplace {}: plugins[{i}] has no string `name`",
                    path.display()
                ))
            }
        }
    }
    Determination::known(names)
}

fn undetermined(what: &str, why: &str, hint: &str) -> i32 {
    eprintln!("sitecraft {what}: UNDETERMINED - {why}");
    eprintln!("{hint}");
    EXIT_UNDETERMINED
}

/// Load the record for a mutating command: absent → empty record.
fn load_for_update(root: &Path) -> Result<Decisions, String> {
    match record::load(root) {
        Determination::Known(Some(d)) => Ok(d),
        Determination::Known(None) => Ok(Decisions::empty()),
        Determination::Undetermined(why) => Err(why.as_str().to_string()),
    }
}

/// Print `v` as one JSON line; an unencodable value is exit 2.
fn print_json<T: Serialize>(what: &str, v: &T) -> i32 {
    match serde_json::to_string(v) {
        Ok(s) => {
            println!("{s}");
            EXIT_CLEAN
        }
        Err(e) => undetermined(
            what,
            &format!("cannot encode the result: {e}"),
            "fix: this is an internal error; report it",
        ),
    }
}

/// `decide next`: print the next question as [`record::Next`] JSON on stdout.
/// Exit 0 when a question or `done` was printed. An absent record means
/// nothing is decided yet (the first question is printed, exit 0); an
/// unreadable or invalid record, or an unavailable catalog, is exit 2.
pub fn cmd_next(root: &Path) -> i32 {
    let d = match record::load(root) {
        Determination::Known(Some(d)) => d,
        Determination::Known(None) => Decisions::empty(),
        Determination::Undetermined(why) => {
            return undetermined("decide next", why.as_str(), BROKEN_RECORD_HINT)
        }
    };
    match record::next(&d) {
        Determination::Known(n) => print_json("decide next", &n),
        Determination::Undetermined(why) => {
            undetermined("decide next", why.as_str(), BROKEN_RECORD_HINT)
        }
    }
}

/// `decide record --area A --question Q --answer V`: load (absent = empty),
/// [`record::record_answer`], save. Exit 0 on success, 2 otherwise.
pub fn cmd_record(root: &Path, area: &str, question: &str, answer: &str) -> i32 {
    let what = "decide record";
    let mut d = match load_for_update(root) {
        Ok(d) => d,
        Err(e) => return undetermined(what, &e, BROKEN_RECORD_HINT),
    };
    if let Err(e) = record::record_answer(&mut d, area, question, answer) {
        return undetermined(
            what,
            &e,
            "fix: run `sitecraft decide next` to get a valid area / question id and its choices, then retry `sitecraft decide record` (the record was not changed)",
        );
    }
    match record::save(root, &d) {
        Ok(()) => {
            eprintln!("sitecraft {what}: recorded {area}/{question} = {answer}");
            EXIT_CLEAN
        }
        Err(e) => undetermined(
            what,
            &e,
            "fix: make sitecraft/ writable, then retry `sitecraft decide record`",
        ),
    }
}

/// `decide check`: [`record::check_root`] mapped through
/// [`crate::exit_code`]; exit 1 / 2 diagnostics name the `decide` command
/// that fixes them.
pub fn cmd_check(root: &Path) -> i32 {
    let v = record::check_root(root);
    match &v {
        Verdict::Clean(_) => {
            eprintln!("sitecraft decide check: clean - every decision is recorded")
        }
        Verdict::Violation(r) => {
            eprintln!("sitecraft decide check: VIOLATION - pending decisions:");
            for item in r.as_str().split("; ") {
                eprintln!("  - {item}");
            }
            eprintln!(
                "fix: ask the user and record each answer (`sitecraft decide next` prints the next question; `irrelevant` is a valid, recorded relevance answer)"
            );
        }
        Verdict::Undetermined(u) => {
            let hint = if record::path_for(root).exists() {
                BROKEN_RECORD_HINT
            } else {
                "fix: start the decision record with `sitecraft decide next`, then answer each question with `sitecraft decide record --area <area> --question <id> --answer <value>`"
            };
            eprintln!("sitecraft decide check: UNDETERMINED - {}", u.as_str());
            eprintln!("{hint}");
        }
    }
    crate::exit_code(&v)
}

/// `decide add-question --id I --text T [--choice C]...`: load (absent =
/// empty), [`record::add_question`], save. Exit 0 / 2.
pub fn cmd_add_question(root: &Path, id: &str, text: &str, choices: Vec<String>) -> i32 {
    let what = "decide add-question";
    let mut d = match load_for_update(root) {
        Ok(d) => d,
        Err(e) => return undetermined(what, &e, BROKEN_RECORD_HINT),
    };
    if let Err(e) = record::add_question(&mut d, id, text, choices) {
        return undetermined(
            what,
            &e,
            "fix: use a new, non-empty --id and non-empty --text / --choice values (the record was not changed)",
        );
    }
    match record::save(root, &d) {
        Ok(()) => {
            eprintln!(
                "sitecraft {what}: added `{id}`; answer it with `sitecraft decide record --area {} --question {id} --answer <value>`",
                record::DOMAIN_AREA
            );
            EXIT_CLEAN
        }
        Err(e) => undetermined(
            what,
            &e,
            "fix: make sitecraft/ writable, then retry `sitecraft decide add-question`",
        ),
    }
}

/// JSON printed by `decide classify`.
#[derive(Serialize)]
struct Classified<'a> {
    feature: &'a str,
    kind: &'a str,
    harness: Option<&'a str>,
    existing: Option<bool>,
}

/// `decide classify --feature F --kind simple|dedicated-harness [--harness P]`:
/// record the classification and print JSON
/// `{"feature":F,"kind":…,"harness":P|null,"existing":bool|null}` where
/// `existing` says whether a plugin named `harness` (default `F`) is in the
/// marketplace at `$SITECRAFT_MARKETPLACE` (only for `dedicated-harness`).
/// Unknown kind, `--harness` with `simple`, unset/unreadable marketplace for a
/// dedicated harness, or record failure → exit 2 (nothing recorded).
pub fn cmd_classify(root: &Path, feature: &str, kind: &str, harness: Option<String>) -> i32 {
    let what = "decide classify";
    let k = match kind {
        "simple" => FeatureKind::Simple,
        "dedicated-harness" => FeatureKind::DedicatedHarness,
        other => {
            return undetermined(
                what,
                &format!("unknown --kind {other:?}"),
                "fix: rerun `sitecraft decide classify --feature <name> --kind simple|dedicated-harness [--harness <plugin>]`",
            )
        }
    };
    let (harness, existing) = match k {
        FeatureKind::Simple => {
            if harness.is_some() {
                return undetermined(
                    what,
                    "--harness only applies to --kind dedicated-harness",
                    "fix: drop --harness, or use `--kind dedicated-harness`",
                );
            }
            (None, None)
        }
        FeatureKind::DedicatedHarness => {
            let name = match harness {
                Some(h) => h,
                None => feature.to_string(),
            };
            let mp = match std::env::var_os(ENV_MARKETPLACE) {
                Some(p) if !p.is_empty() => p,
                _ => {
                    return undetermined(
                        what,
                        &format!("{ENV_MARKETPLACE} is not set, so whether `{name}` already exists cannot be determined"),
                        &format!("fix: set {ENV_MARKETPLACE}=<path to marketplace.json> and rerun `sitecraft decide classify`"),
                    )
                }
            };
            match marketplace_plugins(Path::new(&mp)) {
                Determination::Known(names) => {
                    let exists = names.contains(&name);
                    (Some(name), Some(exists))
                }
                Determination::Undetermined(why) => {
                    return undetermined(
                        what,
                        why.as_str(),
                        &format!("fix: point {ENV_MARKETPLACE} at a readable marketplace.json with a `plugins` array, then rerun `sitecraft decide classify`"),
                    )
                }
            }
        }
    };
    let mut d = match load_for_update(root) {
        Ok(d) => d,
        Err(e) => return undetermined(what, &e, BROKEN_RECORD_HINT),
    };
    if let Err(e) = record::classify(&mut d, feature, k, harness.clone()) {
        return undetermined(
            what,
            &e,
            "fix: rerun `sitecraft decide classify` with a non-empty --feature",
        );
    }
    if let Err(e) = record::save(root, &d) {
        return undetermined(
            what,
            &e,
            "fix: make sitecraft/ writable, then retry `sitecraft decide classify`",
        );
    }
    print_json(
        what,
        &Classified {
            feature,
            kind,
            harness: harness.as_deref(),
            existing,
        },
    )
}
