// このファイルは丸ごと integration test なので expect/unwrap を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
// `unwrap_used`/`panic` are allowed for the `symlink_*` group's on-disk fixture
// setup, the same carve-out `worktree_root_symlink_dotdot.rs` takes: a fixture
// that cannot be built has nothing to assert about.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! `> "$P"/suffix`: the redirect target that is a variable expansion followed
//! by a LITERAL path suffix.
//!
//! `tests/redirect_target_resolution.rs` pins the BARE case: `> "$P"` is
//! resolved to the path an assignment earlier on the line gives it, so that
//! `P=/tmp/x.log; : > "$P"` answers as `: > /tmp/x.log` does. This file pins
//! the case one character longer, which that resolver does not reach:
//! `resolve_redirect_target_at`'s first line is `referenced_variable_name(target)?;`
//! and `referenced_variable_name` requires the ENTIRE word to be `$NAME`,
//! `${NAME}` or `"$NAME"` — "any word with text glued around the reference
//! (`pre$BIN`, `$A$B`) … the value is either not a variable or not just a
//! variable", as its own doc comment puts it.
//!
//! Measured against blastguard 0.2.97 (2026-10-02, the fixture below):
//!
//! ```text
//! P=/etc/fstab; : > $P      -> Deny, names the SYSTEM DIRECTORY axis
//! P=/etc/fstab; : > ${P}    -> Deny, system directory
//! P=/etc/fstab; : > "$P"    -> Deny, system directory
//! P=/etc; : > $P/fstab      -> Deny, GENERIC "contains a shell expansion"
//! P=/etc; : > ${P}/fstab    -> Deny, generic
//! P=/etc; : > "$P"/fstab    -> Deny, generic
//! P=/etc; : > "$P/fstab"    -> Deny, generic
//! P=/home/yuki/proj/ok.log; : > $P        -> ALLOW
//! P=/home/yuki/proj; : > $P/absent.log    -> Deny, generic
//! ```
//!
//! The last two rows are the user-visible defect: one filesystem effect, two
//! verdicts, decided by whether a suffix follows the reference. The `/etc`
//! rows are the other half of the same defect and the reason this is not a
//! pure false-positive ticket — the hazard IS caught today, but by the axis
//! that says "I cannot see which file this is", not by the one that can name
//! `/etc/fstab`. A reason that does not name the file it refuses is a worse
//! answer even when the verdict letter is the same (CLAUDE.md §4).
//!
//! Written by an agent that did not implement the resolver (CLAUDE.md §2(a)).
//! The table above and the `resolved_*`/`decoy_*` notes below record the state
//! BEFORE the fix, which is what these tests were written against; the fix
//! landed in 0.2.98 and the whole file now passes. The five parts are equally
//! load-bearing and an implementation that makes one pass by breaking another
//! has not implemented the feature:
//!
//!   * `resolved_*` — the suffix stops deciding the verdict. These were the
//!     RED this file was written to record; they are green as of 0.2.98.
//!   * `unresolvable_*` — every shape where the shell would NOT use the
//!     readable value keeps the restrictive verdict. These are the fail-open
//!     probes: each assigns a HARMLESS confined path, so a resolver that
//!     believed it would answer `Allow`, and `Allow` is what the assertion
//!     forbids. They passed before the fix and must keep passing after it.
//!   * `control_*` — the bare `$P` / `${P}` / `"$P"` cases that already worked.
//!     The suffix case must not be fixed by regressing them.
//!   * `decoy_*` — per-occurrence correctness for suffixed targets. Was RED
//!     (nothing resolved, so the reason named the FIRST target's token and
//!     never the file the shell truncates); green as of 0.2.98.
//!   * `symlink_*` — the one property this file's `identity` resolver CANNOT
//!     observe, so it is measured against real symlinks and the real binary.
//!     Added after the author of this file reached a wrong conclusion from the
//!     no-symlink model; see that section's own comment, which is the honest
//!     record of the mistake.

use blastguard::detect;
use blastguard::model::Decision;
use blastguard::scope::SafeRoots;
use serde_json::json;

const PROJECT: &str = "/home/yuki/proj";
const HOME: &str = "/home/yuki";

/// A DIRECTORY inside the session's own tree, used as the harmless value in
/// every `unresolvable_*` row. Neither it nor anything under it exists on the
/// test machine, so the recoverability probe answers `NothingToDestroy` and the
/// placement axes alone decide — which for this tree means `Allow`. Verified
/// against the literal spelling by [`control_the_harmless_value_really_is_allowed_when_written_literally`]:
/// if a resolver believes an assignment it should not, the verdict visibly
/// collapses to `Allow` and the assertions catch it.
const OK_DIR: &str = "/home/yuki/proj";

/// The leaf under [`OK_DIR`] that the harmless rows aim at.
const OK_LEAF: &str = "/home/yuki/proj/x.log";

/// The fragment `reversible::probe` emits when it is handed a target it cannot
/// read as a path — "`{target}` contains a shell expansion, so blastguard
/// cannot tell which file this names". It is the verdict every suffixed
/// redirect gets today, and the `resolved_*` tests assert its ABSENCE: a
/// resolved target must be judged by an axis that knows the path, not by the
/// one that admits it does not.
const GENERIC_EXPANSION: &str = "contains a shell expansion";

