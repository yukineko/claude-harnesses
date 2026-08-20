// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Depth-cap fail-open (backlog 4b7b80d3).
//!
//! `detect::MAX_SHELL_DEPTH` bounds how far the analyser recurses into shell /
//! wrapper nesting. Hitting that cap means ANALYSIS DID NOT FINISH — it is the
//! same "we did not finish looking" condition as budget exhaustion, which the
//! sibling rule in `detect_bash` already resolves to an `Ask`. Every depth-cap
//! site instead resolved it to `Allow`, so a destructive payload nested past the
//! cap was waved through unanalysed.
//!
//! These tests drive the PUBLIC entry point (`blastguard::detect::detect` with
//! tool name `Bash`) only — no private helper is called, so they measure what a
//! real PreToolUse hook call would produce.
//!
//! ## Env / hardening
//!
//! `detect()` is pure and reads NO environment: `CLAUDE_CODE_ENTRYPOINT` is
//! consulted only by `crate::interactive` (used by `src/main.rs` when deciding
//! whether an `Ask` may be surfaced or must be `hardened()` into a `Deny`).
//! So these assertions are on `is_blocking()` (Deny OR Ask) and need no env
//! setup or serialisation. That is also the right predicate: `Ask` hardens to
//! `Deny` whenever no human can answer, so an `Ask` is never weaker than the
//! `Allow` it replaces.
//!
//! ## Budget-vs-depth confound (checked, NOT present at these sizes)
//!
//! `detect_bash` also caps total node visits (`MAX_ANALYSIS_NODES = 2_000`) and
//! asks with a DIFFERENT reason when that is exhausted. Every command built here
//! is measured to be far below that cap: before the fix these commands return
//! `Allow`, and budget exhaustion returns `Ask` — so the block, when it appears,
//! cannot be the budget's doing. `assert_depth_ask` additionally rejects a reason
//! that talks about the budget, so a depth test cannot pass for the budget's
//! reason.

use blastguard::detect::detect;
use blastguard::model::Decision;
use serde_json::json;

/// The public entry point, exactly as the PreToolUse hook calls it.
fn analyse(cmd: &str) -> Decision {
    detect("Bash", Some(&json!({ "command": cmd })))
}

/// The cap in `detect.rs`. Mirrored here on purpose: it is private, and a test
/// that silently followed a change to it would stop testing anything.
const MAX_SHELL_DEPTH: usize = 8;

/// Wording accepted for the depth-cap ask. Deliberately a family of words
/// rather than one exact string: the fix owns the phrasing, the test owns the
/// requirement that the reason identify the NESTING/DEPTH limit and not be
/// confusable with the budget ask.
const DEPTH_WORDS: [&str; 4] = ["depth", "nest", "deep", "recursion"];

fn reason_of(d: &Decision) -> String {
    match d {
        Decision::Deny(r) | Decision::Ask(r) => r.to_lowercase(),
        Decision::Allow => String::new(),
    }
}

/// Assert a verdict blocks, and — when it blocks by asking — that the ask names
/// the depth/nesting limit rather than the analysis budget.
///
/// A `Deny` is accepted unconditionally: it is strictly stronger than the ask
/// the fix promises, and some of these command lines legitimately reach a real
/// destructive rule as well.
fn assert_depth_ask(cmd: &str, ctx: &str) {
    let d = analyse(cmd);
    assert!(
        d.is_blocking(),
        "{ctx}: expected a BLOCKING verdict (Deny or Ask) because analysis hit \
         the {MAX_SHELL_DEPTH}-level nesting cap before reaching the payload, \
         got {d:?}\n  command: {cmd}"
    );
    if d.is_ask() {
        let r = reason_of(&d);
        assert!(
            !r.contains("budget"),
            "{ctx}: the ask blames the ANALYSIS BUDGET, not the depth cap — this \
             test would then be passing for the wrong cause. reason: {r}\n  command: {cmd}"
        );
        assert!(
            DEPTH_WORDS.iter().any(|w| r.contains(w)),
            "{ctx}: the ask reason must identify the nesting/depth limit (one of \
             {DEPTH_WORDS:?}) so it is distinguishable from the budget-exhaustion \
             ask. reason: {r}\n  command: {cmd}"
        );
    }
}

fn assert_allow(cmd: &str, ctx: &str) {
    let d = analyse(cmd);
    assert_eq!(
        d,
        Decision::Allow,
        "{ctx}: ordinary shallow command must stay ALLOW — blastguard's whole \
         value is its no-false-positive bias. command: {cmd}"
    );
}

