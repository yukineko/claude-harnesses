// このファイルは丸ごと integration test なので expect/unwrap を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::expect_used)]
//! `> "$P"`: the redirect target that is a bare variable expansion.
//!
//! `detect_bash`'s redirect loop resolves such a token to the literal path an
//! assignment earlier on the SAME line gives it (`resolve_redirect_target`),
//! so that `P=/tmp/x.log; echo hi > "$P"` stops being refused while the
//! identical-effect `echo hi > /tmp/x.log` is allowed — one effect, two
//! verdicts, decided by spelling.
//!
//! Written by an agent that did not write that resolver (CLAUDE.md §2(a)).
//! The three halves are equally load-bearing and a change that makes one pass
//! by breaking another has not implemented the feature:
//!
//!   * `resolved_*` — the spelling no longer decides the verdict.
//!   * `unresolvable_*` — every shape where the shell would NOT use the
//!     readable value keeps the unresolved verdict. These are the fail-open
//!     probes: each assigns a HARMLESS confined path, so a resolver that
//!     believed it would answer `Allow`, and `Allow` is what the assertion
//!     forbids.
//!   * `decoy_*` — an earlier redirect through the SAME token must not decide
//!     the verdict for a later one. THESE CURRENTLY FAIL; see the module
//!     comment on `decoy_rebinding_must_not_excuse_the_later_redirect`.

use blastguard::detect;
use blastguard::model::Decision;
use blastguard::scope::SafeRoots;
use serde_json::json;

const PROJECT: &str = "/home/yuki/proj";
const HOME: &str = "/home/yuki";

/// A path inside the session's own tree that does NOT exist on the test
/// machine, so the recoverability probe answers `NothingToDestroy` and the
/// benign cases are decided by placement alone. Every "harmless value" below
/// is this one: if a resolver believes an assignment it should not, the
/// verdict visibly collapses to `Allow`.
const OK: &str = "/home/yuki/proj/ok.log";

/// Models a filesystem with no symlinks, exactly as `scoped_destructive.rs`
/// does: the LOCATION axis must be decided by the fixture, not by whatever
/// happens to be on this machine.
fn identity(p: &str) -> Option<String> {
    Some(p.to_string())
}

fn roots() -> SafeRoots {
    SafeRoots::new(
        Some(PROJECT),
        Some(PROJECT),
        Some(HOME),
        None,
        Some(identity),
    )
}

/// The hook's own entry point (it knows the session cwd). This is where the
/// resolution actually changes outcomes, so it is the default here.
fn scoped(cmd: &str) -> Decision {
    detect::detect_scoped("Bash", Some(&json!({ "command": cmd })), &roots())
}

/// The location-blind entry every unattended library consumer uses
/// (specguard's forge, condukt's check runner).
fn unscoped(cmd: &str) -> Decision {
    detect::detect("Bash", Some(&json!({ "command": cmd })))
}

fn reason_of(d: &Decision) -> &str {
    match d {
        Decision::Deny(r) | Decision::Ask(r) => r.as_str(),
        Decision::Allow => "",
    }
}

/// `Deny > Ask > Allow`, the ranking `model.rs` documents. Used to compare two
/// verdicts without pinning either one, so these tests survive the ongoing
/// deny-vs-ask retuning of the residual redirect verdict.
fn restrictiveness(d: &Decision) -> u8 {
    match d {
        Decision::Allow => 0,
        Decision::Ask(_) => 1,
        Decision::Deny(_) => 2,
    }
}

// ------------------------------------------------------------- resolved ----

