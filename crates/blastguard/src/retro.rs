//! Retrospective review of gate interventions: *was the stop legitimate?*
//!
//! # Why this exists
//!
//! blastguard records every non-allow verdict to overwatch's violation store
//! (`main::record_violation`), but that record answers only "what did the gate
//! say". It cannot answer the question that decides whether the gate is
//! earning its keep:
//!
//!   * did the operation the gate stopped actually **not happen**, or
//!   * did a human wave it through and run it anyway?
//!
//! A gate whose every intervention is approved has prevented nothing. It did
//! not protect the system or its configuration — it converted a machine verdict
//! into a human click, and a click that is always "yes" is training for the
//! click that should have been "no". That failure mode is invisible in the
//! violation store, because approval leaves no trace there.
//!
//! # Where the facts come from
//!
//! The Claude Code transcript already holds both halves, so this module derives
//! them rather than introducing a second ledger that could disagree with it:
//!
//!   * the **verdict** — a `hook_success` attachment carrying the PreToolUse
//!     JSON the hook printed, plus the `toolUseID` it applies to;
//!   * the **outcome** — whether a `tool_result` for that same `toolUseID`
//!     later appears, and whether it is a user rejection.
//!
//! Script gates (`guard-maintree-bash.py` and friends) do not print PreToolUse
//! JSON; they exit non-zero and Claude Code renders that as a `tool_result`
//! whose text starts `PreToolUse:<Tool> hook error:`. Those are parsed too, so
//! the review covers every gate that stopped a call, not just this crate's.
//!
//! # What this module refuses to claim
//!
//! * **Approved is not the same as wrong.** A human may have reviewed the
//!   command and been right to allow it. What an approval establishes is only
//!   that the gate prevented nothing on that occasion. The report says exactly
//!   that and no more.
//! * **A missing `tool_result` is not a rejection.** The turn may have been
//!   abandoned or the transcript truncated. That is [`Outcome::Unknown`], a
//!   third value, never folded into either of the other two.
//! * **A rephrased-and-rerun command is not detected.** If the agent worked
//!   around a stop by issuing a different command, this module scores the
//!   original as not-executed and cannot see the detour. Prevention counts are
//!   therefore an UPPER bound, and [`Report::caveats`] says so in the output —
//!   a number that flatters the gate must not travel without its qualifier.
//!
//! # Empty is not clean
//!
//! Reading no transcripts, or failing to parse the lines in them, produces
//! `unknown` — never a clean-looking report of zero interventions. CLAUDE.md 3:
//! silence that reads as "nothing to see here" is the fail-open this repo
//! exists to remove, and a review tool that reports "0 problems" when it simply
//! could not read anything would be the purest form of it.

use harness_core::verdict::Determination;
use serde_json::Value;
use std::collections::BTreeMap;

/// What became of a tool call after a gate stopped it.
///
/// Three values, not two — the same reason [`crate::model::Decision`] has
/// three. "I could not tell what happened" is not "it did not happen", and
/// collapsing them would let an unreadable transcript inflate the count of
/// operations the gate appears to have prevented.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Outcome {
    /// A `tool_result` came back that is not a user rejection: the call ran.
    /// For an `ask`, this means a human answered yes.
    Executed,
    /// The call did not run: either the gate denied it outright (no human is
    /// consulted for a `deny`), or the human rejected the `ask`.
    NotExecuted,
    /// No `tool_result` was recorded for this call. Abandoned turn, truncated
    /// transcript, or a shape this parser does not know. Deliberately NOT
    /// merged into `NotExecuted`.
    Unknown,
}

impl Outcome {
    pub fn label(self) -> &'static str {
        match self {
            Outcome::Executed => "executed-anyway",
            Outcome::NotExecuted => "not-executed",
            Outcome::Unknown => "unknown",
        }
    }
}

/// One gate intervention, joined to what happened next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Intervention {
    /// Which gate spoke: `"blastguard"`, or the script a non-zero-exit hook ran.
    pub gate: String,
    /// `"ask"` or `"deny"`.
    pub verdict: String,
    /// Stable rule discriminator. For blastguard this is
    /// [`crate::rule_id::rule_id`] of the reason, so the review groups by the
    /// same key overwatch's recurrence detection uses.
    pub rule_id: String,
    /// The human-facing reason the gate printed.
    pub reason: String,
    /// The command/operand the gate judged, when the transcript records it.
    pub command: Option<String>,
    pub outcome: Outcome,
}

/// Result of reading transcripts, including what could NOT be read.
///
/// `unreadable_lines` and `files_failed` are part of the result rather than
/// being logged and dropped: a report built from a partially-read corpus that
/// does not say so is indistinguishable from a complete one, and the reader
/// would take a shrunken intervention count for an improvement.
#[derive(Debug, Default, Clone)]
pub struct Scan {
    pub interventions: Vec<Intervention>,
    pub files_read: usize,
    pub files_failed: usize,
    pub unreadable_lines: usize,
    /// The transcript directory was never successfully listed — it is absent,
    /// or listing it failed. Kept separate from "listed it and there was
    /// nothing there": both measure nothing, but only this one is a fault the
    /// reader can act on, and merging them would hide a wrong `--dir` behind a
    /// plausible-looking empty result.
    pub dir_unreadable: bool,
}

/// Per-rule aggregation — the unit in which "is this rule earning its keep" is
/// answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleStats {
    pub gate: String,
    pub rule_id: String,
    pub total: usize,
    pub executed: usize,
    pub not_executed: usize,
    pub unknown: usize,
}

impl RuleStats {
    /// Fraction of interventions a human waved through, over those whose
    /// outcome is actually known.
    ///
    /// `None` when nothing is known — an unknown-only rule has no rate, and
    /// returning 0.0 there would read as "never approved", i.e. as evidence the
    /// gate is working.
    pub fn approval_rate(&self) -> Option<f64> {
        let decided = self.executed + self.not_executed;
        if decided == 0 {
            return None;
        }
        Some(self.executed as f64 / decided as f64)
    }

