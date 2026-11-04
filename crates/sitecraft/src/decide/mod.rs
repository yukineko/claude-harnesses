//! `sitecraft decide …` — the decision record and its CLI.
//!
//! Every `cmd_*` function is the full body of one subcommand: it prints its
//! output (JSON or `changed:` lines on stdout where noted, diagnostics with
//! the fixing `decide` command on stderr) and returns the exit code
//! (0 clean/success, 1 violation, 2 undetermined / invalid input / unreadable
//! record).

pub mod capability;
pub mod catalog;
pub mod record;
pub mod rules;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Serialize;

use crate::{
    Determination, Verdict, ENV_MARKETPLACE, EXIT_CLEAN, EXIT_UNDETERMINED, EXIT_VIOLATION,
};
use record::{Decisions, FeatureKind, Fit};

/// How to recover from an unreadable / invalid record (printed with exit 2).
const BROKEN_RECORD_HINT: &str = "fix: correct or delete sitecraft/decisions.toml (it must match the schema documented in `sitecraft::decide::record`), then run `sitecraft decide next` and answer each question with `sitecraft decide record --area <area> --question <id> --answer <value>`";

/// Plugins listed in the `marketplace.json` at `path`: `plugins[].name` →
/// its `description` (`None` when the entry has no `description` key).
///
/// Contract: unreadable file, invalid JSON, no `plugins` array, an entry
/// without a non-blank string `name`, or a `description` that is present but
/// not a string → `Undetermined`; otherwise `Known` (possibly empty only if
/// the array is genuinely empty).
pub fn marketplace_entries(path: &Path) -> Determination<BTreeMap<String, Option<String>>> {
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
    let mut out = BTreeMap::new();
    for (i, p) in plugins.iter().enumerate() {
        let name = match p.get("name").and_then(serde_json::Value::as_str) {
            Some(n) if !n.trim().is_empty() => n.to_string(),
            _ => {
                return Determination::undetermined(format!(
                    "marketplace {}: plugins[{i}] has no string `name`",
                    path.display()
                ))
            }
        };
        let description = match p.get("description") {
            None => None,
            Some(serde_json::Value::String(s)) => Some(s.clone()),
            Some(_) => {
                return Determination::undetermined(format!(
                    "marketplace {}: plugins[{i}] (`{name}`) has a non-string `description`",
                    path.display()
                ))
            }
        };
        out.insert(name, description);
    }
    Determination::known(out)
}