// ---------------------------------------------------------------------------
// Nesting builders. Every one is programmatic so the depth is unambiguous and
// reported in the failure message.
// ---------------------------------------------------------------------------

/// `sh -c sh -c … <payload>`; each `sh -c` layer costs exactly one level of
/// recursion (`analyze_command_at`'s shell arm -> `analyze_shell_payload` ->
/// `detect_bash(depth + 1)`).
fn nest_sh(levels: usize, payload: &str) -> String {
    let mut s = "sh -c ".repeat(levels);
    s.push_str(payload);
    s
}

/// `eval 'eval ' … '<payload>'` — the `eval`/`exec`/`source` evaluation arm.
fn nest_eval(levels: usize, payload: &str) -> String {
    let mut cur = payload.to_string();
    for _ in 0..levels {
        cur = format!("eval '{cur}'");
    }
    cur
}

/// `xargs xargs … <payload>` — the `analyze_xargs` re-analysis arm.
fn nest_xargs(levels: usize, payload: &str) -> String {
    let mut s = "xargs ".repeat(levels);
    s.push_str(payload);
    s
}

const PAYLOAD: &str = "rm -rf /";

// ---------------------------------------------------------------------------
// 1. Headline: a destructive payload nested past the cap must not be allowed.
// ---------------------------------------------------------------------------

#[test]
fn destructive_payload_nested_past_the_shell_depth_cap_is_not_allowed() {
    let levels = MAX_SHELL_DEPTH + 1; // 9
    let cmd = nest_sh(levels, PAYLOAD);
    assert_depth_ask(
        &cmd,
        &format!(
            "`{PAYLOAD}` wrapped in {levels} `sh -c` layers (cap is \
             {MAX_SHELL_DEPTH}); one layer below the cap the SAME payload is \
             denied, so an Allow here is purely 'we stopped looking'"
        ),
    );
}

/// The control for the headline test: one level BELOW the cap, the identical
/// payload is caught. Without this, an assertion about level 9 proves nothing
/// about the cap being the cause.
#[test]
fn the_same_payload_one_level_below_the_cap_is_denied() {
    let cmd = nest_sh(MAX_SHELL_DEPTH - 1, PAYLOAD);
    let d = analyse(&cmd);
    assert!(
        d.is_deny(),
        "control: at depth {} (below the cap) the payload must still be DENIED \
         by the ordinary rm rule; got {d:?}\n  command: {cmd}",
        MAX_SHELL_DEPTH - 1
    );
}

/// Depth is monotone in one direction: once a payload is destructive, adding
/// MORE wrapping must never turn a blocking verdict back into `Allow`. Pinning
/// a range (rather than a single depth) catches a fix that moves the cliff by
/// one instead of removing it.
#[test]
fn deeper_nesting_never_downgrades_a_blocking_verdict_to_allow() {
    for levels in 1..=(MAX_SHELL_DEPTH + 6) {
        let cmd = nest_sh(levels, PAYLOAD);
        assert_depth_ask(&cmd, &format!("{levels} `sh -c` layers"));
    }
}

// ---------------------------------------------------------------------------
// 2. Per-site coverage.
// ---------------------------------------------------------------------------

/// Site: `analyze_command_at`'s `if depth < MAX_SHELL_DEPTH` shell block
/// (`sh -c` payload re-analysis).
#[test]
fn site_shell_dash_c_chain_past_the_cap() {
    assert_depth_ask(
        &nest_sh(MAX_SHELL_DEPTH + 1, PAYLOAD),
        "site: sh -c payload re-analysis",
    );
}

/// Site: the same block's `eval`/`exec`/`source` arm. No `sh` appears in this
/// command line, so the evaluation arm is what is being bounded.
#[test]
fn site_eval_chain_past_the_cap() {
    let levels = MAX_SHELL_DEPTH + 1;
    let cmd = nest_eval(levels, PAYLOAD);
    // Control: below the cap the payload is denied.
    let below = nest_eval(MAX_SHELL_DEPTH - 1, PAYLOAD);
    assert!(
        analyse(&below).is_deny(),
        "control: {} eval layers must still deny; got {:?}\n  command: {below}",
        MAX_SHELL_DEPTH - 1,
        analyse(&below)
    );
    assert_depth_ask(&cmd, &format!("site: eval evaluation arm, {levels} layers"));
}