/// The whole point of the feature, stated as an invariance rather than as a
/// verdict: naming the target through a variable the line itself assigns must
/// answer EXACTLY as naming it literally — same variant, same reason string.
///
/// Pinning the pair rather than the value is deliberate. The absolute verdict
/// for a given path is being retuned independently (the 2026-09-14 operator
/// ruling moved the residual redirect Deny to an Ask); what this feature
/// claims is that the two spellings stop disagreeing, and that claim is
/// checked here whatever the shared answer happens to be.
///
/// The dangerous pairs are in the SAME list on purpose: invariance is only
/// worth having if it holds in the restrictive direction too. If resolution
/// ever stopped feeding the protected-path and system-directory axes, those
/// rows would go `Ask`/`Deny` -> `Allow` and fail here.
#[test]
fn resolved_variable_spelling_answers_exactly_as_the_literal_spelling() {
    for (literal, via_variable) in [
        // benign, confined
        (
            "echo hi > /home/yuki/proj/ok.log",
            "P=/home/yuki/proj/ok.log; echo hi > \"$P\"",
        ),
        // unquoted use
        (
            "echo hi > /home/yuki/proj/ok.log",
            "P=/home/yuki/proj/ok.log; echo hi > $P",
        ),
        // braced use
        (
            "echo hi > /home/yuki/proj/ok.log",
            "P=/home/yuki/proj/ok.log; echo hi > \"${P}\"",
        ),
        // system directory: must stay a Deny, and by the axis that names it
        ("echo x > /etc/fstab", "P=/etc/fstab; echo x > \"$P\""),
        // protected gate/config path
        (
            "echo x > /home/yuki/.claude/settings.json",
            "CFG=/home/yuki/.claude/settings.json; echo x > \"$CFG\"",
        ),
        (
            "echo x > /home/yuki/proj/.githooks/pre-commit",
            "H=/home/yuki/proj/.githooks/pre-commit; echo x > \"$H\"",
        ),
        // someone else's tree
        (
            "echo x > /home/other/secret.txt",
            "P=/home/other/secret.txt; echo x > \"$P\"",
        ),
    ] {
        assert_eq!(
            scoped(literal),
            scoped(via_variable),
            "`{via_variable}` must answer exactly as `{literal}` — the spelling \
             of the target is not a property of the file it names"
        );
    }
}

/// The false positive the feature exists to remove, pinned as an absolute
/// value rather than a pair, so that "they agree" cannot be satisfied by both
/// sides regressing to a block.
#[test]
fn resolved_confined_target_is_not_blocked() {
    let cmd = "P=/home/yuki/proj/ok.log; echo hi > \"$P\"";
    assert_eq!(
        scoped(cmd),
        Decision::Allow,
        "a truncating redirect to a confined, non-existent path is ordinary \
         work whether it is spelled literally or through `$P`"
    );
}

/// Resolution must feed the axes that can NAME the hazard, not merely reach
/// the same verdict by accident: a resolved `/etc/fstab` has to be refused as
/// a system-directory write, which is a `Deny`, not the weaker residual `Ask`.
#[test]
fn resolved_system_directory_target_is_still_denied_by_name() {
    let d = scoped("P=/etc/fstab; echo x > \"$P\"");
    assert!(
        d.is_deny(),
        "a resolved system-directory redirect must stay a flat Deny, got {d:?}"
    );
    assert!(
        reason_of(&d).contains("/etc/fstab"),
        "the reason must name the resolved path, got {:?}",
        reason_of(&d)
    );
}

// --------------------------------------------------------- unresolvable ----