    /// The plain-language finding for this rule.
    ///
    /// Deliberately conservative: it reports what the outcomes establish, and
    /// never upgrades "prevented nothing" into "the rule is wrong". Whether a
    /// rule *should* fire is a judgment about the command; all this has is what
    /// happened.
    pub fn finding(&self) -> &'static str {
        match self.approval_rate() {
            None => "no outcome observed — cannot say",
            Some(r) if self.not_executed == 0 && r >= 1.0 => {
                "prevented nothing: every intervention was approved and ran"
            }
            Some(r) if r >= 0.8 => "mostly waved through",
            Some(_) if self.not_executed > 0 => "stopped operations that did not run",
            Some(_) => "mixed",
        }
    }
}

/// The whole review.
#[derive(Debug, Clone)]
pub struct Report {
    pub rules: Vec<RuleStats>,
    pub scan: Scan,
}

impl Report {
    /// True when the corpus was too incomplete to draw conclusions from.
    ///
    /// Read no files at all → the answer is "unknown", not "no interventions".
    pub fn is_undetermined(&self) -> bool {
        self.scan.files_read == 0
    }

    pub fn caveats(&self) -> Vec<String> {
        let mut out = vec![
            "approved != wrong: an approval shows only that this gate prevented nothing \
that time, not that stopping was a mistake."
                .to_string(),
            "not-executed is an UPPER bound on prevention: a stopped command that the \
agent rephrased and re-ran is not detected."
                .to_string(),
        ];
        if self.scan.files_failed > 0 {
            out.push(format!(
                "{} transcript file(s) could not be read — counts below are a floor, not a total.",
                self.scan.files_failed
            ));
        }
        if self.scan.unreadable_lines > 0 {
            out.push(format!(
                "{} transcript line(s) failed to parse and were skipped.",
                self.scan.unreadable_lines
            ));
        }
        out
    }
}

/// Marker Claude Code writes into a `tool_result` when the human declined.
const REJECTION_MARKERS: [&str; 3] = [
    "doesn't want to take this action",
    "User rejected",
    "user doesn't want to proceed",
];

/// Prefix of the `tool_result` text produced when a hook SCRIPT blocks a call
/// by exiting non-zero (as opposed to printing PreToolUse JSON).
const SCRIPT_BLOCK_MARKER: &str = " hook error: [";

/// Parse one project's transcript lines into interventions.
///
/// Takes already-read lines so the join logic is testable without a filesystem
/// — the same reason the rest of this crate keeps its analysis pure.
pub fn scan_lines(lines: &[String]) -> Scan {
    let mut scan = Scan {
        files_read: 1,
        ..Default::default()
    };
    let mut docs: Vec<Value> = Vec::with_capacity(lines.len());
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(line) {
            Ok(v) => docs.push(v),
            // Counted, not discarded: see `Scan`.
            Err(_) => scan.unreadable_lines += 1,
        }
    }

    // Pass 1 — the verdicts, keyed by the tool call they judged.
    let mut pending: Vec<(String, Intervention)> = Vec::new();
    for doc in &docs {
        if let Some((id, iv)) = hook_json_verdict(doc) {
            pending.push((id, iv));
        }
        if let Some(iv) = script_block_verdict(doc) {
            // A script block never runs the call, so its outcome is settled
            // here and needs no join.
            scan.interventions.push(iv);
        }
    }

    // Pass 2 — the operands, and the outcomes.
    let commands = collect_tool_use_commands(&docs);
    let results = collect_tool_results(&docs);
    for (id, mut iv) in pending {
        iv.command = commands.get(&id).cloned();
        iv.outcome = if iv.verdict == "deny" {
            // A deny is not put to anyone: the call was stopped by definition.
            Outcome::NotExecuted
        } else {
            match results.get(&id) {
                None => Outcome::Unknown,
                Some(text) if is_rejection(text) => Outcome::NotExecuted,
                Some(_) => Outcome::Executed,
            }
        };
        scan.interventions.push(iv);
    }
    scan
}

/// Merge scans from several transcript files.
pub fn merge(scans: Vec<Scan>) -> Scan {
    let mut out = Scan::default();
    for s in scans {
        out.interventions.extend(s.interventions);
        out.files_read += s.files_read;
        out.files_failed += s.files_failed;
        out.unreadable_lines += s.unreadable_lines;
        out.dir_unreadable |= s.dir_unreadable;
    }
    out
}

/// A file that could not be read at all — recorded so the report can say the
/// corpus was incomplete instead of quietly shrinking.
pub fn failed_file() -> Scan {
    Scan {
        files_failed: 1,
        ..Default::default()
    }
}

/// Read every `*.jsonl` transcript in `dir`.
///
/// Every way this can come up short is recorded rather than absorbed, because
/// each one shrinks the intervention count and a shrunken count reads as an
/// improvement:
///
///   * the directory cannot be listed → [`Scan::dir_unreadable`], distinct from
///     a directory that is merely empty. Both leave `files_read` at 0 and so
///     both report UNDETERMINED, but only one of them is a filesystem problem
///     the reader can fix;
///   * a file cannot be read, or has gone missing since the listing →
///     `files_failed`, which turns the totals into an explicit floor via
///     [`Report::caveats`].
///
/// I/O goes through [`harness_core::boundary`] rather than `std::fs` directly:
/// those wrappers return [`Determination`], so "could not read" arrives as a
/// value this function must handle instead of an `Err` an `.ok()` could quietly
/// erase. The first draft here used raw `std::fs` with `entries.flatten()`, and
/// the repo's raw-io ratchet and fail-open-guard both stopped it — in the
/// module whose entire purpose is finding that pattern.
pub fn scan_dir(dir: &std::path::Path) -> Scan {
    let Determination::Known(paths) = harness_core::boundary::read_dir_entries(dir) else {
        return Scan {
            dir_unreadable: true,
            ..Default::default()
        };
    };
    // `read_dir_entries` maps NotFound to `Known(vec![])` by a documented
    // contract ("a directory that does not exist really does contain nothing"),
    // which is right for callers walking a tree and wrong for this one: a
    // mistyped --dir is the single most likely way to get an empty review, and
    // it must not print as "listed it, found no transcripts". `is_dir()` is
    // false for an absent path and true for a real empty one, which separates
    // exactly those two — the Undetermined arm above has already taken every
    // other failure.
    if paths.is_empty() && !dir.is_dir() {
        return Scan {
            dir_unreadable: true,
            ..Default::default()
        };
    }
    let mut scans = Vec::new();
    for path in paths {
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        match harness_core::boundary::read_to_string(&path) {
            Determination::Known(Some(body)) => {
                let lines: Vec<String> = body.lines().map(str::to_string).collect();
                scans.push(scan_lines(&lines));
            }
            // Listed a moment ago and unreadable now, or unreadable outright.
            // Either way it is a transcript this review did not see.
            Determination::Known(None) | Determination::Undetermined(_) => {
                scans.push(failed_file())
            }
        }
    }
    merge(scans)
}

