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
//!
//! TWO ROWS WERE RE-EXAMINED on 2026-10-02, by an agent that wrote neither this
//! file nor the suffix resolver that exposed them. Both sat in the
//! `unresolvable_*` list, and both were written when NO suffixed target
//! resolved, so neither expectation had ever been separable from "the resolver
//! does not reach this shape". Measured against bash, each justification turned
//! out to be a true statement about a DIFFERENT token than the row held — but
//! only ONE of the two rows was actually wrong:
//!
//!   * `> "$P"x` was justified by "`$Px` is a different file". True of the
//!     unquoted `$Px`; false of `"$P"x`, where the closing quote ends the name.
//!     The row WAS wrong: moved to the resolved list, and the unquoted spelling
//!     took its place in the unresolvable list, where that justification is the
//!     measured truth.
//!   * `> "$P"/../../../etc/fstab` was justified by "a suffixed expansion
//!     escapes the assigned directory". That reason does not hold either — the
//!     text lands on `/home/etc/fstab`, which this gate Allows when spelled
//!     literally — but the row's EXPECTATION was right for a reason nobody had
//!     written down, so it stays. See the retraction below.
//!
//! The lesson is the group's own criterion, applied to itself: a row belongs in
//! `unresolvable_*` only when the shell would NOT use the readable value, or
//! when reading it is a guess. "The resolver happens not to handle this
//! spelling" is neither. Note the second row shows the converse trap too — a
//! right expectation resting on a wrong reason is still a liability, because the
//! next reader checks the reason.
//!
//! ONE OF THOSE TWO CORRECTIONS WAS ITSELF WRONG, and the retraction belongs
//! here rather than in a commit message nobody will read again. The row
//!
//!     P=/home/yuki/proj/ok.log; echo hi > "$P"/../../../etc/fstab
//!
//! was briefly replaced by a test asserting pair-equality with the literal, on
//! the ground that `..` is purely lexical and that `normalize_abs` plus
//! `no_symlink_below` already judge both spellings alike. The arithmetic and
//! the `normalize_abs` reading were right; the conclusion was wrong, because
//! `..` is NOT purely lexical to the kernel. `open()` resolves `..` against the
//! directory a component REALLY is, so a single symlink before a `..` makes the
//! text name one file and the syscall open another — and `no_symlink_below`
//! cannot see it, because `normalize_abs` deletes the symlink component before
//! the walk begins. Measured, and reproduced independently on both the deployed
//! 0.2.97 and this worktree's build:
//!
//!     printf WRITTEN > <proj>/lnk/../victim.txt        ( lnk -> other/deep )
//!       lexical parent <proj>/victim.txt       -> LEXICAL-PARENT-UNTOUCHED
//!       the write hit  <proj>/other/victim.txt -> WRITTEN
//!     : > <proj>/esc/../../etc/fstab   -> ALLOW   ( esc -> `/` ; truncates
//!                                                   /etc/fstab for real )
//!     : > <proj>/esc/etc/fstab         -> DENY    ( no `..`, so the symlink
//!                                                   component survives )
//!
//! The row is therefore RESTORED to the `unresolvable_*` list, with the
//! justification it should always have carried. The group's criterion is
//! satisfied after all — "reading it is a guess" — just not for the reason the
//! original annotation gave.
//!
//! Note what the fixture can and cannot show: the `identity` resolver below
//! models "no symlinks", so pair-equality for a `..` suffix is SATISFIABLE here
//! and the retracted test was honest about what it measured. It was still the
//! wrong rule to pin, because the property that decides it is invisible to this
//! fixture. A test for that property needs real symlinks and the real resolver
//! — see `redirect_target_suffix_resolution.rs`'s `symlink_*` group, which
//! spawns the binary the way `worktree_root_symlink_dotdot.rs` does. The
//! literal and bare-`> $P` spellings of the same escape are still Allow and are
//! filed as backlog 3ca56588 (p1); refusing the suffix does not close them.

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
        // A LITERAL SUFFIX that does not begin with `/`. Corrected on
        // 2026-10-02: this shape used to sit in the `unresolvable_*` list
        // justified as "`$Px` is a different file", which is a true statement
        // about the UNQUOTED `$Px` and a false one about the quoted `"$P"x`.
        // The closing quote ends the name, so the value IS used. Measured:
        //   bash -c 'P=<dir>/ok.log; printf ORIG > $P; printf NEW > "$P"x'
        //     -> rc=0, ok.log still ORIG, ok.logx created holding NEW
        //   bash -c 'P=/aa; Px=/bb; printf "%s %s" $Px "$P"x'  ->  /bb /aax
        // The unquoted spelling kept its row in the unresolvable list, where
        // its justification is the measured truth.
        (
            "echo hi > /home/yuki/proj/ok.logx",
            "P=/home/yuki/proj/ok.log; echo hi > \"$P\"x",
        ),
        (
            "echo hi > /home/yuki/proj/ok.logx",
            "P=/home/yuki/proj/ok.log; echo hi > ${P}x",
        ),
        // …and the same shape in the RESTRICTIVE direction, so the rows above
        // cannot be satisfied by a resolver that merely collapses everything to
        // Allow. `.bak`/`.tmp`/`.1` siblings are the reason this shape matters
        // at all, and a sibling of a system file is still a system file.
        // Measured: `P=/etc/fstab; printf "%s" "$P".bak` -> /etc/fstab.bak and
        // `P=/etc/fsta; printf "%s" "$P"b` -> /etc/fstab.
        (
            "echo x > /etc/fstab.bak",
            "P=/etc/fstab; echo x > \"$P\".bak",
        ),
        ("echo x > /etc/fstab", "P=/etc/fsta; echo x > \"$P\"b"),
        (
            "echo x > /home/yuki/.claude/settings.json",
            "P=/home/yuki/.claude/settings; echo x > \"$P\".json",
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
        // These two rows replace a pair that was CORRECTED on 2026-10-02 (see
        // the module header). The claim "`$Px` is a different file" is true —
        // but only of the UNQUOTED spelling, which is what now stands here.
        // Measured side by side on one line, so the two cannot be conflated:
        //   bash -c 'P=/aa; Px=/bb; printf "%s %s" $Px "$P"x'  ->  /bb /aax
        (
            "P=/home/yuki/proj/ok.log; echo hi > $Px",
            "unquoted: the name continues through `x`, so this references `Px` \
             — a different variable, unset here (measured: expands to nothing)",
        ),
        (
            "P=/home/yuki/proj/ok.log; echo hi > $P_x",
            "unquoted: `_` continues an identifier too, so this references `P_x`",
        ),
        // RESTORED on 2026-10-02 after the correction below was itself shown to
        // be wrong, and with the justification the row should always have had.
        // `..` is not unreadable because it is unreadable — it is unreadable
        // because ONE SYMLINK anywhere before it makes the concatenated text
        // name a different file than the kernel opens. Measured
        // non-destructively (`lnk` -> a real dir inside the project):
        //   printf WRITTEN > <proj>/lnk/../victim.txt
        //     lexical parent <proj>/victim.txt       -> LEXICAL-PARENT-UNTOUCHED
        //     the write hit  <proj>/other/victim.txt -> WRITTEN
        // and with `esc` -> `/`, realpath(<proj>/esc/../../etc/fstab) is
        // `/etc/fstab` while lexical normalisation gives `<base>/src/etc/fstab`.
        // `no_symlink_below` cannot save this: `normalize_abs` DELETES the
        // symlink component before the walk ever sees it. Measured on the
        // deployed 0.2.97 and on this worktree's build, session id stripped:
        //   : > <proj>/esc/../../etc/fstab   -> ALLOW   (truncates /etc/fstab)
        //   : > <proj>/esc/etc/fstab         -> DENY    (component survives)
        // The literal and BARE `> $P` spellings are both still Allow — filed as
        // backlog 3ca56588 (p1), NOT closed by refusing the suffix. When that
        // closes, this row is the one to revisit.
        (
            "P=/home/yuki/proj; echo hi > \"$P\"/../../../etc/fstab",
            "a `..` in the suffix is resolved by the KERNEL against what each \
             component really is, so one symlink makes the concatenated text \
             name a different file (measured)",
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