/// Every shape where the shell does NOT end up using the readable assignment —
/// or where reading it is a guess — must keep the unresolved verdict.
///
/// Each row assigns [`OK`], a harmless confined path: believing it yields
/// `Allow`, so `Allow` is exactly what a fail-open looks like here and exactly
/// what is asserted against. The second assertion is the sharper one — the
/// reason must not NAME [`OK`] — because a verdict that stops the call while
/// claiming the wrong file has still lost the fact it was asked for.
///
/// What the real shell does, verified with `bash -c` rather than assumed:
///
/// ```text
/// $ bash -c 'P=/tmp/bgA; echo a > "$P"; P=/tmp/bgB; echo b > "$P"'
///   -> /tmp/bgA holds `a`, /tmp/bgB holds `b`   (rebinding is believed)
/// $ bash -c 'RM=/bin/echo | echo "word=[$RM]"'   -> word=[]   (pipeline subshell)
/// $ bash -c 'RM=/bin/echo & echo "word=[$RM]"'   -> word=[]   (background subshell)
/// $ bash -c 'false && P=/tmp/x; echo "[$P]"'     -> []        (guarded)
/// ```
///
/// Two rows are deliberately STRICTER than bash, and are listed because the
/// error direction is the safe one: `{ P=…; }` (a brace group does run in the
/// current shell, so bash really would use the value) and `export P=…` (bash
/// sets it too). blastguard refuses both, which costs a resolution and invents
/// none — the direction CLAUDE.md §3 requires.
#[test]
fn unresolvable_shapes_keep_the_unresolved_verdict() {
    let mut violations: Vec<String> = Vec::new();
    for (cmd, why) in [
        (
            "P=/home/yuki/proj/ok.log; read P; echo hi > \"$P\"",
            "`read` rebinds the name from stdin, which the text does not contain",
        ),
        (
            "echo hi > \"$UNSET_EXTERNAL\"",
            "nothing on the line assigns the name at all",
        ),
        (
            "true && P=/home/yuki/proj/ok.log; echo hi > \"$P\"",
            "a guarded assignment runs or does not depending on an exit status",
        ),
        (
            "P=/home/yuki/proj/ok.log | echo hi > \"$P\"",
            "a pipeline stage assigns in its own subshell (measured: word=[])",
        ),
        (
            "P=/home/yuki/proj/ok.log & echo hi > \"$P\"",
            "a backgrounded segment assigns in its own subshell (measured: word=[])",
        ),
        (
            "( P=/home/yuki/proj/ok.log ); echo hi > \"$P\"",
            "an explicit subshell's assignment does not reach the parent shell",
        ),
        (
            "{ P=/home/yuki/proj/ok.log; }; echo hi > \"$P\"",
            "a brace group DOES reach the parent shell; refusing it is the safe error",
        ),
        (
            "export P=/home/yuki/proj/ok.log; echo hi > \"$P\"",
            "`export NAME=v` is not a pure assignment list to this scan",
        ),
        (
            "local P=/home/yuki/proj/ok.log; echo hi > \"$P\"",
            "`local` is a builtin, not an assignment list",
        ),
        (
            "declare P=/home/yuki/proj/ok.log; echo hi > \"$P\"",
            "`declare` is a builtin, not an assignment list",
        ),
        (
            "P=(/home/yuki/proj/ok.log); echo hi > \"$P\"",
            "an array's first element depends on IFS / [0]-vs-[@] / quoting",
        ),
        (
            "echo hi > \"${P:-/home/yuki/proj/ok.log}\"",
            "`${P:-default}` names the default only when P is unset — a run-time fact",
        ),
        (
            "echo hi > \"$P\"; P=/home/yuki/proj/ok.log",
            "the assignment is AFTER the redirect and cannot reach it",
        ),
        (
            "P=/home/yuki/proj/ok.log; echo hi > \"$P\"x",
            "the token is not a bare expansion — `$Px` is a different file",
        ),
        (
            "P=/home/yuki/proj/ok.log; echo hi > \"$P\"/../../../etc/fstab",
            "a suffixed expansion escapes the assigned directory",
        ),
        (
            "P=$OTHER; echo hi > \"$P\"",
            "the right-hand side is itself an expansion",
        ),
        (
            "P=; echo hi > \"$P\"",
            "an empty value makes the redirect target vanish",
        ),
        (
            "cat <<EOF\nP=/home/yuki/proj/ok.log\nEOF\necho hi > \"$P\"",
            "a NAME=value typed into a here-document BODY never ran",
        ),
        (
            "P=/home/yuki/proj/ok.log; unset P; echo hi > \"$P\"",
            "`unset` removes the value the scan can read",
        ),
    ] {
        // Collected rather than asserted row by row: a permissive resolver
        // usually breaks SEVERAL of these at once, and stopping at the first
        // one hides how wide the hole is.
        let d = scoped(cmd);
        if !d.is_blocking() {
            violations.push(format!("`{cmd}` -> {d:?} (must stop the call: {why})"));
            continue;
        }
        if !d.clone().hardened().is_deny() {
            violations.push(format!(
                "`{cmd}` -> {:?} with no human to answer (must harden to Deny: {why})",
                d.clone().hardened()
            ));
            continue;
        }
        // The RESOLVED spelling, not the bare path: `${P:-/home/yuki/proj/ok.log}`
        // legitimately carries the path inside the raw token it quotes back,
        // and quoting the token back is precisely the unresolved answer.
        if reason_of(&d).contains(&format!("'> {OK}'")) {
            violations.push(format!(
                "`{cmd}` reported the target as the resolved `{OK}` ({why})"
            ));
        }
    }
    assert!(
        violations.is_empty(),
        "{} shape(s) resolved to a value the shell would not use:\n  {}",
        violations.len(),
        violations.join("\n  ")
    );
}

// ----------------------------------------------------------------- decoy ----

