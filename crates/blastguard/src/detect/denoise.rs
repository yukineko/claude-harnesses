//! De-noised rescan of shell-eval regions (99b506b7), moved verbatim out of
//! `detect.rs` (file-growth ratchet).

use super::{harm_mark, Ctx, Decision, VerdictAcc};

/// A destructive command word can be hidden from `-c` payload extraction by
/// piling up backslash-escaped quotes until `first_shell_word`'s faithful POSIX
/// unquoting truncates the payload BEFORE the command word. 99b506b7 (verified
/// bypass): naive hand-nesting `sh -c "sh -c \"sh -c \\"rm -rf /\\"\""` accretes
/// `\\"` runs that `first_shell_word` reads as escaped-backslash-then-closing-
/// quote, so the extracted payload stops at `sh -c "sh -c \` and `rm -rf /` is
/// dropped; the trailing tokens are then treated as the shell's positional
/// operands (`$0`,`$1`,…), not commands, so no candidate normalises to `rm` and
/// the whole line went ALLOW from depth 4 down. The structured extraction cannot
/// be made to recover an *ambiguously* over-escaped payload without also
/// mis-parsing the benign twin, so this is a WIDENING backstop rather than a
/// smarter parser: strip the escape punctuation outright and re-scan EVERY
/// resulting position with the full rule engine.
///
/// Scope and trigger keep it from touching ordinary work:
///   * It is applied ONLY to shell-EVAL regions (the `-c`/eval operand, which is
///     genuinely code), never to plain operands — so a quoted DATA string like
///     `grep -rn 'rm -rf' src/` (not an eval region) is never de-noised.
///   * It fires ONLY when the region actually carries a backslash, which is the
///     construct that defeats `first_shell_word`. A single-/double-quoted data
///     string with no backslash (`sh -c "git commit -m 'rm -rf x'"`) is left on
///     its exact previous path, so the fix adds no new false positive there.
///
/// When it does fire on a backslash-escaped region whose de-noised form contains
/// a destructive command word (e.g. `sh -c "echo \"rm -rf /\""`, where the
/// string is only echoed), the deny is a false positive — but a recoverable one,
/// and the documented acceptable side of this module's fail-closed bias for
/// shell-eval wrappers, identical to the `dash_c_payloads`/`command_candidates`
/// widening. Benign eval regions with no destructive word (`echo hi` nested to
/// any depth) de-noise to a stream that matches no rule arm and stay ALLOW.
pub(super) fn denoised_eval_rescan(region: &str, depth: usize, ctx: &Ctx<'_>) -> Decision {
    // Only backslash escaping can defeat `first_shell_word` into truncating a
    // payload; without it the structured extraction already sees the region
    // correctly, so skip to preserve exact prior behaviour (and avoid de-noising
    // benign quoted data).
    if !region.contains('\\') {
        return Decision::Allow;
    }
    // Strip the escape/quote punctuation so an over-escaped command word
    // (`\"rm`, `\\"rm`, `r""m`) collapses back to the bare program name, while
    // whitespace that separated real words is preserved.
    let denoised: String = region
        .chars()
        .filter(|c| !matches!(c, '\\' | '"' | '\''))
        .collect();
    if denoised == region {
        return Decision::Allow;
    }
    let tokens: Vec<&str> = denoised.split_whitespace().collect();
    let mut acc = VerdictAcc::default();
    // Every position is a candidate command word (deny-if-ANY), exactly like the
    // exec-wrapper widening in `command_candidates`: whichever position the real
    // shell would exec is guaranteed to be among them.
    for idx in 0..tokens.len() {
        if let Some(deny) = acc.record(harm_mark::analyze_command_at(
            &tokens, idx, depth, None, ctx,
        )) {
            return deny;
        }
    }
    acc.finish()
}