/// Site: `analyze_xargs`'s `if depth >= MAX_SHELL_DEPTH { return Allow }`.
#[test]
fn site_xargs_chain_past_the_cap() {
    let levels = MAX_SHELL_DEPTH + 1;
    let cmd = nest_xargs(levels, PAYLOAD);
    let below = nest_xargs(MAX_SHELL_DEPTH - 1, PAYLOAD);
    assert!(
        analyse(&below).is_deny(),
        "control: {} xargs layers must still deny; got {:?}\n  command: {below}",
        MAX_SHELL_DEPTH - 1,
        analyse(&below)
    );
    assert_depth_ask(&cmd, &format!("site: analyze_xargs, {levels} layers"));
}

/// Site: `unknown_wrapper_ask`'s `if depth >= MAX_SHELL_DEPTH { return Allow }`.
///
/// An unrecognised wrapper in front of a destructive command line asks at
/// shallow depth ("`my-cleanup-wrapper` is not a command blastguard
/// recognises…"). Past the cap that rule returned `Allow`, i.e. the deeper the
/// unknown wrapper, the quieter blastguard got.
#[test]
fn site_unknown_wrapper_past_the_cap() {
    let tail = "my-cleanup-wrapper rm -rf /";
    // Measured cliff: this site stops asking one layer earlier than the
    // payload-recursion sites, because the wrapper rule is evaluated at the
    // segment's own depth rather than at depth + 1.
    let below = nest_xargs(MAX_SHELL_DEPTH - 2, tail);
    assert!(
        analyse(&below).is_blocking(),
        "control: an unknown wrapper in front of `rm -rf /` asks below the cap; \
         got {:?}\n  command: {below}",
        analyse(&below)
    );
    // Measured: layers 0..=7 still produce the wrapper's OWN ask (analysis
    // reached the wrapper), layers >= 8 fell through to Allow. Only the latter
    // range is the depth-cap fail-open, so only it is held to the depth wording.
    for levels in (MAX_SHELL_DEPTH - 1)..MAX_SHELL_DEPTH {
        let cmd = nest_xargs(levels, tail);
        assert!(
            analyse(&cmd).is_blocking(),
            "control: {levels} xargs layers is still below the cap and must \
             block; got {:?}\n  command: {cmd}",
            analyse(&cmd)
        );
    }
    for levels in MAX_SHELL_DEPTH..=(MAX_SHELL_DEPTH + 2) {
        assert_depth_ask(
            &nest_xargs(levels, tail),
            &format!("site: unknown_wrapper_ask behind {levels} xargs layers"),
        );
    }
}

/// Site: `analyze_find`'s `if depth < MAX_SHELL_DEPTH` `-exec` tail
/// re-analysis.
///
/// Honest scope note: this command line is blocked below the cap and allowed
/// above it, but the transition cannot be attributed to `analyze_find`'s site
/// ALONE from outside the crate — the `-exec` tail is re-analysed via
/// `detect_bash(depth + 1)`, and the wrapper rule that fires on the tail has its
/// own cap one level down. The observable requirement is the same either way:
/// a destructive `find -exec` payload behind deep nesting must not be allowed.
#[test]
fn site_find_exec_tail_past_the_cap() {
    let tail = "find . -exec my-wrapper rm -rf {} +";
    let below = nest_sh(MAX_SHELL_DEPTH - 2, tail);
    assert!(
        analyse(&below).is_blocking(),
        "control: the same find -exec line blocks below the cap; got {:?}\n  command: {below}",
        analyse(&below)
    );
    for levels in (MAX_SHELL_DEPTH - 1)..=(MAX_SHELL_DEPTH + 2) {
        assert_depth_ask(
            &nest_sh(levels, tail),
            &format!("site: find -exec tail re-analysis behind {levels} sh -c layers"),
        );
    }
}

/// A destructive `find -exec` whose exec'd program is a SHELL (so the payload is
/// only reachable through re-analysis), nested past the cap.
#[test]
fn find_exec_shell_payload_past_the_cap() {
    for levels in (MAX_SHELL_DEPTH + 1)..=(MAX_SHELL_DEPTH + 3) {
        assert_depth_ask(
            &nest_sh(levels, "find . -exec sh -c 'rm -rf /' +"),
            &format!("find -exec sh -c payload behind {levels} sh -c layers"),
        );
    }
}

/// Mixed chain — the shapes a real bypass would use are not homogeneous.
#[test]
fn mixed_wrapper_chain_past_the_cap() {
    let cmd = format!(
        "{}{}",
        "sh -c xargs sh -c xargs sh -c xargs sh -c xargs sh -c xargs ", PAYLOAD
    );
    // 10 alternating wrapper layers.
    assert_depth_ask(&cmd, "mixed sh -c / xargs chain, 10 layers");
}

// ---------------------------------------------------------------------------
// 3. Over-block guards. These are load-bearing: the fix must add asks ONLY on
//    the unanalysed-because-too-deep path, never on ordinary work.
// ---------------------------------------------------------------------------