/// Claude Code's on-disk transcript directory for a project path.
///
/// The slug is the absolute path with `/` replaced by `-`, which is how Claude
/// Code names these directories.
pub fn transcript_dir_for(project: &std::path::Path) -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME")?;
    let slug = project.to_str()?.replace(['/', '.'], "-");
    Some(
        std::path::PathBuf::from(home)
            .join(".claude")
            .join("projects")
            .join(slug),
    )
}

pub fn build_report(scan: Scan) -> Report {
    let mut by_rule: BTreeMap<(String, String), RuleStats> = BTreeMap::new();
    for iv in &scan.interventions {
        let key = (iv.gate.clone(), iv.rule_id.clone());
        let e = by_rule.entry(key).or_insert_with(|| RuleStats {
            gate: iv.gate.clone(),
            rule_id: iv.rule_id.clone(),
            total: 0,
            executed: 0,
            not_executed: 0,
            unknown: 0,
        });
        e.total += 1;
        match iv.outcome {
            Outcome::Executed => e.executed += 1,
            Outcome::NotExecuted => e.not_executed += 1,
            Outcome::Unknown => e.unknown += 1,
        }
    }
    let mut rules: Vec<RuleStats> = by_rule.into_values().collect();
    // Loudest first: most interventions, then most waved through.
    rules.sort_by(|a, b| b.total.cmp(&a.total).then(b.executed.cmp(&a.executed)));
    Report { rules, scan }
}

/// Extract a verdict from a `hook_success` attachment carrying PreToolUse JSON.
fn hook_json_verdict(doc: &Value) -> Option<(String, Intervention)> {
    let att = doc.get("attachment")?;
    let hook_name = att.get("hookName")?.as_str()?;
    if !hook_name.starts_with("PreToolUse") {
        return None;
    }
    let tool_use_id = att.get("toolUseID")?.as_str()?.to_string();
    let stdout = att.get("stdout")?.as_str()?;
    let parsed: Value = serde_json::from_str(stdout.trim()).ok()?;
    let out = parsed.get("hookSpecificOutput")?;
    let verdict = out.get("permissionDecision")?.as_str()?;
    if verdict == "allow" {
        return None;
    }
    let reason = out
        .get("permissionDecisionReason")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let gate = gate_name_from_command(att.get("command").and_then(Value::as_str).unwrap_or(""));
    let rule_id = if gate == "blastguard" {
        crate::rule_id::rule_id(&reason).to_string()
    } else {
        // Not this crate's wording, so `rule_id` would map it to "unknown" and
        // fold every other gate's rules into one bucket. Keep them separable.
        "external-gate".to_string()
    };
    Some((
        tool_use_id,
        Intervention {
            gate,
            verdict: verdict.to_string(),
            rule_id,
            reason,
            command: None,
            outcome: Outcome::Unknown,
        },
    ))
}

/// Extract a block performed by a hook SCRIPT exiting non-zero.
fn script_block_verdict(doc: &Value) -> Option<Intervention> {
    let content = doc.get("message")?.get("content")?.as_array()?;
    for block in content {
        if block.get("type")?.as_str()? != "tool_result" {
            continue;
        }
        let text = tool_result_text(block);
        let Some(idx) = text.find(SCRIPT_BLOCK_MARKER) else {
            continue;
        };
        if !text.starts_with("PreToolUse") {
            continue;
        }
        let after = &text[idx + SCRIPT_BLOCK_MARKER.len()..];
        let script = after.split(']').next().unwrap_or("").trim();
        let reason = after
            .split_once("]:")
            .map(|(_, r)| r.trim())
            .unwrap_or("")
            .to_string();
        return Some(Intervention {
            gate: gate_name_from_command(script),
            verdict: "deny".to_string(),
            rule_id: "script-block".to_string(),
            reason: reason.chars().take(400).collect(),
            command: None,
            outcome: Outcome::NotExecuted,
        });
    }
    None
}

/// Reduce a hook's command line to the gate's name.
///
/// Transcript text reaches us with three kinds of noise that are not part of
/// the name: shell/JSON quoting, backslash escaping doubled by a nested
/// re-serialization, and hard line wraps that can fall INSIDE a path. All three
/// were observed in the real corpus splitting one gate across several rows, so
/// they are removed before the name is taken rather than being matched around.
fn gate_name_from_command(cmd: &str) -> String {
    let normalized: String = cmd
        .chars()
        .filter(|c| *c != '\\' && !c.is_whitespace())
        .collect();
    let name = normalized
        .rsplit('/')
        .next()
        .unwrap_or(&normalized)
        .trim_matches('"')
        .trim_end_matches(".py");
    if name.is_empty() {
        return "unknown-gate".to_string();
    }
    name.to_string()
}