/// Plugin names listed in the `marketplace.json` at `path` (its
/// `plugins[].name`). Same contract as [`marketplace_entries`].
pub fn marketplace_plugins(path: &Path) -> Determination<BTreeSet<String>> {
    marketplace_entries(path).map(|m| m.into_keys().collect())
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

/// Append the history rows for `before` → `after` (D8), save, and print one
/// `changed: <old> -> <new>` line on stdout per changed value (pruned values
/// are reported on stderr). Returns `Ok(())` after a successful save; a clock
/// or save failure is `Err(exit code)` with the diagnostic printed (nothing
/// is printed as changed then).
fn save_with_history(
    root: &Path,
    what: &str,
    before: &Decisions,
    mut after: Decisions,
) -> Result<(), i32> {
    let mut rows = record::diff_history(before, &after, "");
    if !rows.is_empty() {
        let at = match record::now_utc_rfc3339() {
            Ok(t) => t,
            Err(e) => {
                return Err(undetermined(
                    what,
                    &e,
                    "fix: correct the system clock, then retry (the record was not changed)",
                ))
            }
        };
        for r in &mut rows {
            r.at.clone_from(&at);
        }
        after.history.extend(rows.iter().cloned());
    }
    if let Err(e) = record::save(root, &after) {
        return Err(undetermined(
            what,
            &e,
            &format!("fix: make sitecraft/ writable, then retry `sitecraft {what}`"),
        ));
    }
    for r in &rows {
        if r.pruned {
            eprintln!(
                "sitecraft {what}: pruned {}/{} (was {:?}; no longer asked)",
                r.area, r.question, r.old
            );
        } else {
            println!("changed: {} -> {}", r.old, r.new);
        }
    }
    Ok(())
}

/// `decide next`: print the next question as [`record::Next`] JSON on stdout.
/// Exit 0 when a question or `done` was printed. An absent record means
/// nothing is decided yet (the first question is printed, exit 0); an
/// unreadable or invalid record, or an unavailable catalog, is exit 2. The
/// purpose is never asked here (D1).
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

/// `decide record --area A --question Q --answer V [--answer V2]... [--note T]`:
/// load (absent = empty), [`record::record_answer`], history, save. Exit 0 on
/// success (with `changed:` lines for changed values), 2 otherwise (record
/// unchanged).
pub fn cmd_record(
    root: &Path,
    area: &str,
    question: &str,
    answers: &[String],
    note: Option<&str>,
) -> i32 {
    let what = "decide record";
    let before = match load_for_update(root) {
        Ok(d) => d,
        Err(e) => return undetermined(what, &e, BROKEN_RECORD_HINT),
    };
    let mut d = before.clone();
    if let Err(e) = record::record_answer(&mut d, area, question, answers, note) {
        return undetermined(
            what,
            &e,
            "fix: run `sitecraft decide next` to get a valid area / question id, its choices and whether it is multi-select, then retry `sitecraft decide record` (the record was not changed)",
        );
    }
    match save_with_history(root, what, &before, d) {
        Ok(()) => {
            eprintln!(
                "sitecraft {what}: recorded {area}/{question} = {}",
                answers.join(", ")
            );
            EXIT_CLEAN
        }
        Err(code) => code,
    }
}

/// `decide purpose --text T` (D1): record the trimmed purpose. A blank text
/// is exit 1 with the record untouched (not even read); an unreadable record
/// or a save failure is exit 2; success is exit 0 (with a `changed:` line
/// when a different purpose was recorded before).
pub fn cmd_purpose(root: &Path, text: &str) -> i32 {
    let what = "decide purpose";
    if text.trim().is_empty() {
        eprintln!("sitecraft {what}: VIOLATION - --text is blank (the record was not changed)");
        eprintln!(
            "fix: rerun `sitecraft decide purpose --text \"<要約>\"` with the purpose summary agreed with the user"
        );
        return EXIT_VIOLATION;
    }
    let before = match load_for_update(root) {
        Ok(d) => d,
        Err(e) => return undetermined(what, &e, BROKEN_RECORD_HINT),
    };
    let mut d = before.clone();
    if let Err(e) = record::set_purpose(&mut d, text) {
        return undetermined(what, &e, "fix: pass a non-blank --text");
    }
    match save_with_history(root, what, &before, d) {
        Ok(()) => {
            eprintln!("sitecraft {what}: recorded the purpose");
            EXIT_CLEAN
        }
        Err(code) => code,
    }
}

/// `decide check`: [`record::check_root_report`] mapped through
/// [`crate::exit_code`]; exit 1 / 2 diagnostics name the `decide` command
/// that fixes them. Warning rules (W1) are printed as `warning: <id>: …` on
/// stderr and never change the exit code (U4). Each run appends a `decide`
/// line to the check history ([`crate::history::recorded`]); a line that
/// cannot be appended (no state dir included) makes it exit 2.
pub fn cmd_check(root: &Path) -> i32 {
    let state = crate::state_dir();
    let state = match &state {
        Determination::Known(s) => Ok(s.as_path()),
        Determination::Undetermined(why) => Err(why.as_str()),
    };
    let mut out = crate::history::Output::printing();
    let v = crate::history::recorded(
        state,
        root,
        "decide",
        "sitecraft decide check",
        &mut out,
        |o| {
            let report = record::check_root_report(root);
            print_check(root, &report.verdict, &report.warnings, o);
            report.verdict
        },
    );
    crate::exit_code(&v)
}

/// The Stop gate's `decide check`: [`record::check_root`], with the same
/// report lines as [`cmd_check`] written into `out` (for the history
/// fingerprint).
pub fn gate_check(root: &Path, out: &mut crate::history::Output) -> Verdict {
    let v = record::check_root(root);
    print_check(root, &v, &[], out);
    v
}

/// The `decide check` report lines of `v` (warnings first) into `out`.
fn print_check(root: &Path, v: &Verdict, warnings: &[String], out: &mut crate::history::Output) {
    for w in warnings {
        out.err(format!("warning: {w}"));
    }
    match v {
        Verdict::Clean(_) => {
            out.err(
                "sitecraft decide check: clean - the purpose and every decision are recorded and no consistency rule is violated",
            );
        }
        Verdict::Violation(r) => {
            out.err(
                "sitecraft decide check: VIOLATION - pending decisions / violated consistency rules:",
            );
            for item in r.as_str().split("; ") {
                out.err(format!("  - {item}"));
            }
            out.err(
                "fix: ask the user and record each answer (`sitecraft decide next` prints the next question; `irrelevant` is a valid, recorded relevance answer); for a violated rule, ask the user which of the involved answers to change and re-record it",
            );
        }
        Verdict::Undetermined(u) => {
            let hint = if record::path_for(root).exists() {
                BROKEN_RECORD_HINT
            } else {
                "fix: start the decision record with `sitecraft decide purpose --text \"<要約>\"` and `sitecraft decide next`, then answer each question with `sitecraft decide record --area <area> --question <id> --answer <value>`"
            };
            out.err(format!(
                "sitecraft decide check: UNDETERMINED - {}",
                u.as_str()
            ));
            out.err(hint);
        }
    }
}

/// `decide capability --name N --relevance R [--how T] [--done-when T]
/// [--verify-kind K] [per-kind args]` (rulings D2): build the
/// `[capabilities.N]` table ([`capability::table_from_args`]), record it,
/// append history for a changed table, save. Exit 0 on success (with a
/// `changed:` line when the table changed), 2 otherwise (invalid name,
/// relevance, kind or kind arguments, missing `--how` / `--done-when` /
/// `--verify-kind` for a relevant capability, unreadable record, save
/// failure; the record is unchanged).
pub fn cmd_capability(root: &Path, name: &str, args: &capability::CapabilityArgs) -> i32 {
    let what = "decide capability";
    let table = match capability::table_from_args(name, args) {
        Ok(t) => t,
        Err(e) => {
            return undetermined(
                what,
                &e,
                "fix: rerun `sitecraft decide capability --name <a-z0-9-> --relevance relevant|irrelevant --how <text> --done-when <text> --verify-kind <kind>` with the kind's arguments: builtin:blender-headless --script <rel> --glb <rel> --render <rel> [--input <glob>]...; builtin:threejs-render --page </path> [--selector <css>] [--input <glob>]...; command --argv <program> [--argv <arg>]... --input <glob> [--input <glob>]... (the record was not changed)",
            )
        }
    };
    let before = match load_for_update(root) {
        Ok(d) => d,
        Err(e) => return undetermined(what, &e, BROKEN_RECORD_HINT),
    };
    let mut d = before.clone();
    if let Err(e) = record::record_capability(&mut d, name, table) {
        return undetermined(
            what,
            &e,
            "fix: correct the arguments (the record was not changed)",
        );
    }
    match save_with_history(root, what, &before, d) {
        Ok(()) => {
            eprintln!("sitecraft {what}: recorded capability `{name}`");
            if args.verify_kind.as_deref() == Some(capability::KIND_COMMAND) {
                eprintln!(
                    "sitecraft {what}: a `command` capability runs only after a human reviews its argv and runs `sitecraft trust` (the LLM does not run it)"
                );
            }
            EXIT_CLEAN
        }
        Err(code) => code,
    }
}

/// `decide add-question --id I --text T [--choice C]... [--multi]`: load
/// (absent = empty), [`record::add_question`], save. Exit 0 / 2.
pub fn cmd_add_question(
    root: &Path,
    id: &str,
    text: &str,
    choices: Vec<String>,
    multi: bool,
) -> i32 {
    let what = "decide add-question";
    let mut d = match load_for_update(root) {
        Ok(d) => d,
        Err(e) => return undetermined(what, &e, BROKEN_RECORD_HINT),
    };
    if let Err(e) = record::add_question(&mut d, id, text, choices, multi) {
        return undetermined(
            what,
            &e,
            "fix: use a new, non-empty --id and non-empty --text / --choice values (the record was not changed)",
        );
    }
    match record::save(root, &d) {
        Ok(()) => {
            let more = if multi { " [--answer <value>]..." } else { "" };
            eprintln!(
                "sitecraft {what}: added `{id}`; answer it with `sitecraft decide record --area {} --question {id} --answer <value>{more}`",
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
    description: Option<&'a str>,
    reuse: &'static str,
}

/// `reuse` of the classify JSON (D9).
fn reuse_word(existing: Option<bool>, fit: Option<Fit>) -> &'static str {
    match (existing, fit) {
        (Some(true), None) => "unconfirmed",
        (Some(true), Some(Fit::Yes)) => "confirmed",
        (Some(true), Some(Fit::No)) => "rejected",
        _ => "not-applicable",
    }
}

/// `decide classify --feature F --kind simple|dedicated-harness [--harness P]
/// [--confirm-fit yes|no]`: record the classification and print JSON
/// `{"feature":F,"kind":…,"harness":P|null,"existing":bool|null,
/// "description":…|null,"reuse":…}` where `existing` says whether a plugin
/// named `harness` (default `F`) is in the marketplace at
/// `$SITECRAFT_MARKETPLACE` (only for `dedicated-harness`), `description` is
/// that marketplace entry's description (null when not existing or the entry
/// has none) and `reuse` is `unconfirmed` (existing, no fit recorded) /
/// `confirmed` (fit yes) / `rejected` (fit no) / `not-applicable` (not
/// existing, or simple) (rulings D9).
///
/// `--confirm-fit` records the **user's** answer to whether the existing
/// plugin fits the purpose; it is accepted only when `existing` is true.
/// Unknown kind, `--harness` with `simple`, `--confirm-fit` that is not
/// `yes`/`no` or given when nothing exists to reuse, unset/unreadable
/// marketplace for a dedicated harness, or record failure → exit 2 (nothing
/// recorded).
pub fn cmd_classify(
    root: &Path,
    feature: &str,
    kind: &str,
    harness: Option<String>,
    confirm_fit: Option<&str>,
) -> i32 {
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
    let fit = match confirm_fit {
        None => None,
        Some("yes") => Some(Fit::Yes),
        Some("no") => Some(Fit::No),
        Some(other) => {
            return undetermined(
                what,
                &format!("--confirm-fit must be `yes` or `no`, got {other:?}"),
                "fix: record only the user's answer: `--confirm-fit yes` or `--confirm-fit no`",
            )
        }
    };
    let (harness, existing, description) = match k {
        FeatureKind::Simple => {
            if harness.is_some() {
                return undetermined(
                    what,
                    "--harness only applies to --kind dedicated-harness",
                    "fix: drop --harness, or use `--kind dedicated-harness`",
                );
            }
            (None, None, None)
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
            match marketplace_entries(Path::new(&mp)) {
                Determination::Known(entries) => {
                    let found = entries.get(&name).cloned();
                    let exists = found.is_some();
                    (Some(name), Some(exists), found.flatten())
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
    if fit.is_some() && existing != Some(true) {
        return undetermined(
            what,
            "--confirm-fit only applies when an existing plugin was found (`existing: true`); there is nothing to reuse",
            "fix: drop --confirm-fit; with `existing: false` (or --kind simple) build the feature without reusing a plugin",
        );
    }
    let mut d = match load_for_update(root) {
        Ok(d) => d,
        Err(e) => return undetermined(what, &e, BROKEN_RECORD_HINT),
    };
    // A fit only means something for an existing plugin; drop a stale one.
    let keep_fit = existing == Some(true);
    if let Err(e) = record::classify(&mut d, feature, k, harness.clone(), fit) {
        return undetermined(
            what,
            &e,
            "fix: rerun `sitecraft decide classify` with a non-empty --feature",
        );
    }
    if !keep_fit {
        if let Some(f) = d.features.get_mut(feature) {
            f.fit = None;
        }
    }
    let recorded_fit = d.features.get(feature).and_then(|f| f.fit);
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
            description: description.as_deref(),
            reuse: reuse_word(existing, recorded_fit),
        },
    )
}