/// Models a filesystem with no symlinks, exactly as `redirect_target_resolution.rs`
/// and `scoped_destructive.rs` do: the LOCATION axis must be decided by the
/// fixture, not by whatever happens to be on this machine.
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

/// The feature, stated as an invariance rather than as a verdict: naming the
/// target through a variable the line itself assigns PLUS a literal suffix
/// must answer EXACTLY as naming the concatenation literally — same variant,
/// same reason string.
///
/// Pinning the pair rather than the value is deliberate, for the reason the
/// bare-case file gives: the absolute verdict for a given path is being
/// retuned independently, while what this feature claims is that the two
/// spellings stop disagreeing.
///
/// Both directions are in the SAME list on purpose. The `/etc` rows go
/// Deny(generic) -> Deny(system directory) and the project rows go Deny ->
/// Allow; an implementation that only moves one of them has not made the
/// spelling irrelevant, it has only moved the disagreement.
///
/// Every spelling of the reference is exercised against the same suffix,
/// because `referenced_variable_name` accepts four forms and a fix that
/// handles only the unbraced one leaves three holes:
/// `$P/s`, `${P}/s`, `"$P"/s` and the fully-quoted `"$P/s"`.
#[test]
fn resolved_suffixed_variable_answers_exactly_as_the_literal_spelling() {
    let mut violations: Vec<String> = Vec::new();
    for (literal, via_variable) in [
        // ---- dangerous: must stay blocked, by the axis that NAMES the file
        ("echo x > /etc/fstab", "P=/etc; echo x > $P/fstab"),
        ("echo x > /etc/fstab", "P=/etc; echo x > ${P}/fstab"),
        ("echo x > /etc/fstab", "P=/etc; echo x > \"$P\"/fstab"),
        ("echo x > /etc/fstab", "P=/etc; echo x > \"$P/fstab\""),
        // multi-segment suffix, still a system directory
        (
            "echo x > /etc/sudoers.d/evil",
            "P=/etc; echo x > $P/sudoers.d/evil",
        ),
        // protected gate/config paths. The protected-path axis already
        // substring-matches the RAW token today (measured: the Ask for
        // `P=/home/yuki; : > $P/.claude/settings.json` names
        // `$P/.claude/settings.json`), so the verdict letter already agrees —
        // what does not agree is the PATH in the reason, which is the half a
        // human acts on.
        (
            "echo x > /home/yuki/.claude/settings.json",
            "P=/home/yuki; echo x > $P/.claude/settings.json",
        ),
        (
            "echo x > /home/yuki/proj/.githooks/pre-commit",
            "P=/home/yuki/proj; echo x > $P/.githooks/pre-commit",
        ),
        // ---- benign: the false positive the feature exists to remove
        (
            "echo hi > /home/yuki/proj/absent.log",
            "P=/home/yuki/proj; echo hi > $P/absent.log",
        ),
        (
            "echo hi > /home/yuki/proj/absent.log",
            "P=/home/yuki/proj; echo hi > ${P}/absent.log",
        ),
        (
            "echo hi > /home/yuki/proj/absent.log",
            "P=/home/yuki/proj; echo hi > \"$P\"/absent.log",
        ),
        (
            "echo hi > /home/yuki/proj/absent.log",
            "P=/home/yuki/proj; echo hi > \"$P/absent.log\"",
        ),
        // multi-segment suffix
        (
            "echo hi > /home/yuki/proj/sub/dir/deep.log",
            "P=/home/yuki/proj; echo hi > $P/sub/dir/deep.log",
        ),
        // a suffix carrying a dot and a dash, the two characters most likely
        // to be mis-tokenised by a hand-rolled boundary scan
        (
            "echo hi > /home/yuki/proj/my-file.v2.log",
            "P=/home/yuki/proj; echo hi > $P/my-file.v2.log",
        ),
        // a trailing slash on the VALUE. No normalisation is demanded here:
        // measured, `echo hi > /home/yuki/proj//trail.log` and the single-slash
        // spelling are both `Allow`, so a naive `value + suffix` concatenation
        // satisfies this row. It is listed to pin that the doubled slash does
        // not knock the target out of the session tree. (`//` is NOT collapsed
        // in reason strings — `echo x > /etc//fstab` reports `/etc//fstab` —
        // so do not add a doubled-slash row whose verdict is a Deny/Ask without
        // deciding normalisation first.)
        (
            "echo hi > /home/yuki/proj/trail.log",
            "P=/home/yuki/proj/; echo hi > $P/trail.log",
        ),
        // someone else's home. Allowed literally in this fixture (the file does
        // not exist, so there are no bytes to lose) and Denied through the
        // variable today — the same spelling-decides-the-verdict split, in the
        // direction that costs a human a prompt.
        (
            "echo x > /home/other/secret.txt",
            "P=/home/other; echo x > $P/secret.txt",
        ),
    ] {
        // Collected rather than asserted row by row: a partial fix usually
        // leaves SEVERAL of these standing, and stopping at the first one hides
        // how much of the feature is missing.
        let (lit, var) = (scoped(literal), scoped(via_variable));
        if lit != var {
            violations.push(format!(
                "`{via_variable}` -> {var:?}\n       but `{literal}` -> {lit:?}"
            ));
        }
    }
    assert!(
        violations.is_empty(),
        "{} spelling(s) still decide the verdict — the suffix after a resolvable \
         reference is not a property of the file it names:\n  {}",
        violations.len(),
        violations.join("\n  ")
    );
}