fn collect_tool_use_commands(docs: &[Value]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for doc in docs {
        let Some(content) = doc.get("message").and_then(|m| m.get("content")) else {
            continue;
        };
        let Some(blocks) = content.as_array() else {
            continue;
        };
        for b in blocks {
            if b.get("type").and_then(Value::as_str) != Some("tool_use") {
                continue;
            }
            let Some(id) = b.get("id").and_then(Value::as_str) else {
                continue;
            };
            let input = b.get("input");
            let cmd = input
                .and_then(|i| i.get("command"))
                .and_then(Value::as_str)
                .or_else(|| {
                    input
                        .and_then(|i| i.get("file_path"))
                        .and_then(Value::as_str)
                })
                .unwrap_or_default();
            if !cmd.is_empty() {
                out.insert(id.to_string(), cmd.to_string());
            }
        }
    }
    out
}

fn collect_tool_results(docs: &[Value]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for doc in docs {
        let Some(blocks) = doc
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(Value::as_array)
        else {
            continue;
        };
        for b in blocks {
            if b.get("type").and_then(Value::as_str) != Some("tool_result") {
                continue;
            }
            let Some(id) = b.get("tool_use_id").and_then(Value::as_str) else {
                continue;
            };
            out.insert(id.to_string(), tool_result_text(b));
        }
    }
    out
}

fn tool_result_text(block: &Value) -> String {
    match block.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

fn is_rejection(text: &str) -> bool {
    REJECTION_MARKERS.iter().any(|m| text.contains(m))
}

/// Render the review as text.
pub fn render(report: &Report) -> String {
    let mut s = String::new();
    if report.is_undetermined() {
        s.push_str(
            "gate retrospective: UNDETERMINED — no transcript was read.\n\
             This is not a report of zero interventions; nothing was measured.\n",
        );
        s.push_str(if report.scan.dir_unreadable {
            "cause: the transcript directory was not listed — it is absent, or unreadable.\n"
        } else {
            "cause: the directory was listed but held no *.jsonl transcript.\n"
        });
        return s;
    }
    s.push_str(&format!(
        "gate retrospective — {} intervention(s) across {} transcript(s)\n\n",
        report.scan.interventions.len(),
        report.scan.files_read
    ));
    if report.rules.is_empty() {
        s.push_str("No gate stopped any tool call in this corpus.\n\n");
    } else {
        // Wide enough for the real names: `guard-maintree-bash` and
        // `guard-maintree-edit` are DIFFERENT gates that a narrow column
        // rendered as the same truncated string, which reads as one gate
        // double-counted.
        s.push_str(&format!(
            "{:<21} {:<28} {:>4} {:>10} {:>7} {:>7}  {}\n",
            "GATE", "RULE", "N", "RAN-ANYWAY", "STOPPED", "UNKNOWN", "FINDING"
        ));
        for r in &report.rules {
            s.push_str(&format!(
                "{:<21} {:<28} {:>4} {:>10} {:>7} {:>7}  {}\n",
                truncate(&r.gate, 21),
                truncate(&r.rule_id, 28),
                r.total,
                r.executed,
                r.not_executed,
                r.unknown,
                r.finding()
            ));
        }
        s.push('\n');
    }
    s.push_str("caveats:\n");
    for c in report.caveats() {
        s.push_str(&format!("  - {c}\n"));
    }
    s
}

/// Collapse every run of whitespace (including embedded newlines) to a single
/// space, and trim the ends.
///
/// Used on `command` in [`render_list`] rather than a bare
/// `.replace('\n', " ")`: a real recorded command can be multi-line shell
/// (`BIN=/path\nSB=/path\ncd "$SB" && …`), and squeezing whole runs — not just
/// newline characters — means two recordings of the same logical command that
/// differ only in incidental line-wrap or indentation still compare EQUAL as
/// single-line strings. That equality is the point: the mode this feeds is a
/// diff of two listings by intervention identity, and identity should track
/// what was run, not how the transcript happened to wrap it.
///
/// `gate` and `rule_id` do not need this treatment: `gate` is already reduced
/// through [`gate_name_from_command`], which strips every whitespace
/// character before the name is taken, so it can never carry a newline into
/// this render. `rule_id` is always one of [`crate::rule_id::rule_id`]'s fixed
/// `&'static str` literals (or the hardcoded `"script-block"` /
/// `"external-gate"` fallbacks) — checked, not assumed — none of which contain
/// whitespace at all.
///
/// A whitespace-ONLY input squeezes to `""`. That is a legitimate result of
/// this function, not a bug in it: it is the caller in [`render_list`] that
/// must not print the empty string bare, and it doesn't — it maps that case
/// to [`COMMAND_BLANK`], distinct from the `None` case's [`COMMAND_UNKNOWN`].
fn squeeze_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    s.chars().take(n.saturating_sub(1)).collect::<String>() + "…"
}

/// Marker printed in place of a command that the transcript never recorded.
///
/// `render_list` must never print a blank in this column: a blank reads as
/// "no command", which is a claim about the intervention this module cannot
/// make (the transcript may simply not have carried a `tool_use` block for
/// that id — see `collect_tool_use_commands`). Absent stays visibly absent.
///
/// This is a claim about the TRANSCRIPT ("no operand was ever recorded for
/// this id"), which is a different fact from [`COMMAND_BLANK`] ("an operand
/// was recorded, and it was blank") — see that constant's docstring for why
/// the two must not share text.
const COMMAND_UNKNOWN: &str = "<command not recorded>";