#[test]
fn ordinary_shallow_commands_are_still_allowed() {
    for cmd in [
        "ls",
        "ls -la src/",
        "git status",
        "git log --oneline -20",
        "cargo test -p blastguard",
        "cargo build --release",
        "npm run build",
        "echo hi",
        "sh -c 'echo hi'",
        "bash -c 'cargo test'",
        "sh -c 'sh -c \"echo hi\"'",
        "find . -name '*.rs'",
        "find . -name '*.rs' -exec grep -l 'rm' {} \\;",
        "echo hi | xargs echo",
        "git diff | head -50",
        "rg TODO crates/",
        "python3 scripts/check-plugin-versions.py",
    ] {
        assert_allow(cmd, "over-block guard");
    }
}

/// The same shallow-nesting shapes the fix touches, with a HARMLESS payload:
/// wrapping `echo hi` in a few shell layers must stay silent.
#[test]
fn shallow_nesting_of_a_harmless_payload_is_allowed() {
    for levels in 0..MAX_SHELL_DEPTH {
        assert_allow(
            &nest_sh(levels, "echo hi"),
            &format!("{levels} sh -c layers around `echo hi`"),
        );
        assert_allow(
            &nest_xargs(levels, "echo hi"),
            &format!("{levels} xargs layers around `echo hi`"),
        );
    }
}

/// A harmless payload nested PAST the cap. This one is a judgement call the fix
/// makes deliberately: analysis did not finish, so the honest answer is an ask
/// even though the payload happens to be `echo`. What must NOT happen is a
/// DENY — an unanalysed command is unknown, not known-destructive.
#[test]
fn harmless_payload_past_the_cap_may_ask_but_must_not_deny() {
    let cmd = nest_sh(MAX_SHELL_DEPTH + 1, "echo hi");
    let d = analyse(&cmd);
    assert!(
        !d.is_deny(),
        "a command that was merely not finished being analysed must not be \
         reported as known-destructive; got {d:?}\n  command: {cmd}"
    );
}

// ---------------------------------------------------------------------------
// 4. Composition: Deny must outrank the depth Ask.
//
//    This is the property that makes `acc.record(depth_exhausted())` correct and
//    an early `return depth_exhausted()` wrong. Without it, a refactor to the
//    early-return form would silently DOWNGRADE a known-destructive command line
//    to a question — and a question can be answered "yes".
// ---------------------------------------------------------------------------

#[test]
fn a_deny_elsewhere_on_the_line_outranks_the_depth_ask() {
    let deep = nest_sh(MAX_SHELL_DEPTH + 1, PAYLOAD);
    let cmd = format!("{deep} ; rm -rf /home/someone");
    let d = analyse(&cmd);
    assert!(
        d.is_deny(),
        "a line that is BOTH depth-capped and clearly destructive must return \
         the DENY, not the depth Ask (Deny > Ask > Allow); got {d:?}\n  command: {cmd}"
    );
}

#[test]
fn a_deny_before_the_depth_capped_segment_also_wins() {
    let deep = nest_xargs(MAX_SHELL_DEPTH + 1, PAYLOAD);
    let cmd = format!("rm -rf /home/someone && {deep}");
    let d = analyse(&cmd);
    assert!(
        d.is_deny(),
        "order must not matter: the DENY wins whether it precedes or follows \
         the depth-capped segment; got {d:?}\n  command: {cmd}"
    );
}

/// Same property inside a SINGLE segment: the wrapper (`flock … -c '…'`)
/// re-analysis site is depth-capped here, while a later, non-recursive rule in
/// the same scan still sees the destructive `rm`. Recording the depth ask into
/// the accumulator keeps that Deny; returning it early would hide it.
#[test]
fn a_deny_from_a_later_rule_in_the_same_segment_survives_the_depth_cap() {
    let cmd = nest_sh(MAX_SHELL_DEPTH + 1, "flock /tmp/lock -c 'rm -rf /'");
    let d = analyse(&cmd);
    assert!(
        d.is_deny(),
        "the depth-capped wrapper site must not short-circuit past the rules \
         that follow it in the same segment scan; got {d:?}\n  command: {cmd}"
    );
}

// ---------------------------------------------------------------------------
// 5. The depth ask must be distinguishable from the budget ask.
// ---------------------------------------------------------------------------