/// ADVERSARIAL, AND CURRENTLY FAILING. A harmless redirect placed EARLIER on
/// the line, through the same variable token, must not decide the verdict for
/// a later redirect that the shell aims somewhere else.
///
/// `resolve_redirect_target` locates the token's segment with
/// `segs.iter().position(|s| redirect_targets(&s.text).iter().any(|t| t == target))`
/// — the FIRST segment whose redirect scan yields this token. `detect_bash`
/// then calls it once per target in `redirect_targets(cmd)`, and every
/// occurrence of the same token text gets the SAME first-segment answer. On a
/// line that rebinds the name between two redirects, the later redirect is
/// therefore judged as the earlier one's file.
///
/// The real shell, measured (not guessed):
///
/// ```text
/// $ bash -c 'P=/tmp/bgdemo/proj/notes.md; echo a > "$P"; \
///            P=/tmp/bgdemo/fakeetc/fstab; echo PWNED > "$P"'
/// $ cat /tmp/bgdemo/proj/notes.md      -> a
/// $ cat /tmp/bgdemo/fakeetc/fstab      -> PWNED     (was: ORIGINAL FSTAB)
/// ```
///
/// The token's FIRST occurrence is not the conservative choice the resolver's
/// doc comment claims. "Fewer assignments believed" is only restrictive for
/// the occurrence being judged; here it silently transplants one occurrence's
/// answer onto a different one.
#[test]
fn decoy_rebinding_must_not_excuse_the_later_redirect() {
    for (alone, with_decoy, hazard) in [
        (
            "P=/etc/fstab; echo b > \"$P\"",
            "P=/home/yuki/proj/ok.log; echo a > \"$P\"; P=/etc/fstab; echo b > \"$P\"",
            "/etc/fstab",
        ),
        (
            "P=/home/yuki/.claude/settings.json; echo b > \"$P\"",
            "P=/home/yuki/proj/ok.log; echo a > \"$P\"; \
             P=/home/yuki/.claude/settings.json; echo b > \"$P\"",
            "/home/yuki/.claude/settings.json",
        ),
        (
            "P=/home/yuki/proj/.githooks/pre-commit; echo b > \"$P\"",
            "P=/home/yuki/proj/ok.log; echo a > \"$P\"; \
             P=/home/yuki/proj/.githooks/pre-commit; echo b > \"$P\"",
            "/home/yuki/proj/.githooks/pre-commit",
        ),
    ] {
        let alone_d = scoped(alone);
        let decoy_d = scoped(with_decoy);
        assert!(
            restrictiveness(&decoy_d) >= restrictiveness(&alone_d),
            "adding a harmless earlier redirect through the same token weakened \
             the verdict: `{alone}` -> {alone_d:?} but `{with_decoy}` -> {decoy_d:?}"
        );
        assert!(
            reason_of(&decoy_d).contains(hazard),
            "the verdict for `{with_decoy}` must name `{hazard}` — the file the \
             shell actually truncates; got {:?}",
            reason_of(&decoy_d)
        );
    }
}

/// The same defect reached without a second redirect ON THE LINE being needed
/// as a decoy at all: a here-document BODY containing the token text supplies
/// the first `redirect_targets` hit, and the body is data the shell never
/// runs.
///
/// Measured, `bash -c` (the body line is printed, not executed, and the real
/// redirect still lands on the rebound path):
///
/// ```text
/// $ bash -c 'P=/tmp/bgdemo/proj/notes.md
///            cat <<'"'"'EOF'"'"'
///            echo x > "$P"
///            EOF
///            P=/tmp/bgdemo/fakeetc/fstab
///            echo PWNED-HEREDOC > "$P"'
/// echo x > "$P"
/// $ cat /tmp/bgdemo/fakeetc/fstab   -> PWNED-HEREDOC
/// ```
#[test]
fn decoy_inside_a_here_document_body_must_not_excuse_the_real_redirect() {
    let cmd = "P=/home/yuki/proj/ok.log\ncat <<'EOF'\necho x > \"$P\"\nEOF\n\
               P=/etc/fstab\necho pwn > \"$P\"";
    let d = scoped(cmd);
    assert!(
        d.is_deny(),
        "the only redirect this line runs truncates /etc/fstab; got {d:?}"
    );
    assert!(
        reason_of(&d).contains("/etc/fstab"),
        "the reason must name the file the shell truncates; got {:?}",
        reason_of(&d)
    );
}

/// The unattended path (`detect`, no location model) is where a wrong answer
/// is worst: specguard's forge and condukt's check runner hand the string to
/// `sh -c` with no human present, and `Decision::hardened` cannot rescue an
/// `Allow`.
///
/// Measured twice as the resolver's surrounding code moved, and it got worse,
/// which is why both assertions are here rather than only the second:
///
/// ```text
/// detect.rs ffbf310e -> Ask("'> /tmp/x.log' … does not exist yet …")
///                       blocks, but names the DECOY's file, not /etc/fstab
/// detect.rs c702be8a -> Allow
/// ```
#[test]
fn decoy_rebinding_does_not_misname_the_target_for_unattended_consumers() {
    let cmd = "P=/tmp/x.log; echo a > \"$P\"; P=/etc/fstab; echo b > \"$P\"";
    let d = unscoped(cmd);
    assert!(
        d.is_blocking(),
        "`{cmd}` truncates /etc/fstab and reaches `sh -c` with no human present; \
         got {d:?}"
    );
    assert!(
        reason_of(&d).contains("/etc/fstab"),
        "the reason must name the file the shell truncates, not the decoy; got {:?}",
        reason_of(&d)
    );
}