/// Resolution must feed the axes that can NAME the hazard, not merely reach the
/// same verdict letter by accident.
///
/// This is the assertion the pair test above cannot make on its own: today
/// `P=/etc; : > $P/fstab` and `: > /etc/fstab` are BOTH `Deny`, so a test that
/// only compared `is_deny()` would already pass while the reason a human reads
/// still says "blastguard cannot tell which file this names". The generic text
/// is therefore asserted ABSENT, and the system-directory text asserted
/// present, for every spelling.
#[test]
fn resolved_system_directory_suffix_is_denied_by_the_system_directory_axis() {
    let literal = scoped("echo x > /etc/fstab");
    assert!(
        literal.is_deny(),
        "fixture precondition: the literal `/etc/fstab` write must be a Deny, got {literal:?}"
    );
    assert!(
        reason_of(&literal).contains("system directory"),
        "fixture precondition: the literal Deny must come from the system-directory \
         axis, got {:?}",
        reason_of(&literal)
    );

    for cmd in [
        "P=/etc; echo x > $P/fstab",
        "P=/etc; echo x > ${P}/fstab",
        "P=/etc; echo x > \"$P\"/fstab",
        "P=/etc; echo x > \"$P/fstab\"",
    ] {
        let d = scoped(cmd);
        assert!(
            d.is_deny(),
            "`{cmd}` truncates /etc/fstab and must stay a flat Deny, got {d:?}"
        );
        assert!(
            reason_of(&d).contains("/etc/fstab"),
            "the reason for `{cmd}` must name the resolved path `/etc/fstab`, got {:?}",
            reason_of(&d)
        );
        assert!(
            !reason_of(&d).contains(GENERIC_EXPANSION),
            "the reason for `{cmd}` still admits it could not read the target \
             ({GENERIC_EXPANSION:?}) — resolution did not reach the axes; got {:?}",
            reason_of(&d)
        );
        assert_eq!(
            reason_of(&d),
            reason_of(&literal),
            "`{cmd}` must be refused with the SAME reason as the literal spelling"
        );
    }
}

/// The false positive, pinned as an absolute value rather than a pair, so that
/// "they agree" cannot be satisfied by both sides regressing to a block.
#[test]
fn resolved_confined_suffix_target_is_not_blocked() {
    for cmd in [
        "P=/home/yuki/proj; echo hi > $P/absent.log",
        "P=/home/yuki/proj; echo hi > ${P}/absent.log",
        "P=/home/yuki/proj; echo hi > \"$P\"/absent.log",
        "P=/home/yuki/proj; echo hi > \"$P/absent.log\"",
        "P=/home/yuki/proj; echo hi > $P/sub/dir/deep.log",
        "P=/home/yuki/proj; echo hi > $P/my-file.v2.log",
    ] {
        assert_eq!(
            scoped(cmd),
            Decision::Allow,
            "a truncating redirect to a confined, non-existent path is ordinary \
             work whether the directory is spelled literally or through `$P`: `{cmd}`"
        );
    }
}

/// The unattended entry (`detect`, no location model) must gain the same
/// resolution. specguard's forge and condukt's check runner hand the string to
/// `sh -c` with no human present, so an unnamed hazard there cannot be
/// escalated to anyone.
#[test]
fn resolved_system_directory_suffix_is_named_for_unattended_consumers() {
    let cmd = "P=/etc; echo x > $P/fstab";
    let d = unscoped(cmd);
    assert!(
        d.is_blocking(),
        "`{cmd}` truncates /etc/fstab and reaches `sh -c` with no human present; got {d:?}"
    );
    assert!(
        reason_of(&d).contains("/etc/fstab"),
        "the reason must name the file the shell truncates; got {:?}",
        reason_of(&d)
    );
}

// --------------------------------------------------------------- control ----

/// The harmless value's own verdict, asserted so the `unresolvable_*` rows
/// below are known to be real fail-open probes rather than rows that would say
/// "not Allow" whatever happened. If this test ever starts failing, every
/// `unresolvable_*` assertion has quietly stopped testing anything.
#[test]
fn control_the_harmless_value_really_is_allowed_when_written_literally() {
    assert_eq!(
        scoped(&format!("echo hi > {OK_LEAF}")),
        Decision::Allow,
        "the `unresolvable_*` rows assign `{OK_DIR}` precisely because writing \
         `{OK_LEAF}` literally is Allowed — a credulous resolver's answer is \
         therefore visibly `Allow`"
    );
}

/// The BARE cases the existing resolver already handles. Pinned here as well as
/// in `redirect_target_resolution.rs` because this file's feature is one
/// `referenced_variable_name` call away from them: a fix that widens that
/// function (rather than adding a suffix split around it) can break the bare
/// forms, and "the suffix case now works" is not the feature if it cost these.
#[test]
fn control_bare_variable_targets_keep_resolving() {
    for cmd in [
        "P=/home/yuki/proj/ok.log; echo hi > $P",
        "P=/home/yuki/proj/ok.log; echo hi > ${P}",
        "P=/home/yuki/proj/ok.log; echo hi > \"$P\"",
        "P=/home/yuki/proj/ok.log; echo hi > \"${P}\"",
    ] {
        assert_eq!(
            scoped(cmd),
            scoped("echo hi > /home/yuki/proj/ok.log"),
            "`{cmd}` is the already-working bare case and must keep answering as \
             the literal spelling"
        );
    }
    for cmd in [
        "P=/etc/fstab; echo x > $P",
        "P=/etc/fstab; echo x > ${P}",
        "P=/etc/fstab; echo x > \"$P\"",
    ] {
        let d = scoped(cmd);
        assert!(d.is_deny(), "`{cmd}` must stay a flat Deny, got {d:?}");
        assert!(
            reason_of(&d).contains("/etc/fstab") && reason_of(&d).contains("system directory"),
            "`{cmd}` must keep being refused by the system-directory axis, by name; \
             got {:?}",
            reason_of(&d)
        );
        assert!(
            !reason_of(&d).contains(GENERIC_EXPANSION),
            "`{cmd}` regressed to the unreadable-target verdict; got {:?}",
            reason_of(&d)
        );
    }
}

