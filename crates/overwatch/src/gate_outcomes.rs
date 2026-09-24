//! `overwatch gate-outcomes`: how each Stop gate's firings ended.
//!
//! # Why this exists
//!
//! A gate is only worth defending if its verdicts are, on the whole, acted on.
//! If the reasoned exit is taken every time, either the gate's basis is wrong or
//! the gate is unnecessary — and that verdict is reachable only from measured
//! data, **with the denominator**. An override-only log reads as healthy exactly
//! when the gate is overridden constantly, so this readout always prints the
//! number of firings next to each outcome's count and rate (backlog 26d84958).
//!
//! # Derived at read time — no new store
//!
//! The Stop gates (donegate, tdd, reviewgate) already journal every verdict and
//! every session-scoped skip to `~/.<gate>/state/log.jsonl`
//! (`harness_core::gate::run::append_jsonl` / `skip_command` /
//! `consume_session_skip`). This module reads those files **by path**, like
//! [`crate::review_gate_decisions`] reads condukt's journal, so the dependency
//! direction (`harness-core <- overwatch <- blastguard <- condukt`) is untouched.
//!
//! Known limitation: the default state path is read. A gate whose `state_dir`
//! was overridden in its config is not found here and reports `absent`.
//!
//! **The logs are machine-global**: no line carries a project or cwd, so every
//! number here is summed across every project on this machine. The header says
//! so on every run.
//!
//! # Classification (explicit records only — user ruling 2026-09-24)
//!
//! Lines are walked in file order, tracked per session (`session` on verdict
//! lines, `session_id` on event lines; event lines carry no `ts`, so file order
//! is the only ordering available).
//!
//! * **firing**: a verdict starting with `blocked`. Consecutive blocks in one
//!   session before any resolver are ONE firing; `blocks` counts the raw lines.
//! * **complied**: the gate's own pass verdict later in the same session.
//! * **overridden**: a `skip_consumed` event; its human reason is kept for
//!   clustering.
//! * **released by the loop bound**: a `giveup` / `*-giveup` verdict — the
//!   bounded `stop_hook_active` allow. Neither the agent complying nor a human
//!   overriding, so it is its own bucket, never folded into `complied`.
//! * **unresolved**: a firing still open when the file ends. Never complied.
//! * **escalated**: these gates have no escalate path, so it is `n/a` (JSON
//!   `null`), never a `0` that would read as "never escalated".
//!
//! # Fail-closed
//!
//! An unreadable log (permission, non-UTF-8) is `undetermined` with null counts,
//! not an empty history. A line that is not a JSON object, lacks a session, or
//! carries a verdict/event this module does not recognise is counted as an
//! undetermined line with its line number — never dropped. Either makes the
//! process exit 3 after printing.

use harness_core::verdict::Determination;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

/// Exit code when any part of the answer could not be determined (mirrors
/// `review_queue::SourceHealth::SomeUndetermined`).
pub const EXIT_UNDETERMINED: i32 = 3;

/// One gate's vocabulary: which verdicts pass, which are known and neutral.
struct GateSpec {
    name: &'static str,
    pass: &'static [&'static str],
    neutral_verdicts: &'static [&'static str],
}

const NEUTRAL_EVENTS: &[&str] = &[
    "skip_issued",
    "concession_recorded",
    "concession_still_owed",
    "concession_unclearable",
    "concession_spent",
];

const GATES: &[GateSpec] = &[
    GateSpec {
        name: "donegate",
        pass: &["green"],
        neutral_verdicts: &["skip"],
    },
    GateSpec {
        name: "tdd",
        pass: &["ok"],
        neutral_verdicts: &["skip"],
    },
    GateSpec {
        name: "reviewgate",
        pass: &["already-reviewed", "no-reviewable-changes", "empty-diff"],
        neutral_verdicts: &["skip", "no-git", "git-scan-failed", "diff-truncated"],
    },
];

/// What one log line means for the firing state machine.
#[derive(Debug, PartialEq, Eq)]
enum LineKind {
    Block,
    Pass,
    Override(String),
    Release,
    Neutral,
}

/// Counts for one gate whose log was read (or is absent).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Tally {
    pub lines: usize,
    pub firings: usize,
    pub blocks: usize,
    pub complied: usize,
    pub overridden: usize,
    pub released: usize,
    pub unresolved: usize,
    pub override_reasons: BTreeMap<String, usize>,
    pub overrides_without_firing: usize,
    pub releases_without_firing: usize,
    pub undetermined_lines: Vec<usize>,
}