/// A command that exhausts the NODE budget without ever nesting past the depth
/// cap: many SIBLING segments, each only three `find -exec` levels deep.
///
/// The obvious budget input (one line with 40 nested `-exec find .` tokens) is
/// *both* wide and deep, and since the depth cap started producing a verdict of
/// its own the depth ask is what surfaces on it — measured, and the reason the
/// lib-side `d4_exponential_find_exec_is_bounded_and_denies` assertion was
/// loosened. Breadth-without-depth is what still isolates the budget bound.
fn budget_hog() -> String {
    let unit = "find . -exec find . -exec find . -exec echo {} + + +";
    // The repeat count is calibrated, not arbitrary, and it moved once already
    // when `analyze_find` started stripping the `+`/`\;` terminator off an
    // -exec tail before re-analysing it (that token used to be re-analysed as
    // an operand, so each unit cost more nodes). Measured with the built hook
    // binary on this exact unit string:
    //
    //     100  -> ALLOW (silent)   <- analysis now COMPLETES; benign `echo`
    //     200  -> ask: "…too complex to analyse within the safety budget"
    //     400  -> ask: (same)
    //     1600 -> ask: (same)
    //
    // 300 keeps this off the boundary. If a future change makes the analyser
    // cheaper again, this test fails LOUDLY (the verdict becomes Allow) rather
    // than silently testing nothing — which is the property the count exists to
    // preserve. Re-measure and re-tune; do not weaken the assertions below.
    vec![unit; 300].join(" ; ")
}

/// The sibling condition. Both mean "analysis did not finish", but they are
/// different failures and their reasons must not be interchangeable — otherwise
/// a depth test can pass because the budget ran out instead.
#[test]
fn budget_exhaustion_and_depth_exhaustion_have_different_reasons() {
    let budget = analyse(&budget_hog());
    assert!(
        budget.is_blocking(),
        "control: budget exhaustion must block; got {budget:?}"
    );
    let budget_reason = reason_of(&budget);
    assert!(
        budget_reason.contains("budget"),
        "control: the budget ask should name the budget; got {budget_reason}"
    );

    let depth = analyse(&nest_sh(MAX_SHELL_DEPTH + 1, PAYLOAD));
    let depth_reason = reason_of(&depth);
    assert!(
        !depth_reason.is_empty() && DEPTH_WORDS.iter().any(|w| depth_reason.contains(w)),
        "control: the depth verdict should name the depth limit; got {depth_reason}"
    );
    assert_ne!(
        depth_reason, budget_reason,
        "the depth-cap verdict must not reuse the budget-exhaustion reason — a \
         reader (and this test suite) has to be able to tell which limit was hit"
    );
}

/// Coverage restored after loosening the lib-side D4 assertion.
///
/// `d4_exponential_find_exec_is_bounded_and_denies` used to be the only place
/// pinning the budget reason verbatim; its input is now answered by the depth
/// cap. Without this test, the budget bound would still exist in the code and be
/// pinned by nothing — the loosening would have quietly deleted coverage rather
/// than moved it. Both halves of the D4 contract are re-asserted here on an
/// input the depth cap cannot claim: it blocks, it blocks FAST, and it hardens
/// to the pre-existing deny.
#[test]
fn budget_bound_is_still_reachable_with_its_own_reason() {
    let cmd = budget_hog();
    let start = std::time::Instant::now();
    let d = analyse(&cmd);
    let elapsed = start.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "a PreToolUse hook must never hang the turn: analysis took {elapsed:?}"
    );
    assert_eq!(
        d.clone().hardened(),
        Decision::deny(
            "command is too complex to analyse within the safety budget — blastguard cannot vouch for it either way"
        ),
        "the budget bound must still be reachable from the public entry point, \
         with its own reason, and must still harden to a Deny"
    );
    // And it must be the BUDGET, not the depth cap wearing its clothes.
    assert!(
        !reason_of(&d).contains("recursion depth"),
        "this input is only 3 levels deep — a depth verdict here would mean the \
         depth cap is firing on nesting it should not see"
    );
}

/// The D4 budget-reset property, likewise re-anchored on an input that really
/// does exhaust the budget. (`d4_budget_resets_between_top_level_calls` in
/// `detect.rs` now blocks for the DEPTH reason on its input, so it no longer
/// demonstrates anything about the budget being reset.)
#[test]
fn budget_resets_between_top_level_calls_measured_on_a_budget_bound_input() {
    let hog = budget_hog();
    assert!(reason_of(&analyse(&hog)).contains("budget"));
    assert_allow("ls -la", "after budget exhaustion");
    assert_allow("cargo test", "after budget exhaustion");
    // And the hog is still judged the same way on a second call: the budget is
    // per-command, so exhausting it must not be a one-shot.
    assert!(reason_of(&analyse(&hog)).contains("budget"));
}