// --------------------------------------------------------- unresolvable ----

/// Every shape where the shell does NOT end up using the readable assignment —
/// or where reading it is a guess — must keep today's restrictive verdict.
///
/// Each row assigns [`OK_DIR`] and aims at [`OK_LEAF`]: believing the
/// assignment yields `Allow` (pinned by
/// [`control_the_harmless_value_really_is_allowed_when_written_literally`]), so
/// `Allow` is exactly what a fail-open looks like here and exactly what is
/// asserted against. The third assertion is the sharper one — the reason must
/// not report the target AS [`OK_LEAF`] — because a verdict that stops the call
/// while claiming the wrong file has still lost the fact it was asked for, and
/// a half-resolving implementation (split at the first `/`, resolve the head,
/// leave an expansion in the tail) would otherwise sail through the first two.
///
/// What the real shell does, measured with `bash -c` on this machine
/// (2026-10-02, GNU bash 5.x under WSL2) rather than assumed:
///
/// ```text
/// $ bash -c 'cd SB && P=dir : > $P/pfx.log; echo "exit=$?"'
///     bash: line 1: /pfx.log: Permission denied        exit=1
/// $ bash -c 'P=dir echo "word=[$P]"'          -> word=[]
/// $ bash -c 'P=dir true; echo "later=[$P]"'   -> later=[]
///     ^ a command PREFIX does not bind the name, for the use in its own
///       segment nor for any later one.
/// $ bash -c 'false && P=dir; echo "[$P]"'     -> []
/// $ bash -c 'true || P=dir; echo "[$P]"'      -> []
/// $ bash -c 'true && P=dir; echo "[$P]"'      -> [dir]     (bash DOES bind)
/// $ bash -c 'P=(aa bb); echo "[$P]"'          -> [aa]
/// $ bash -c 'P=/aa; P+=/bb; echo "[$P]"'      -> [/aa/bb]
/// $ bash -c 'export P=/aa; echo "[$P]"'       -> [/aa]     (bash DOES bind)
/// $ bash -c 'P=/aa; unset P; echo "[$P]"'     -> []
/// $ bash -c 'P=/aa; eval "P=/bb"; echo "[$P]"'-> [/bb]
/// $ bash -c 'P=; echo "[$P/x.log]"'           -> [/x.log]  (at the ROOT)
/// $ bash -c 'cat <<EOF
///            P=/aa
///            EOF
///            echo "[$P]"'                     -> P=/aa then []  (body is data)
/// $ bash -c 'P=/aa; echo "[$P/$Q]"'           -> [/aa/]
/// $ bash -c 'P=/aa; echo "[pre$P/x]"'         -> [pre/aa/x]
/// $ bash -c 'echo "[${P:-/dd}/x]"'            -> [/dd/x]
/// $ bash -c 'echo "[$1/x]"'                   -> [/x]      (at the ROOT)
/// $ bash -c 'echo "[$$/x]"'                   -> [<pid>/x] (RELATIVE)
/// ```
///
/// Four rows are deliberately STRICTER than bash, and are listed because the
/// error direction is the safe one — each costs a resolution and invents none,
/// the direction CLAUDE.md §3 requires:
///
///   * `true && P=…` — bash binds it; blastguard refuses because the text does
///     not say the guard succeeded.
///   * `export P=…` and `declare P=…` — bash binds them; the scan does not
///     read declaration builtins.
///   * `{ P=…; }` — a brace group runs in the current shell, so bash really
///     would use the value.
///
/// The `$$`/`$1` rows are stricter in a different and sharper sense: bash's
/// answers there (`<pid>/x`, `/x`) are not merely unknown, they are a RELATIVE
/// path and a write at the filesystem root. Resolving either to something under
/// the session tree would be an invention, not a guess.
#[test]
fn unresolvable_suffixed_shapes_keep_the_unresolved_verdict() {
    let mut violations: Vec<String> = Vec::new();
    // (command, the value a credulous resolver would report, why it is wrong)
    // An empty second field means "no assignment exists to be misread".
    for (cmd, credulous, why) in [
        (
            "echo hi > $P/x.log",
            "",
            "nothing on the line assigns the name at all",
        ),
        (
            "echo hi > $P/x.log; P=/home/yuki/proj",
            "",
            "the assignment is AFTER the redirect and cannot reach it",
        ),
        (
            "P=/home/yuki/proj echo hi > $P/x.log",
            OK_LEAF,
            "a command PREFIX does not bind the name for its own segment \
             (measured: word=[], target became `/pfx.log`)",
        ),
        (
            "P=/home/yuki/proj true; echo hi > $P/x.log",
            OK_LEAF,
            "a command prefix is transient (measured: later=[])",
        ),
        (
            "true && P=/home/yuki/proj; echo hi > $P/x.log",
            OK_LEAF,
            "a guarded assignment runs or does not depending on an exit status; \
             refusing it is the safe error (bash: [dir])",
        ),
        (
            "false && P=/home/yuki/proj; echo hi > $P/x.log",
            OK_LEAF,
            "measured: the && guard failed and the name stayed empty",
        ),
        (
            "true || P=/home/yuki/proj; echo hi > $P/x.log",
            OK_LEAF,
            "measured: the || guard short-circuited and the name stayed empty",
        ),
        (
            "P=$(mktemp -d); echo hi > $P/x.log",
            "",
            "the right-hand side is itself a command substitution",
        ),
        (
            "P=$OTHER; echo hi > $P/x.log",
            "",
            "the right-hand side is itself an expansion",
        ),
        (
            "P=(/home/yuki/proj); echo hi > $P/x.log",
            OK_LEAF,
            "an array's first element depends on IFS / [0]-vs-[@] / quoting \
             (measured: [aa] for P=(aa bb))",
        ),
        (
            "export P=/home/yuki/proj; echo hi > $P/x.log",
            OK_LEAF,
            "`export NAME=v` is not a pure assignment list to this scan; bash \
             does bind it, so refusing is the safe error",
        ),
        (
            "declare P=/home/yuki/proj; echo hi > $P/x.log",
            OK_LEAF,
            "`declare` is a builtin, not an assignment list",
        ),
        (
            "local P=/home/yuki/proj; echo hi > $P/x.log",
            OK_LEAF,
            "`local` is a builtin, not an assignment list",
        ),
        (
            "{ P=/home/yuki/proj; }; echo hi > $P/x.log",
            OK_LEAF,
            "a brace group DOES reach the parent shell; refusing it is the safe error",
        ),
        (
            "( P=/home/yuki/proj ); echo hi > $P/x.log",
            OK_LEAF,
            "an explicit subshell's assignment does not reach the parent shell",
        ),
        (
            "P=/home/yuki/proj | echo hi > $P/x.log",
            OK_LEAF,
            "a pipeline stage assigns in its own subshell",
        ),
        (
            "P=/home/yuki/proj & echo hi > $P/x.log",
            OK_LEAF,
            "a backgrounded segment assigns in its own subshell",
        ),
        (
            "P=/home/yuki/proj; P+=/sub; echo hi > $P/x.log",
            OK_LEAF,
            "`P+=` appends, so the current value is `/home/yuki/proj/sub` \
             (measured: [/aa/bb]) and the first assignment is stale",
        ),
        (
            "P=/home/yuki/proj; unset P; echo hi > $P/x.log",
            OK_LEAF,
            "`unset` removes the value the scan can read (measured: [])",
        ),
        (
            "P=/home/yuki/proj; eval \"P=/etc\"; echo hi > $P/x.log",
            OK_LEAF,
            "`eval` can rebind the name from text this scan does not evaluate \
             (measured: [/bb])",
        ),
        (
            "P=/home/yuki/proj; read P; echo hi > $P/x.log",
            OK_LEAF,
            "`read` rebinds the name from stdin, which the text does not contain",
        ),
        (
            "cat <<EOF\nP=/home/yuki/proj\nEOF\necho hi > $P/x.log",
            OK_LEAF,
            "a NAME=value typed into a here-document BODY never ran (measured: [])",
        ),
        (
            "P=/home/yuki/proj; echo hi > $P/$Q",
            OK_DIR,
            "the SUFFIX is itself an expansion — two unknowns, and `$Q` empty \
             makes the target the directory `/home/yuki/proj/` (measured: [/aa/])",
        ),
        (
            "P=/home/yuki/proj; echo hi > $P/${Q}",
            OK_DIR,
            "same, braced: a suffix containing an expansion is not a literal suffix",
        ),
        (
            "P=/home/yuki/proj; echo hi > ${P}/${Q}/x.log",
            OK_DIR,
            "an expansion in the MIDDLE of the suffix is just as unreadable as \
             one at its end",
        ),
        (
            "P=/home/yuki/proj; echo hi > pre$P/x.log",
            OK_LEAF,
            "a PREFIX glued before the reference names a different path \
             (measured: [pre/aa/x]); the existing resolver already refuses \
             `pre$BIN` and this must stay so",
        ),
        (
            "echo hi > ${P:-/home/yuki/proj}/x.log",
            OK_LEAF,
            "`${P:-default}` names the default only when P is unset — a run-time fact",
        ),
        (
            "echo hi > $1/x.log",
            "",
            "`$1` is a positional parameter, not a variable this line assigns \
             (measured: the target became `/x.log`, at the filesystem ROOT)",
        ),
        (
            "echo hi > $$/x.log",
            "",
            "`$$` is the pid — a RELATIVE path whose first component is a number \
             nothing on the line determines (measured: [<pid>/x])",
        ),
        (
            "P=; echo hi > $P/x.log",
            OK_LEAF,
            "an empty value makes the target `/x.log`, a write at the filesystem \
             ROOT (measured) — the opposite of confined",
        ),
        (
            "P=\"/home/yuki/proj\\x\"; echo hi > $P/x.log",
            OK_LEAF,
            "a backslash in the value means three different things depending on \
             quoting; `unquote_literal_value` refuses all three",
        ),
    ] {
        // Collected rather than asserted row by row: a permissive resolver
        // usually breaks SEVERAL of these at once, and stopping at the first one
        // hides how wide the hole is.
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
        // The RESOLVED spelling, not the bare path: `${P:-/home/yuki/proj}/x.log`
        // legitimately carries the path inside the raw token it quotes back, and
        // quoting the token back IS the unresolved answer.
        if !credulous.is_empty() && reason_of(&d).contains(&format!("'> {credulous}'")) {
            violations.push(format!(
                "`{cmd}` reported the target as the credulous `{credulous}` ({why})"
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

/// A suffix carrying GLOB metacharacters must NOT be resolved, and the reason
/// is not "globbing is hard" — it is that the text does not determine the file.
///
/// Measured with `bash -c` (2026-10-02), which is why the restrictive answer is
/// the sound one rather than merely the cautious one. Three different files are
/// named by the same text, chosen by the filesystem at run time:
///
/// ```text
/// # one match: the EXISTING file is truncated
/// $ echo OLD > vs/one.log
/// $ bash -c 'P=vs; echo NEW > $P/*.log'      -> exit 0, vs/one.log == NEW
///
/// # no match: a file literally named `*.log` is created
/// $ bash -c 'cd nm && echo hi > *.log; ls'   -> exit 0, `*.log`
///
/// # two matches: nothing is written at all
/// $ bash -c 'cd g && echo hi > *.log'        -> bash: *.log: ambiguous redirect
///
/// # and the shell's own mode changes the answer again
/// $ bash --posix -c 'cd px && echo P2 > *.log; ls'
///       -> exit 0, creates `*.log`; px/solo.log still ORIGINAL
///
/// # `?` behaves identically
/// $ echo OLD > qm/a.log
/// $ bash -c 'P=qm; echo QM > $P/?.log'       -> exit 0, qm/a.log == QM
/// ```
///
/// Resolving the glob to a literal is therefore not a conservative
/// simplification, it is the permissive error, and it is directly reachable
/// from the stated fix ("concatenate the literal suffix"): `*` is not an
/// expansion as far as `has_unresolvable_expansion` is concerned, so a naive
/// concatenation produces `/home/yuki/proj/*.log`, and
/// `echo hi > /home/yuki/proj/*.log` measures as **Allow** in this fixture
/// (nothing is literally named `*.log`, so the recoverability probe answers
/// `NothingToDestroy`). The single-match row above shows bash truncating a real
/// file for that same text. Per CLAUDE.md §3 a target the text does not
/// determine is not a permissive target.
#[test]
fn unresolvable_glob_suffix_is_not_resolved_to_a_literal_path() {
    // Pin the trap first, so the test says WHY it is restrictive: if this ever
    // stops being Allow, the glob rows below stop being fail-open probes.
    assert_eq!(
        scoped("echo hi > /home/yuki/proj/*.log"),
        Decision::Allow,
        "fixture precondition: a literal `*.log` under the project root is Allowed, \
         which is what makes resolving a glob suffix the permissive error"
    );
    for cmd in [
        "P=/home/yuki/proj; echo hi > $P/*.log",
        "P=/home/yuki/proj; echo hi > $P/?.log",
        "P=/home/yuki/proj; echo hi > $P/[ab].log",
        "P=/home/yuki/proj; echo hi > ${P}/*.log",
        "P=/home/yuki/proj; echo hi > \"$P\"/*.log",
    ] {
        let d = scoped(cmd);
        assert!(
            d.is_blocking(),
            "`{cmd}` names whichever file the filesystem happens to match at run \
             time (measured: an existing `one.log` was truncated), so it must not \
             be resolved to a literal path; got {d:?}"
        );
        assert!(
            d.clone().hardened().is_deny(),
            "`{cmd}` must harden to Deny when no human is available; got {:?}",
            d.clone().hardened()
        );
    }
}

/// An `rm`-shaped reminder that the suffix split must not be applied to the
/// APPEND twin by halves. `>>` does not truncate, so its verdict differs from
/// `>`; what must stay true is that the same resolution reaches it — the
/// `a83802ad` lesson recorded on `append_target_occurrences`, that the append
/// path judged the raw token only and Allowed `P=/etc/fstab; echo x >> $P`.
///
/// Pinned as "not Allow" rather than as a reason string, because the append
/// verdict for a system directory is the system-directory axis' business and is
/// being retuned independently of this feature.
#[test]
fn unresolvable_append_suffix_to_a_system_directory_is_not_allowed() {
    for cmd in [
        "P=/etc; echo x >> $P/fstab",
        "P=/etc; echo x >> ${P}/fstab",
        "P=/etc; echo x >> \"$P\"/fstab",
    ] {
        let d = scoped(cmd);
        assert!(
            d.is_blocking(),
            "`{cmd}` appends into a system directory; got {d:?}"
        );
    }
}

// ----------------------------------------------------------------- decoy ----

/// ADVERSARIAL, AND CURRENTLY FAILING — for a reason one step short of the
/// bare-case decoy in `redirect_target_resolution.rs`. There, the resolver ran
/// and handed every occurrence of a token the FIRST occurrence's answer. Here
/// the resolver does not run at all (`referenced_variable_name` rejects
/// `$P/a.log`), so the line falls through to the single line-level Deny raised
/// by the FIRST redirect, whose reason quotes `'> $P/a.log'` and never mentions
/// `/etc/fstab` — the file the shell actually truncates.
///
/// It is written now because an implementation that resolves by TOKEN TEXT
/// instead of by occurrence would make the `resolved_*` tests pass while
/// reintroducing exactly that bug for the suffixed shape. The machinery to
/// avoid it already exists (`redirect_target_occurrences` carries a segment
/// index per occurrence, and `resolve_redirect_target_at` takes one), so this
/// is reachable, not aspirational.
///
/// The real shell, measured (2026-10-02), both writes landing:
///
/// ```text
/// $ bash -c 'P=SB/d1; echo a > $P/a.log; P=SB/d2; echo b > $P/b.log'
/// $ ls SB/d1   -> a.log
/// $ ls SB/d2   -> b.log
/// ```
#[test]
fn decoy_rebinding_must_not_excuse_the_later_suffixed_redirect() {
    for (alone, with_decoy, hazard) in [
        (
            "P=/etc; echo b > $P/fstab",
            "P=/home/yuki/proj; echo a > $P/a.log; P=/etc; echo b > $P/fstab",
            "/etc/fstab",
        ),
        (
            "P=/etc; echo b > $P/sudoers.d/evil",
            "P=/home/yuki/proj; echo a > $P/a.log; P=/etc; echo b > $P/sudoers.d/evil",
            "/etc/sudoers.d/evil",
        ),
        (
            "P=/home/yuki; echo b > $P/.claude/settings.json",
            "P=/home/yuki/proj; echo a > $P/a.log; \
             P=/home/yuki; echo b > $P/.claude/settings.json",
            "/home/yuki/.claude/settings.json",
        ),
    ] {
        let alone_d = scoped(alone);
        let decoy_d = scoped(with_decoy);
        assert!(
            restrictiveness(&decoy_d) >= restrictiveness(&alone_d),
            "adding a harmless earlier redirect through the same token weakened the \
             verdict: `{alone}` -> {alone_d:?} but `{with_decoy}` -> {decoy_d:?}"
        );
        assert!(
            reason_of(&decoy_d).contains(hazard),
            "the verdict for `{with_decoy}` must name `{hazard}` — the file the shell \
             actually truncates; got {:?}",
            reason_of(&decoy_d)
        );
    }
}

/// The same defect reached without a second live redirect: a here-document BODY
/// supplies the decoy, and a body is data the shell never runs.
///
/// Measured (2026-10-02) — the body line is printed, and the only redirect the
/// line executes lands on the rebound path:
///
/// ```text
/// $ bash -c 'cat <<EOF
///            P=/aa
///            EOF
///            echo "[$P]"'      -> prints `P=/aa`, then `[]`
/// ```
#[test]
fn decoy_inside_a_here_document_body_must_not_excuse_the_real_suffixed_redirect() {
    let cmd = "P=/home/yuki/proj\ncat <<'EOF'\necho x > $P/a.log\nEOF\n\
               P=/etc\necho pwn > $P/fstab";
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

// --------------------------------------------------------------- symlink ----
//
// Everything above installs `identity` as the symlink resolver, which models a
// filesystem with no symlinks. That model cannot observe the property that
// decides whether a `..` in the suffix may be resolved, and on 2026-10-02 the
// author of this file got that question wrong BECAUSE of the model: the fixture
// happily satisfied pair-equality for a `..` suffix, so pair-equality looked
// like the rule to pin. It is not. `open()` resolves `..` against the directory
// a component REALLY is, so one symlink before a `..` makes the concatenated
// text name one file and the syscall open another.
//
// This group therefore uses REAL symlinks and the REAL resolver, by spawning
// the binary the way `worktree_root_symlink_dotdot.rs` does. That file pins the
// same hazard class for `rm -rf` and never for a redirect, which is why the
// redirect side went unnoticed. Measured non-destructively, with `lnk` a
// symlink to a real directory inside the project:
//
//     printf WRITTEN > <proj>/lnk/../victim.txt
//       lexical parent <proj>/victim.txt       -> LEXICAL-PARENT-UNTOUCHED
//       the write hit  <proj>/other/victim.txt -> WRITTEN
//
// `no_symlink_below` cannot catch it, because `normalize_abs` deletes the
// symlink component before the walk begins — which is also why the no-`..`
// control below IS caught.
//
// NOT asserted here, deliberately: the LITERAL and bare-`> $P` spellings of the
// same escape are Allow today (measured on the deployed 0.2.97 and on this
// build). That is a real p1 fail-open, filed as backlog 3ca56588 with remedy
// candidates left for a human, and it is not this file's to adjudicate —
// pinning a verdict for it would pre-empt that ruling, and pinning the current
// Allow would cement the bug. What IS asserted is the filesystem fact that
// makes the hazard real, so the rows below cannot pass by accident.

use std::io::Write as _;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// An on-disk fixture with a real escaping symlink, under `CARGO_TARGET_TMPDIR`
/// rather than `/tmp` — `/tmp` is itself a blastguard safe root, which would
/// decide the verdict for us.
struct SymlinkFx {
    base: PathBuf,
    home: PathBuf,
    proj: PathBuf,
}

impl SymlinkFx {
    fn new(name: &str) -> SymlinkFx {
        let base = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join("redirect_target_suffix_symlink")
            .join(name);
        let _ = std::fs::remove_dir_all(&base);
        let f = SymlinkFx {
            home: base.join("home"),
            proj: base.join("src/proj"),
            base,
        };
        std::fs::create_dir_all(&f.home).unwrap();
        std::fs::create_dir_all(f.proj.join("sub")).unwrap();
        // `esc` -> `/`, so `esc/../..` is still `/` to the kernel and the tail
        // `etc/fstab` lands on the real `/etc/fstab`, while lexical
        // normalisation pops `esc` and `proj` and stays inside the fixture.
        symlink("/", f.proj.join("esc")).unwrap();
        f
    }

    /// Prove the fixture before trusting any verdict about it: the text really
    /// does name `expected_real` to the kernel, and the LEXICAL reading of the
    /// same text really does not. Without this the rows could pass while
    /// measuring nothing.
    fn assert_escape_is_real(&self, spelling: &str, expected_real: &str) {
        let written = self.proj.join(spelling);
        let real = std::fs::canonicalize(&written)
            .unwrap_or_else(|e| panic!("fixture: {} must resolve: {e}", written.display()));
        assert_eq!(
            real,
            Path::new(expected_real),
            "fixture: {} must really resolve to {expected_real}",
            written.display()
        );
        let lexical = lexically_normalise(&written.display().to_string());
        assert_ne!(
            lexical,
            expected_real,
            "fixture: the LEXICAL reading of {} must differ from the real one, \
             or this row is not testing the symlink property at all",
            written.display()
        );
    }

    /// Empty stdout is this hook's `Allow`.
    fn verdict(&self, command: &str) -> String {
        let payload = json!({
            "hook_event_name": "PreToolUse",
            "cwd": self.proj.display().to_string(),
            "tool_name": "Bash",
            "tool_input": { "command": command },
        })
        .to_string();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("CLAUDE_PROJECT_DIR", &self.proj)
            .env("HOME", &self.home)
            .env("BLASTGUARD_APPROVALS_DIR", self.base.join("store"))
            .env("CLAUDE_CODE_ENTRYPOINT", "cli")
            .env_remove("BLASTGUARD_ASK")
            .env_remove("TMPDIR")
            // Without this the repeat-refusal ledger (`downgrade_on_repeat`,
            // keyed on `CLAUDE_CODE_SESSION_ID` read from the ENVIRONMENT, not
            // from the payload) turns a second identical Deny into an Ask.
            // Every binary-spawning test in this crate scrubs it, for exactly
            // this reason.
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .current_dir(&self.base);
        let mut child = cmd.spawn().expect("binary spawns");
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(payload.as_bytes());
        }
        let out = child.wait_with_output().expect("binary runs");
        assert_eq!(
            out.status.code(),
            Some(0),
            "hook must exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

/// Collapse `.` and `..` textually, the way `scope::normalize_abs` does, so the
/// fixture check compares the LEXICAL reading against the real one rather than
/// assuming they differ.
fn lexically_normalise(path: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for comp in path.split('/') {
        match comp {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    format!("/{}", out.join("/"))
}

/// THE REASON the `..` refusal in `literal_suffix` exists, pinned against a real
/// symlink instead of against prose.
///
/// Resolving this suffix would hand the axes `<proj>/esc/../../etc/fstab`, and
/// `resolve_redirect_target_at`'s `Some(String)` binds to the SAME `target`
/// variable a literal does, before `place_redirect_target` and every axis run —
/// there is no separate code path for resolved values. So whatever the literal
/// answers, a resolved suffix answers; measured, the literal answers `Allow`.
/// Resolving here would therefore move a command that truncates `/etc/fstab`
/// from Deny to Allow, which is the one direction forbidden.
#[test]
fn symlink_before_dotdot_in_a_suffix_must_not_be_resolved() {
    let f = SymlinkFx::new("hazard");
    f.assert_escape_is_real("esc/../../etc/fstab", "/etc/fstab");
    let cmd = format!("P={}; : > $P/esc/../../etc/fstab", f.proj.display());
    let out = f.verdict(&cmd);
    assert!(
        !out.trim().is_empty(),
        "`{cmd}` truncates /etc/fstab (fixture-verified above) and must NOT be \
         Allow; got empty output (= Allow)"
    );
}

/// Control proving the fixture's symlink is really wired, and that the symlink
/// axis does work when lexical normalisation leaves the component in place.
/// Without this the row above could pass because the fixture was inert.
#[test]
fn symlink_without_a_dotdot_is_caught_by_the_symlink_axis() {
    let f = SymlinkFx::new("nodotdot");
    let cmd = format!("P={}; : > $P/esc/etc/fstab", f.proj.display());
    let out = f.verdict(&cmd);
    assert!(
        !out.trim().is_empty(),
        "`{cmd}` names /etc/fstab through a symlink that survives normalisation \
         and must NOT be Allow; got empty output (= Allow)"
    );
}

/// Control proving the binary is not simply refusing everything under this
/// fixture — without it the two rows above would be vacuous.
#[test]
fn symlink_fixture_still_allows_an_ordinary_confined_suffix() {
    let f = SymlinkFx::new("control");
    for suffix in ["absent.log", "sub/absent.log"] {
        let cmd = format!("P={}; : > $P/{suffix}", f.proj.display());
        let out = f.verdict(&cmd);
        assert!(
            out.trim().is_empty(),
            "`{cmd}` is an ordinary write inside the session's own project and \
             must be Allow, or the rows above prove nothing; got {out}"
        );
    }
}