fn classify(spec: &GateSpec, obj: &serde_json::Map<String, Value>) -> Option<(String, LineKind)> {
    let session = obj
        .get("session")
        .or_else(|| obj.get("session_id"))
        .and_then(Value::as_str)?
        .to_string();
    if let Some(v) = obj.get("verdict").and_then(Value::as_str) {
        let kind = if v.starts_with("blocked") {
            LineKind::Block
        } else if v == "giveup" || v.ends_with("-giveup") {
            LineKind::Release
        } else if spec.pass.contains(&v) {
            LineKind::Pass
        } else if spec.neutral_verdicts.contains(&v) {
            LineKind::Neutral
        } else {
            return None;
        };
        return Some((session, kind));
    }
    if let Some(e) = obj.get("event").and_then(Value::as_str) {
        let kind = if e == "skip_consumed" {
            let reason = obj
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("(no reason recorded)")
                .to_string();
            LineKind::Override(reason)
        } else if NEUTRAL_EVENTS.contains(&e) {
            LineKind::Neutral
        } else {
            return None;
        };
        return Some((session, kind));
    }
    None
}

/// Pure core: tally one gate's log text.
fn tally_text(spec: &GateSpec, text: &str) -> Tally {
    let mut t = Tally::default();
    // session -> an episode is open
    let mut open: HashMap<String, bool> = HashMap::new();
    for (idx, raw) in text.lines().enumerate() {
        if raw.trim().is_empty() {
            continue;
        }
        t.lines += 1;
        let parsed = serde_json::from_str::<Value>(raw).ok();
        let classified = parsed
            .as_ref()
            .and_then(Value::as_object)
            .and_then(|o| classify(spec, o));
        let Some((session, kind)) = classified else {
            t.undetermined_lines.push(idx + 1);
            continue;
        };
        let is_open = open.get(&session).copied().unwrap_or(false);
        match kind {
            LineKind::Block => {
                t.blocks += 1;
                if !is_open {
                    t.firings += 1;
                    open.insert(session, true);
                }
            }
            LineKind::Pass => {
                if is_open {
                    t.complied += 1;
                    open.insert(session, false);
                }
            }
            LineKind::Override(reason) => {
                if is_open {
                    t.overridden += 1;
                    *t.override_reasons.entry(reason).or_insert(0) += 1;
                    open.insert(session, false);
                } else {
                    t.overrides_without_firing += 1;
                }
            }
            LineKind::Release => {
                if is_open {
                    t.released += 1;
                    open.insert(session, false);
                } else {
                    t.releases_without_firing += 1;
                }
            }
            LineKind::Neutral => {}
        }
    }
    t.unresolved = open.values().filter(|o| **o).count();
    t
}

/// One gate's result: the log was read (`Some` text) / absent (`None`), or it
/// could not be read at all.
pub struct GateReport {
    pub gate: &'static str,
    pub log: PathBuf,
    pub status: Determination<Option<Tally>>,
}

fn log_path(home: &Path, gate: &str) -> PathBuf {
    home.join(format!(".{gate}"))
        .join("state")
        .join("log.jsonl")
}

/// Read every gate's log under `home`.
pub fn collect(home: &Path) -> Vec<GateReport> {
    GATES
        .iter()
        .map(|spec| {
            let log = log_path(home, spec.name);
            let status = match harness_core::boundary::read_to_string(&log) {
                Determination::Known(Some(text)) => {
                    Determination::known(Some(tally_text(spec, &text)))
                }
                Determination::Known(None) => Determination::known(None),
                // Forwarded, not re-recorded: boundary already counted this.
                Determination::Undetermined(why) => Determination::Undetermined(why),
            };
            GateReport {
                gate: spec.name,
                log,
                status,
            }
        })
        .collect()
}

fn rate(n: usize, d: usize) -> Value {
    if d == 0 {
        Value::Null
    } else {
        json!(n as f64 / d as f64)
    }
}

fn sorted_reasons(t: &Tally) -> Vec<(String, usize)> {
    let mut v: Vec<(String, usize)> = t
        .override_reasons
        .iter()
        .map(|(r, c)| (r.clone(), *c))
        .collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    v
}

/// Whether any report is undetermined (unreadable log or undetermined lines).
pub fn any_undetermined(reports: &[GateReport]) -> bool {
    reports.iter().any(|r| match &r.status {
        Determination::Undetermined(_) => true,
        Determination::Known(Some(t)) => !t.undetermined_lines.is_empty(),
        Determination::Known(None) => false,
    })
}

