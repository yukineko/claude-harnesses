// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The retrospective's tests that run through blastguard's REAL reason ->
//! rule-id table. Moved verbatim from `crates/blastguard/src/retro.rs`'s
//! `#[cfg(test)] mod tests` when the retrospective moved to `overwatch::retro`
//! (2026-11-17); the only edit is the injected `rule_id` argument at each
//! `scan_lines` / `scan_dir` call. Assertions are unchanged. Tests that never
//! reach the table stayed in `crates/overwatch/src/retro.rs`.

use blastguard::rule_id::rule_id;
use overwatch::retro::*;
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
    let scan = scan_lines(&lines, rule_id);
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
    let scan = scan_lines(&lines, rule_id);
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
    let scan = scan_lines(&lines, rule_id);
    assert_eq!(scan.interventions[0].outcome, Outcome::Unknown);
}

#[test]
fn a_deny_is_not_executed_without_needing_a_result() {
    let lines = vec![hook_line("t1", "deny", "rm -rf would delete the project")];
    let scan = scan_lines(&lines, rule_id);
    assert_eq!(scan.interventions[0].verdict, "deny");
    assert_eq!(scan.interventions[0].outcome, Outcome::NotExecuted);
}

#[test]
fn an_allow_is_not_an_intervention() {
    let lines = vec![hook_line("t1", "allow", "")];
    assert!(scan_lines(&lines, rule_id).interventions.is_empty());
}

#[test]
fn a_script_gate_block_is_captured_too() {
    // guard-maintree-bash.py and friends never print PreToolUse JSON.
    let text = "PreToolUse:Bash hook error: [python3 \"$CLAUDE_PROJECT_DIR/scripts/\
guard-maintree-bash.py\"]: Refused: `touch x` mutates this project's MAIN working tree.";
    let lines = vec![tool_result_line("t9", text)];
    let scan = scan_lines(&lines, rule_id);
    assert_eq!(scan.interventions.len(), 1);
    assert_eq!(scan.interventions[0].gate, "guard-maintree-bash");
    assert_eq!(scan.interventions[0].outcome, Outcome::NotExecuted);
    assert!(scan.interventions[0].reason.contains("MAIN working tree"));
}

#[test]
fn unparseable_lines_are_counted_not_silently_dropped() {
    // A shrinking intervention count must never be an artefact of a parser
    // that gave up quietly.
    let lines = vec!["{not json".to_string(), hook_line("t1", "ask", EXPANSION)];
    let scan = scan_lines(&lines, rule_id);
    assert_eq!(scan.unreadable_lines, 1);
    assert_eq!(scan.interventions.len(), 1);
    let report = build_report(scan);
    assert!(report
        .caveats()
        .iter()
        .any(|c| c.contains("failed to parse")));
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
    let report = build_report(scan_lines(&lines, rule_id));
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
    let report = build_report(scan_lines(&lines, rule_id));
    assert_eq!(report.rules[0].approval_rate(), None);
    assert!(report.rules[0].finding().contains("cannot say"));
}

#[test]
fn unknown_outcomes_never_count_as_prevention() {
    let lines = vec![
        hook_line("t1", "ask", EXPANSION),
        tool_use_line("t1", "\"$OW\" x"),
    ];
    let report = build_report(scan_lines(&lines, rule_id));
    assert_eq!(report.rules[0].not_executed, 0);
    assert_eq!(report.rules[0].unknown, 1);
}

#[test]
fn an_unlistable_directory_is_distinguished_from_an_empty_one() {
    // Both measure nothing, but only one is a fault the reader can fix. A
    // mistyped --dir that renders as a plausible empty result is how a
    // review tool reports "all clear" about a corpus it never opened.
    let scan = scan_dir(
        std::path::Path::new("/nonexistent/blastguard-retro-probe"),
        rule_id,
    );
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
        scan_lines(&[hook_line("t1", "deny", "x")], rule_id),
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
    let report = build_report(scan_lines(&[hook_line("t1", "deny", "x")], rule_id));
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
    let report = build_report(scan_lines(&lines, rule_id));
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
    let report = build_report(scan_lines(&lines, rule_id));
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
    let report = build_report(scan_lines(&lines, rule_id));
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
    let report = build_report(scan_lines(&[hook_line("t1", "deny", "x")], rule_id));
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
    let out = render_list(&build_report(scan_lines(&lines, rule_id)), None);
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
    let report = build_report(scan_lines(&lines, rule_id));
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
    let baseline_out = render_list(&build_report(scan_lines(&baseline_lines, rule_id)), None);
    let baseline_count = baseline_out.lines().count();

    let multiline_command = "BIN=/path/to/bin\nSB=/path/to/sandbox\ncd \"$SB\" && \"$BIN\" run";
    let mut with_multiline = baseline_lines.clone();
    with_multiline.push(hook_line("t1", "ask", EXPANSION));
    with_multiline.push(tool_use_line("t1", multiline_command));
    with_multiline.push(tool_result_line("t1", "ok, ran fine"));
    let with_multiline_out = render_list(&build_report(scan_lines(&with_multiline, rule_id)), None);
    let with_multiline_count = with_multiline_out.lines().count();

    assert_eq!(
        with_multiline_count,
        baseline_count + 1,
        "adding one intervention (with a multi-line recorded command) must \
grow the listing by exactly one line, not one per embedded newline.\n\
baseline:\n{baseline_out}\nwith multiline command:\n{with_multiline_out}"
    );
}