/// Marker printed when a command WAS recorded but is empty once squeezed.
///
/// `collect_tool_use_commands` only drops an operand when `cmd.is_empty()`
/// (i.e. zero bytes). A whitespace-only recorded command — `"   \n\t  "` —
/// is not empty by that check, so it survives into `Intervention.command` as
/// `Some("   \n\t  ")`, and `squeeze_whitespace` (which trims and collapses
/// runs of whitespace) turns that into `""`. Left unhandled, the row would
/// print `cmd= reason=…`: exactly the blank [`COMMAND_UNKNOWN`]'s own
/// docstring promises this column never prints.
///
/// Deliberately a DIFFERENT string from `COMMAND_UNKNOWN` rather than
/// reusing it: "no `tool_use` block exists for this id" and "a `tool_use`
/// block exists and its `command`/`file_path` field is blank" are different
/// facts about the transcript, and a reviewer diffing two listings by
/// intervention identity is entitled to tell them apart (the same reasoning
/// [`Outcome::Unknown`] uses to stay separate from `NotExecuted`).
const COMMAND_BLANK: &str = "<command recorded blank>";

/// Render one row per intervention, instead of `render`'s per-rule counts.
///
/// # Why this exists alongside `render`
///
/// `build_report` folds every [`Intervention`] into a [`RuleStats`] counter.
/// That answers "how many, and how did they end" but not "which ones" — and
/// `crate::rule_id::rule_id` maps MORE THAN ONE decision arm in `detect.rs`
/// onto the same id (at minimum `unknown_wrapper_ask` and
/// `analyze_shell_payload` both produce a reason containing "whose value only
/// exists at run time", both mapped to `unresolvable-command-word`). So a
/// count for that id is a sum over arms the id itself cannot separate, and
/// telling "the right 80 of 115 disappeared" from "80 arbitrary ones
/// disappeared" needs the identity of each intervention, not its tally. This
/// prints the RAW `reason` text for that — the id groups, the reason
/// distinguishes within the group.
///
/// `rule_filter`, when `Some`, keeps only interventions whose `rule_id`
/// matches exactly — narrowing the listing to the one rule under review
/// without hand-grepping the full corpus.
pub fn render_list(report: &Report, rule_filter: Option<&str>) -> String {
    let mut s = String::new();
    if report.is_undetermined() {
        // Same refusal as `render`, and for the same reason: a listing with
        // zero rows because nothing was read must not be visually
        // indistinguishable from a listing with zero rows because nothing was
        // ever stopped. Both would otherwise print "no interventions".
        s.push_str(
            "gate retrospective: UNDETERMINED — no transcript was read.\n\
             This is not a report of zero interventions; nothing was measured.\n",
        );
        s.push_str(if report.scan.dir_unreadable {
            "cause: the transcript directory was not listed — it is absent, or unreadable.\n"
        } else {
            "cause: the directory was listed but held no *.jsonl transcript.\n"
        });
        return s;
    }
    let matched: Vec<&Intervention> = report
        .scan
        .interventions
        .iter()
        .filter(|iv| rule_filter.is_none_or(|f| iv.rule_id == f))
        .collect();
    s.push_str(&format!(
        "gate retrospective (listing) — {} of {} intervention(s) across {} transcript(s)\n\n",
        matched.len(),
        report.scan.interventions.len(),
        report.scan.files_read
    ));
    if matched.is_empty() {
        // Not clean: a filter that matches nothing (typo'd --rule-id, or a
        // rule that genuinely never fired) is a fact about the query, and
        // must read as one — never as a blank that could pass for "no
        // interventions found" the way an unpopulated table would.
        s.push_str("No intervention matched this listing.\n\n");
    } else {
        for iv in &matched {
            // Reasons are free text pulled from a hook's stdout or a script's
            // stderr and can embed newlines (and, from a CRLF-recorded
            // transcript, a bare `\r`); replace both so each intervention
            // stays exactly one printed row, as the done criteria require.
            //
            // This is NOT the same treatment as `command` below
            // (`squeeze_whitespace`, which also collapses interior runs of
            // whitespace to one space): `command`'s full squeeze exists so
            // two recordings of the same logical command compare EQUAL
            // despite incidental re-wrapping — that is the identity
            // `render_list` promises for diffing two listings. `reason` is
            // prose meant for a human to read, not compared for identity
            // across runs, so collapsing its internal spacing would only
            // destroy formatting for no benefit; it only needs the two
            // characters that could turn one row into more than one line.
            let reason = iv
                .reason
                .replace("\r\n", "\n")
                .replace('\r', "\n")
                .replace('\n', " ");
            // Commands recorded from the transcript are real multi-line shell
            // (e.g. `BIN=/path\nSB=/path\ncd "$SB" && …`), not free text with
            // occasional embedded newlines. Left uncollapsed, one intervention
            // spans an unpredictable number of output lines, and some
            // continuation lines start with the word "blastguard" — a
            // line-oriented reader (or a line-count-based diff, which is the
            // whole point of this mode: the next task diffs two listings by
            // intervention identity) cannot tell that apart from a new row.
            // `squeeze_whitespace` rather than a bare newline replacement: two
            // recordings of the same logical command can still differ in
            // incidental formatting (a blank continuation line, a re-wrapped
            // transcript) without differing in what was actually run, and the
            // whole point of identity comparison is to treat those as the
            // same command rather than reporting a phantom change.
            //
            // Absent (`None`) and "recorded but blank after squeezing" are
            // both refused, but with DIFFERENT markers — see `COMMAND_BLANK`
            // for why they must not read as the same fact.
            let command = match iv.command.as_deref() {
                None => COMMAND_UNKNOWN.to_string(),
                Some(raw) => {
                    let squeezed = squeeze_whitespace(raw);
                    if squeezed.is_empty() {
                        COMMAND_BLANK.to_string()
                    } else {
                        squeezed
                    }
                }
            };
            s.push_str(&format!(
                "{:<21} {:<28} {:<15} cmd={command} reason={reason}\n",
                truncate(&iv.gate, 21),
                truncate(&iv.rule_id, 28),
                iv.outcome.label(),
            ));
        }
        s.push('\n');
    }
    s.push_str("caveats:\n");
    for c in report.caveats() {
        s.push_str(&format!("  - {c}\n"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hook_line(tool_use_id: &str, decision: &str, reason: &str) -> String {
        serde_json::json!({
            "attachment": {
                "type": "hook_success",
                "hookName": "PreToolUse:Bash",
                "toolUseID": tool_use_id,
                "command": "${CLAUDE_PLUGIN_ROOT}/bin/blastguard",
                "stdout": serde_json::json!({
                    "hookSpecificOutput": {
                        "hookEventName": "PreToolUse",
                        "permissionDecision": decision,
                        "permissionDecisionReason": reason,
                    }
                }).to_string(),
            }
        })
        .to_string()
    }

    fn tool_use_line(id: &str, command: &str) -> String {
        serde_json::json!({
            "message": {"role": "assistant", "content": [
                {"type": "tool_use", "id": id, "name": "Bash", "input": {"command": command}}
            ]}
        })
        .to_string()
    }

    fn tool_result_line(id: &str, content: &str) -> String {
        serde_json::json!({
            "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": id, "content": content}
            ]}
        })
        .to_string()
    }

    const EXPANSION: &str = "the command word `$OW` is an expansion whose value only exists at \
run time — blastguard cannot tell what program this runs";

    #[test]
    fn an_approved_ask_is_recorded_as_executed_anyway() {
        // The whole point of the module: an ask a human said yes to prevented
        // nothing, and must be visible as such.
        let lines = vec![
            hook_line("t1", "ask", EXPANSION),
            tool_use_line("t1", "\"$OW\" status"),
            tool_result_line("t1", "ok, ran fine"),
        ];
        let scan = scan_lines(&lines);
        assert_eq!(scan.interventions.len(), 1);
        let iv = &scan.interventions[0];
        assert_eq!(iv.outcome, Outcome::Executed);
        assert_eq!(iv.gate, "blastguard");
        assert_eq!(iv.rule_id, "unresolvable-command-word");
        assert_eq!(iv.command.as_deref(), Some("\"$OW\" status"));
    }

    #[test]
    fn a_rejected_ask_is_recorded_as_not_executed() {
        let lines = vec![
            hook_line("t1", "ask", EXPANSION),
            tool_use_line("t1", "\"$OW\" status"),
            tool_result_line("t1", "The user doesn't want to take this action right now."),
        ];
        let scan = scan_lines(&lines);
        assert_eq!(scan.interventions[0].outcome, Outcome::NotExecuted);
    }

    #[test]
    fn an_ask_with_no_result_is_unknown_not_prevented() {
        // The fail-open this guards: counting an abandoned turn as a
        // prevention would make every truncated transcript flatter the gate.
        let lines = vec![
            hook_line("t1", "ask", EXPANSION),
            tool_use_line("t1", "\"$OW\" status"),
        ];
        let scan = scan_lines(&lines);
        assert_eq!(scan.interventions[0].outcome, Outcome::Unknown);
    }

    #[test]
    fn a_deny_is_not_executed_without_needing_a_result() {
        let lines = vec![hook_line("t1", "deny", "rm -rf would delete the project")];
        let scan = scan_lines(&lines);
        assert_eq!(scan.interventions[0].verdict, "deny");
        assert_eq!(scan.interventions[0].outcome, Outcome::NotExecuted);
    }

    #[test]
    fn an_allow_is_not_an_intervention() {
        let lines = vec![hook_line("t1", "allow", "")];
        assert!(scan_lines(&lines).interventions.is_empty());
    }

    #[test]
    fn a_script_gate_block_is_captured_too() {
        // guard-maintree-bash.py and friends never print PreToolUse JSON.
        let text = "PreToolUse:Bash hook error: [python3 \"$CLAUDE_PROJECT_DIR/scripts/\
guard-maintree-bash.py\"]: Refused: `touch x` mutates this project's MAIN working tree.";
        let lines = vec![tool_result_line("t9", text)];
        let scan = scan_lines(&lines);
        assert_eq!(scan.interventions.len(), 1);
        assert_eq!(scan.interventions[0].gate, "guard-maintree-bash");
        assert_eq!(scan.interventions[0].outcome, Outcome::NotExecuted);
        assert!(scan.interventions[0].reason.contains("MAIN working tree"));
    }

    #[test]
    fn one_gate_stays_one_row_across_transcript_escaping_variants() {
        // Observed in the real corpus: the same hook appears with plain quotes,
        // with doubled backslash-escaping (a nested tool_result re-serialized),
        // and once wrapped across a line INSIDE the path. Splitting on those
        // artefacts filed one gate under several names, which understates how
        // concentrated its interventions are — the number the review exists to
        // report.
        let variants = [
            r#"python3 "$CLAUDE_PROJECT_DIR/scripts/guard-maintree-bash.py""#,
            r#"python3 \"$CLAUDE_PROJECT_DIR/scripts/guard-maintree-bash.py\""#,
            "python3 \\\"$CLAUDE_PROJECT_DIR/scripts/\\\nguard-maintree-bash.py\\\"",
        ];
        for v in variants {
            assert_eq!(
                gate_name_from_command(v),
                "guard-maintree-bash",
                "variant did not normalize: {v:?}"
            );
        }
    }

    #[test]
    fn unparseable_lines_are_counted_not_silently_dropped() {
        // A shrinking intervention count must never be an artefact of a parser
        // that gave up quietly.
        let lines = vec!["{not json".to_string(), hook_line("t1", "ask", EXPANSION)];
        let scan = scan_lines(&lines);
        assert_eq!(scan.unreadable_lines, 1);
        assert_eq!(scan.interventions.len(), 1);
        let report = build_report(scan);
        assert!(report
            .caveats()
            .iter()
            .any(|c| c.contains("failed to parse")));
    }

    #[test]
    fn no_transcripts_read_is_undetermined_not_clean() {
        // CLAUDE.md 3: "could not measure" must not render as "nothing wrong".
        let report = build_report(Scan::default());
        assert!(report.is_undetermined());
        let out = render(&report);
        assert!(out.contains("UNDETERMINED"));
        assert!(!out.contains("0 intervention(s)"));
    }

    #[test]
    fn a_rule_approved_every_time_is_reported_as_preventing_nothing() {
        let mut lines = Vec::new();
        for i in 0..3 {
            let id = format!("t{i}");
            lines.push(hook_line(&id, "ask", EXPANSION));
            lines.push(tool_use_line(&id, "\"$OW\" status"));
            lines.push(tool_result_line(&id, "fine"));
        }
        let report = build_report(scan_lines(&lines));
        assert_eq!(report.rules.len(), 1);
        let r = &report.rules[0];
        assert_eq!(r.total, 3);
        assert_eq!(r.executed, 3);
        assert_eq!(r.approval_rate(), Some(1.0));
        assert!(r.finding().contains("prevented nothing"));
    }

    #[test]
    fn approval_rate_is_none_when_nothing_is_known() {
        // Returning 0.0 here would read as "never approved" — evidence the gate
        // works — from a rule about which nothing at all was observed.
        let lines = vec![
            hook_line("t1", "ask", EXPANSION),
            tool_use_line("t1", "\"$OW\" x"),
        ];
        let report = build_report(scan_lines(&lines));
        assert_eq!(report.rules[0].approval_rate(), None);
        assert!(report.rules[0].finding().contains("cannot say"));
    }

    #[test]
    fn unknown_outcomes_never_count_as_prevention() {
        let lines = vec![
            hook_line("t1", "ask", EXPANSION),
            tool_use_line("t1", "\"$OW\" x"),
        ];
        let report = build_report(scan_lines(&lines));
        assert_eq!(report.rules[0].not_executed, 0);
        assert_eq!(report.rules[0].unknown, 1);
    }

    #[test]
    fn an_unlistable_directory_is_distinguished_from_an_empty_one() {
        // Both measure nothing, but only one is a fault the reader can fix. A
        // mistyped --dir that renders as a plausible empty result is how a
        // review tool reports "all clear" about a corpus it never opened.
        let scan = scan_dir(std::path::Path::new("/nonexistent/blastguard-retro-probe"));
        assert!(scan.dir_unreadable);
        let report = build_report(scan);
        assert!(report.is_undetermined());
        let out = render(&report);
        assert!(out.contains("UNDETERMINED"));
        assert!(out.contains("absent, or unreadable"), "{out}");

        // The empty-but-readable case must NOT claim a listing failure.
        let empty = build_report(Scan::default());
        assert!(!render(&empty).contains("absent, or unreadable"));
    }

    #[test]
    fn failed_files_are_surfaced_as_a_floor_caveat() {
        let scan = merge(vec![
            failed_file(),
            scan_lines(&[hook_line("t1", "deny", "x")]),
        ]);
        let report = build_report(scan);
        assert!(report
            .caveats()
            .iter()
            .any(|c| c.contains("floor, not a total")));
    }

    #[test]
    fn the_rephrase_caveat_always_travels_with_the_numbers() {
        // A prevention count that leaves without its qualifier overstates the
        // gate; the caveat is part of the result, not the prose around it.
        let report = build_report(scan_lines(&[hook_line("t1", "deny", "x")]));
        assert!(report.caveats().iter().any(|c| c.contains("UPPER bound")));
        assert!(render(&report).contains("UPPER bound"));
    }

    #[test]
    fn listing_carries_the_command_and_the_rule_id_the_table_folds_away() {
        // `build_report`'s table can only say "115 interventions for this
        // id"; it cannot say which 115. This is the identity-level check that
        // the listing actually carries both keys needed to answer that: the
        // stable id (to find the rule) and the exact command (to compare it
        // against another run's listing).
        let lines = vec![
            hook_line("t1", "ask", EXPANSION),
            tool_use_line("t1", "\"$OW\" status --unique-marker-xyz"),
            tool_result_line("t1", "ok, ran fine"),
        ];
        let report = build_report(scan_lines(&lines));
        let out = render_list(&report, None);
        assert!(
            out.contains("\"$OW\" status --unique-marker-xyz"),
            "listing did not carry the recorded command: {out}"
        );
        assert!(
            out.contains("unresolvable-command-word"),
            "listing did not carry the rule id: {out}"
        );
    }

    #[test]
    fn listing_marks_a_missing_command_explicitly_never_blank() {
        // Same principle as the rest of this crate: absent must not render as
        // clean. A deny with no `tool_use` block for its id has no recorded
        // command, and the listing must say so rather than leaving the field
        // empty (which reads as "there was no command").
        //
        // Pinned to the SPECIFIC row that lacks a command, not just "the
        // marker appears somewhere in the output": an implementation that
        // printed the marker on every row — including one that DOES have a
        // recorded command — would pass a bare `out.contains(...)` check.
        // Two interventions here, one with a command and one without, so the
        // marker must land on exactly the one that has none.
        let lines = vec![
            hook_line("t1", "deny", "rm -rf would delete the project"),
            hook_line("t2", "ask", EXPANSION),
            tool_use_line("t2", "\"$OW\" status --present-marker-abc"),
            tool_result_line("t2", "ok, ran fine"),
        ];
        let report = build_report(scan_lines(&lines));
        let out = render_list(&report, None);
        let deny_row = out
            .lines()
            .find(|l| l.contains("rm -rf would delete the project"))
            .expect("deny row missing from listing");
        assert!(
            deny_row.contains(COMMAND_UNKNOWN),
            "row with no recorded command did not carry the marker: {deny_row}"
        );
        let ask_row = out
            .lines()
            .find(|l| l.contains("present-marker-abc"))
            .expect("ask row missing from listing");
        assert!(
            !ask_row.contains(COMMAND_UNKNOWN),
            "row WITH a recorded command wrongly carried the absent-command marker: {ask_row}"
        );
    }

    #[test]
    fn listing_marks_a_whitespace_only_recorded_command_distinctly_from_absent() {
        // `collect_tool_use_commands` only drops a command when
        // `cmd.is_empty()`; a WHITESPACE-ONLY recorded command ("   \n\t  ")
        // is not empty by that check, so it survives into `Intervention` as
        // `Some("   \n\t  ")`. `squeeze_whitespace` then collapses it to `""`,
        // and the row prints `cmd= reason=...` — the exact blank this module
        // exists to refuse (COMMAND_UNKNOWN's own docstring promises this
        // column is "never" blank).
        //
        // This is also a DIFFERENT fact from no command at all: here the
        // transcript DID carry a `tool_use` block for this id, its `command`
        // field was simply blank. `COMMAND_BLANK` says that; `COMMAND_UNKNOWN`
        // (asserted absent below) would overstate — it would claim no operand
        // was ever recorded, when one was, and it was empty.
        let lines = vec![
            hook_line("t1", "ask", EXPANSION),
            tool_use_line("t1", "   \n\t  "),
            tool_result_line("t1", "ok, ran fine"),
        ];
        let report = build_report(scan_lines(&lines));
        let out = render_list(&report, None);
        assert!(
            !out.contains("cmd= reason="),
            "blank command column leaked through as an empty string: {out}"
        );
        assert!(out.contains(COMMAND_BLANK), "{out}");
        assert!(
            !out.contains(COMMAND_UNKNOWN),
            "whitespace-only recorded command must not read as \"not recorded\": {out}"
        );
    }

    #[test]
    fn listing_mode_prints_the_caveats_block() {
        // done_criteria (c): listing mode must carry the same caveats as the
        // table, not just the per-row facts. Verified directly here instead
        // of only by a human running the binary by hand.
        let report = build_report(scan_lines(&[hook_line("t1", "deny", "x")]));
        let out = render_list(&report, None);
        assert!(out.contains("caveats:"), "{out}");
        assert!(out.contains("UPPER bound"), "{out}");
        assert!(out.contains("approved != wrong"), "{out}");
    }

    #[test]
    fn reason_carriage_returns_do_not_survive_into_the_listing() {
        // `command` goes through `squeeze_whitespace` (splits on ANY Unicode
        // whitespace, `\r` included). `reason` only had `.replace('\n', " ")`,
        // so a bare `\r` — e.g. from a CRLF-recorded reason — survived into
        // the printed row. The corpus this ships against has zero CR bytes
        // today, so this is not a live line-count bug like the multiline
        // command one above, but the asymmetry between the two fields was
        // unjustified: neither field's contract wants a control character in
        // a listing meant to be one row per line.
        let lines = vec![hook_line("t1", "deny", "line one\r\nline two")];
        let out = render_list(&build_report(scan_lines(&lines)), None);
        assert!(
            !out.contains('\r'),
            "carriage return leaked into row: {out:?}"
        );
        assert!(out.contains("line one line two"), "{out}");
    }

    #[test]
    fn listing_rule_filter_narrows_to_the_matching_rule() {
        let lines = vec![
            hook_line("t1", "ask", EXPANSION),
            tool_use_line("t1", "\"$OW\" a"),
            hook_line("t2", "deny", "rm -rf would delete the project"),
        ];
        let report = build_report(scan_lines(&lines));
        let out = render_list(&report, Some("unresolvable-command-word"));
        assert!(out.contains("\"$OW\" a"));
        assert!(!out.contains("rm -rf would delete the project"));
    }

    #[test]
    fn a_command_with_embedded_newlines_stays_one_line_in_the_listing() {
        // Found by running the built binary against the real corpus:
        // `retro --rule-id unresolvable-command-word` reported "115 of 208
        // intervention(s)", but `grep -c '^blastguard'` over the same output
        // counted 121 — the listing was not one row per intervention. Cause:
        // newlines in `reason` were collapsed, but newlines in `command` were
        // not, and real recorded commands are multi-line shell
        // (`BIN=/path\nSB=/path\ncd "$SB" && …`). Some continuation lines
        // even start with the word "blastguard", making them
        // indistinguishable from a new row to a line-oriented reader — which
        // is exactly what the next task's identity-diff needs to be able to
        // do.
        //
        // Asserted as a LINE-COUNT DELTA rather than a prefix-match count: a
        // prefix filter like `starts_with("blastguard")` only catches the
        // failure when a continuation line happens to collide with another
        // row's gate name (as it did in the real corpus by coincidence). The
        // actual defect is that the row's line count is unpredictable, so the
        // real test is "does adding ONE intervention add ONE line" — which
        // fails whether or not a continuation line happens to start with
        // "blastguard".
        let baseline_lines = vec![hook_line("t0", "deny", "rm -rf would delete the project")];
        let baseline_out = render_list(&build_report(scan_lines(&baseline_lines)), None);
        let baseline_count = baseline_out.lines().count();

        let multiline_command = "BIN=/path/to/bin\nSB=/path/to/sandbox\ncd \"$SB\" && \"$BIN\" run";
        let mut with_multiline = baseline_lines.clone();
        with_multiline.push(hook_line("t1", "ask", EXPANSION));
        with_multiline.push(tool_use_line("t1", multiline_command));
        with_multiline.push(tool_result_line("t1", "ok, ran fine"));
        let with_multiline_out = render_list(&build_report(scan_lines(&with_multiline)), None);
        let with_multiline_count = with_multiline_out.lines().count();

        assert_eq!(
            with_multiline_count,
            baseline_count + 1,
            "adding one intervention (with a multi-line recorded command) must \
grow the listing by exactly one line, not one per embedded newline.\n\
baseline:\n{baseline_out}\nwith multiline command:\n{with_multiline_out}"
        );
    }

    #[test]
    fn listing_still_refuses_when_nothing_was_read() {
        // Listing mode must keep the same UNDETERMINED refusal as the table,
        // and for the same reason: zero rows because nothing was read must
        // not print the same as zero rows because nothing was stopped.
        let out = render_list(&build_report(Scan::default()), None);
        assert!(out.contains("UNDETERMINED"));
        assert!(!out.contains("0 intervention(s)"));
    }
}