fn gate_json(r: &GateReport) -> Value {
    let log = r.log.display().to_string();
    match &r.status {
        Determination::Undetermined(why) => json!({
            "gate": r.gate, "log": log, "status": "undetermined", "reason": why.as_str(),
            "lines": null, "firings": null, "blocks": null, "complied": null,
            "overridden": null, "released_by_loop_bound": null, "unresolved": null,
            "escalated": null,
            "rates": {"complied": null, "overridden": null, "released_by_loop_bound": null, "unresolved": null},
            "override_reasons": null, "overrides_without_firing": null,
            "releases_without_firing": null, "undetermined_lines": null,
            "undetermined_line_numbers": null,
        }),
        Determination::Known(opt) => {
            let absent = opt.is_none();
            let t = opt.clone().unwrap_or_default();
            let reasons: Vec<Value> = sorted_reasons(&t)
                .into_iter()
                .map(|(reason, count)| json!({"reason": reason, "count": count}))
                .collect();
            json!({
                "gate": r.gate, "log": log,
                "status": if absent { "absent" } else { "read" },
                "reason": null,
                "lines": t.lines, "firings": t.firings, "blocks": t.blocks,
                "complied": t.complied, "overridden": t.overridden,
                "released_by_loop_bound": t.released, "unresolved": t.unresolved,
                "escalated": null,
                "rates": {
                    "complied": rate(t.complied, t.firings),
                    "overridden": rate(t.overridden, t.firings),
                    "released_by_loop_bound": rate(t.released, t.firings),
                    "unresolved": rate(t.unresolved, t.firings),
                },
                "override_reasons": reasons,
                "overrides_without_firing": t.overrides_without_firing,
                "releases_without_firing": t.releases_without_firing,
                "undetermined_lines": t.undetermined_lines.len(),
                "undetermined_line_numbers": t.undetermined_lines,
            })
        }
    }
}

/// UTC civil date for unix seconds (Howard Hinnant's days-to-civil).
fn utc_date(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

fn pct(n: usize, d: usize) -> String {
    if d == 0 {
        "n/a".to_string()
    } else {
        format!("{:.1}%", n as f64 * 100.0 / d as f64)
    }
}

fn render_text(reports: &[GateReport], rev: &str, date: &str) -> String {
    let mut out = String::new();
    out.push_str("gate-outcomes: how each Stop gate's firings ended\n");
    out.push_str("  command: overwatch gate-outcomes\n");
    out.push_str(&format!("  rev: {rev}\n"));
    out.push_str(&format!("  date: {date} (UTC)\n"));
    out.push_str(
        "  scope: machine-global — the logs carry no project, so these counts sum every \
         project on this machine\n",
    );
    for r in reports {
        out.push('\n');
        out.push_str(&format!("{} ({})\n", r.gate, r.log.display()));
        match &r.status {
            Determination::Undetermined(why) => {
                out.push_str(&format!(
                    "  UNDETERMINED: log could not be read — {}\n  escalated n/a\n",
                    why.as_str()
                ));
            }
            Determination::Known(opt) => {
                if opt.is_none() {
                    out.push_str("  log absent (no firings recorded)\n");
                }
                let t = opt.clone().unwrap_or_default();
                let f = t.firings;
                out.push_str(&format!(
                    "  firings {f} (block verdicts {}, lines {})\n",
                    t.blocks, t.lines
                ));
                for (label, n) in [
                    ("complied", t.complied),
                    ("overridden", t.overridden),
                    ("released by the loop bound", t.released),
                    ("unresolved", t.unresolved),
                ] {
                    out.push_str(&format!("  {label:<27} {n:>5} / {f}  {}\n", pct(n, f)));
                }
                out.push_str("  escalated n/a (no escalate path in this gate)\n");
                let reasons = sorted_reasons(&t);
                if !reasons.is_empty() {
                    out.push_str("  override reasons:\n");
                    for (reason, count) in reasons {
                        out.push_str(&format!("    {count:>3}x {reason}\n"));
                    }
                }
                if t.overrides_without_firing > 0 {
                    out.push_str(&format!(
                        "  overrides without an observed firing: {}\n",
                        t.overrides_without_firing
                    ));
                }
                if t.releases_without_firing > 0 {
                    out.push_str(&format!(
                        "  releases without an observed firing: {}\n",
                        t.releases_without_firing
                    ));
                }
                if !t.undetermined_lines.is_empty() {
                    out.push_str(&format!(
                        "  UNDETERMINED lines: {} (line numbers {:?}) — not classified, not dropped\n",
                        t.undetermined_lines.len(),
                        t.undetermined_lines
                    ));
                }
            }
        }
    }
    out
}

/// CLI entry: print the readout; exit 3 when any part was undetermined.
pub fn run_cli(json_out: bool, now: i64) -> anyhow::Result<()> {
    let Some(home) = dirs::home_dir() else {
        eprintln!("overwatch gate-outcomes: UNDETERMINED — no home directory");
        std::process::exit(EXIT_UNDETERMINED);
    };
    let reports = collect(&home);
    let rev = format!("overwatch {}", env!("CARGO_PKG_VERSION"));
    let date = utc_date(now);
    if json_out {
        let out = json!({
            "command": "overwatch gate-outcomes --json",
            "rev": rev,
            "date": date,
            "scope": "machine-global",
            "gates": reports.iter().map(gate_json).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        print!("{}", render_text(&reports, &rev, &date));
    }
    if any_undetermined(&reports) {
        eprintln!(
            "overwatch gate-outcomes: some logs or lines could not be determined (exit {EXIT_UNDETERMINED})"
        );
        std::process::exit(EXIT_UNDETERMINED);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_date_known_points() {
        assert_eq!(utc_date(0), "1970-01-01");
        assert_eq!(utc_date(951_782_400), "2000-02-29");
        assert_eq!(utc_date(1_790_208_000), "2026-09-24");
    }
}
