//! The location axis: a destructive command whose targets provably land inside
//! this session's own tree (or a temp dir) is a QUESTION; the same command
//! aimed anywhere else — or aimed at something blastguard cannot resolve —
//! stays a REFUSAL.
//!
//! Why this file exists at all. Measured on blastguard 0.2.50, with real
//! PreToolUse payloads piped into the hook binary:
//!
//! ```text
//! rm -rf target   -> deny: recursive rm (-r) can delete an entire directory tree
//! rm -rf /tmp/foo -> deny: recursive rm (-r) can delete an entire directory tree
//! rm -rf /usr/lib -> deny: recursive rm (-r) can delete an entire directory tree
//! rm -rf /        -> deny: recursive rm (-r) can delete an entire directory tree
//! ```
//!
//! One verdict and one reason string for four blast radii that differ by orders
//! of magnitude. The operator who cannot clear `rm -rf target` does not stop
//! deleting `target`; they reach for a route with LESS analysis behind it (a
//! python `shutil.rmtree`, a generated shell script, a bypass-permissions
//! session). So the false positives were not free: they were pushing work out
//! of the gate's sight.
//!
//! The two halves of this file are equally load-bearing. `relaxed_*` tests pin
//! that the confined case is now answerable; `still_refused_*` tests pin that
//! nothing else moved. A change that makes the first half pass by weakening the
//! second half has not implemented this feature, it has removed the gate.

use blastguard::detect;
use blastguard::model::Decision;
use blastguard::scope::SafeRoots;
use serde_json::json;

const PROJECT: &str = "/home/yuki/proj";
const HOME: &str = "/home/yuki";

/// Models a filesystem with no symlinks: every path is already its own real
/// path. Injected so these tests never touch the disk.
fn identity(p: &str) -> Option<String> {
    Some(p.to_string())
}

/// Models `<project>/link` being a symlink to `/usr/lib` — the escape a
/// lexical-only placement check cannot see.
fn symlinked(p: &str) -> Option<String> {
    let link = format!("{PROJECT}/link");
    if p == link || p.starts_with(&format!("{link}/")) {
        return Some(p.replacen(&link, "/usr/lib", 1));
    }
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

fn scoped(cmd: &str) -> Decision {
    detect::detect_scoped("Bash", Some(&json!({ "command": cmd })), &roots())
}

fn scoped_with(cmd: &str, roots: &SafeRoots) -> Decision {
    detect::detect_scoped("Bash", Some(&json!({ "command": cmd })), roots)
}

/// The contract for a confined destructive command.
///
/// Asserts BOTH halves, which is strictly more than `!is_deny()`:
///
///   * an interactive session gets an `Ask` — one keypress, and the operator
///     sees the resolved blast radius in the reason string;
///   * anywhere no human can answer, `hardened()` collapses it to `Deny`, so
///     the autonomous-agent threat model is EXACTLY as it was before this
///     feature. That second assertion is what stops this file from being a
///     description of a fail-open.
#[track_caller]
fn assert_confined_ask(cmd: &str, d: Decision) {
    assert!(
        d.is_ask(),
        "`{cmd}` is confined to a safe root, so it must Ask (not refuse); got {d:?}"
    );
    assert!(
        d.clone().hardened().is_deny(),
        "`{cmd}`: with no human to answer, the Ask must harden to a Deny; got {:?}",
        d.hardened()
    );
    if let Decision::Ask(reason) = d {
        assert!(
            reason.contains("is confined to"),
            "`{cmd}`: the reason must name the bounded blast radius, got {reason:?}"
        );
    }
}

/// The contract for everything else: a flat refusal, unchanged by this feature.
#[track_caller]
fn assert_still_denied(cmd: &str, d: Decision) {
    assert!(
        d.is_deny(),
        "`{cmd}` must stay a Deny — it is not provably confined; got {d:?}"
    );
}

/// The contract for a PROTECTED target: location buys it nothing.
///
/// Weaker than `assert_still_denied` on purpose, because some protected-path
/// verdicts are legitimately an `Ask` already (writing to a gate config can
/// strengthen it as easily as disarm it, and blastguard refuses to guess which
/// — see `protected_path_block`). What must hold is that the verdict still
/// stops the call AND is not the confined-blast-radius Ask: a gate config does
/// not become approvable by being conveniently nearby.
#[track_caller]
fn assert_not_relaxed(cmd: &str, d: Decision) {
    assert!(d.is_blocking(), "`{cmd}` must still be stopped; got {d:?}");
    assert!(
        d.clone().hardened().is_deny(),
        "`{cmd}` must harden to a Deny with no human present; got {:?}",
        d.clone().hardened()
    );
    let reason = match &d {
        Decision::Deny(r) | Decision::Ask(r) => r.as_str(),
        Decision::Allow => "",
    };
    assert!(
        !reason.contains("is confined to"),
        "`{cmd}` must NOT be excused by its location; got {reason:?}"
    );
}

// ---------------------------------------------------------------- relaxed ----

#[test]
fn relaxed_recursive_rm_inside_the_project() {
    for cmd in [
        "rm -rf target",
        "rm -fr target",
        "rm -rf ./target/debug",
        "rm -rf /home/yuki/proj/target",
        "rm -rf node_modules",
        "rm -rf crates/blastguard/target",
        "rm -rf target build dist",
    ] {
        assert_confined_ask(cmd, scoped(cmd));
    }
}

#[test]
fn relaxed_recursive_rm_inside_a_temp_dir() {
    for cmd in ["rm -rf /tmp/foo", "rm -rf /var/tmp/build-cache"] {
        assert_confined_ask(cmd, scoped(cmd));
    }
}

#[test]
fn relaxed_wildcard_rm_whose_literal_prefix_is_confined() {
    // The glob itself is never resolved (this crate does not expand shell
    // constructs); what is resolved is the literal directory prefix it can only
    // ever expand INSIDE.
    for cmd in ["rm -rf target/*", "rm target/*.o", "rm -f /tmp/foo/*"] {
        assert_confined_ask(cmd, scoped(cmd));
    }
}

#[test]
fn relaxed_rm_after_a_cd_that_stays_inside() {
    for cmd in [
        "cd crates && rm -rf target",
        "cd /home/yuki/proj/crates && rm -rf blastguard/target",
    ] {
        assert_confined_ask(cmd, scoped(cmd));
    }
}

#[test]
fn relaxed_find_delete_confined_to_a_temp_dir_or_a_subdirectory() {
    for cmd in [
        "find /tmp/x -mtime +1 -delete",
        "find target -name '*.o' -delete",
        "find ./target -type f -delete",
    ] {
        assert_confined_ask(cmd, scoped(cmd));
    }
}

#[test]
fn relaxed_find_delete_from_the_project_root_needs_a_narrowing_predicate() {
    // `find . -name '*.o' -delete` is ordinary work: the search root is the
    // project itself, but the expression names a subset.
    for cmd in [
        "find . -name '*.o' -delete",
        "find . -type f -name '*.tmp' -delete",
        "find . -mtime +30 -delete",
    ] {
        assert_confined_ask(cmd, scoped(cmd));
    }
}

#[test]
fn relaxed_find_exec_rm_confined_to_the_project() {
    for cmd in [
        "find target -name '*.tmp' -exec rm -f {} +",
        "find . -name '*.o' -exec rm -rf {} \\;",
    ] {
        assert_confined_ask(cmd, scoped(cmd));
    }
}

#[test]
fn relaxed_truncating_forms_inside_the_project() {
    for cmd in [
        "truncate -s 0 target/log.txt",
        "echo hi > target/log.txt",
        "cargo test > /tmp/test.log",
        "shred target/secret.bin",
    ] {
        assert_confined_ask(cmd, scoped(cmd));
    }
}

#[test]
fn relaxed_git_clean_and_recursive_chmod_inside_the_project() {
    // `git clean` never removes tracked files or `.git` itself, so — unlike
    // `rm` — the safe root being the target is acceptable for it.
    for cmd in [
        "git clean -fdx",
        "git clean -fd target",
        "chmod -R 755 target",
        "chown -R yuki target",
    ] {
        assert_confined_ask(cmd, scoped(cmd));
    }
}

// ----------------------------------------------------------- still denied ----

#[test]
fn still_refused_outside_every_safe_root() {
    for cmd in [
        "rm -rf /usr/lib",
        "rm -rf /",
        "rm -rf /etc",
        "rm -rf /mnt/c/Users",
        "rm -rf /mnt/c/Windows/System32",
        "rm -rf /home/yuki",
        "rm -rf /home/yuki/.cache",
        "rm -rf ../../usr/lib",
        "find / -name '*.o' -delete",
        "find /usr -name x -delete",
        "truncate -s 0 /usr/lib/libc.so",
        "shred /etc/shadow",
        "chmod -R 000 /usr",
    ] {
        assert_still_denied(cmd, scoped(cmd));
    }
}

#[test]
fn still_refused_when_the_target_is_not_a_literal_path() {
    // Nothing here is expanded by blastguard, so nothing here is resolvable —
    // and unresolvable is not "fine".
    for cmd in [
        "rm -rf $HOME",
        "rm -rf ${BUILD_DIR}",
        "rm -rf ~/.cache",
        "rm -rf \"$(pwd)/target\"",
        "rm -rf `pwd`/target",
        "rm -rf *",
        "rm -rf ./*",
        "rm -rf {target,dist}",
    ] {
        assert_still_denied(cmd, scoped(cmd));
    }
}

#[test]
fn still_refused_when_the_working_directory_is_unknown() {
    // A `cd` this analysis cannot resolve means a relative operand has no known
    // base — so it is not assumed to sit in the project.
    for cmd in [
        "cd $BUILD && rm -rf target",
        "cd ~ && rm -rf .cache",
        "popd && rm -rf target",
    ] {
        assert_still_denied(cmd, scoped(cmd));
    }
}

#[test]
fn still_refused_when_a_cd_takes_the_relative_operand_out_of_the_tree() {
    // The bug this pins: judging `lib` against the SESSION cwd instead of the
    // cwd the line actually established would call `/usr/lib` confined.
    for cmd in [
        "cd /usr && rm -rf lib",
        "cd /home/yuki/proj/.. && rm -rf yuki",
        "cd / && rm -rf usr",
    ] {
        assert_still_denied(cmd, scoped(cmd));
    }
}

#[test]
fn still_refused_for_the_safe_root_itself() {
    // `rm -rf <the project>` takes `.git`, every gate config and the worktree
    // the session runs in. Being "inside the project" is not a thing the
    // project's own directory is.
    for cmd in [
        "rm -rf /home/yuki/proj",
        "rm -rf .",
        "rm -rf ./",
        "rm -rf /tmp",
        "rm -rf crates/..",
    ] {
        assert_still_denied(cmd, scoped(cmd));
    }
}

#[test]
fn still_refused_for_protected_gate_paths_inside_the_project() {
    // Location cannot buy anything here: these are protected BECAUSE they are
    // in this tree.
    for cmd in [
        "rm -rf .git",
        "rm -rf .claude",
        "rm -rf .githooks",
        "rm -rf .claude/hooks",
        "rm .claude/settings.json",
        "rm -rf /home/yuki/proj/.claude",
        "find .githooks -type f -delete",
        "truncate -s 0 .claude/settings.json",
        "echo x > .githooks/pre-commit",
    ] {
        assert_not_relaxed(cmd, scoped(cmd));
    }
}

#[test]
fn still_refused_when_a_symlink_leaves_the_tree() {
    let s = SafeRoots::new(Some(PROJECT), None, Some(HOME), None, Some(symlinked));
    for cmd in ["rm -rf link", "rm -rf ./link/", "find link -delete"] {
        assert_still_denied(cmd, scoped_with(cmd, &s));
    }
}

#[test]
fn still_refused_when_find_would_walk_the_whole_project_unfiltered() {
    // No narrowing predicate: `find . -delete` deletes the tree, `.git` and
    // all. The root-itself rule that applies to `rm` applies here too.
    for cmd in ["find . -delete", "find /home/yuki/proj -delete"] {
        assert_still_denied(cmd, scoped(cmd));
    }
}

#[test]
fn still_refused_when_find_exec_reaches_outside_its_search_root() {
    // The `{}` placeholder stands for a matched path, but an -exec payload can
    // also name absolute targets of its own.
    for cmd in [
        "find target -name x -exec rm -rf /usr/lib \\;",
        "find / -exec rm -rf {} +",
        "find target -exec sh -c 'rm -rf /' \\;",
    ] {
        assert_still_denied(cmd, scoped(cmd));
    }
}

// -------------------------------------------------------------- unchanged ----

#[test]
fn the_unscoped_entry_point_is_unchanged() {
    // `detect::detect` has no location model, so every consumer that hands
    // commands to `sh -c` with no human present (condukt's check runner,
    // specguard's forge, daily's task runner) sees exactly the pre-0.2.51
    // verdicts. This is the compatibility half of the feature, and it is the
    // reason the relaxation cannot leak into unattended execution.
    for cmd in [
        "rm -rf target",
        "rm -rf /tmp/foo",
        "find . -name '*.o' -delete",
        "truncate -s 0 target/log.txt",
        "git clean -fdx",
        "chmod -R 755 target",
    ] {
        let d = detect::detect("Bash", Some(&json!({ "command": cmd })));
        assert!(
            d.is_deny(),
            "unscoped `{cmd}` must keep its Deny; got {d:?}"
        );
    }
}

#[test]
fn an_empty_safe_root_model_is_the_unscoped_behaviour() {
    // `SafeRoots::none()` is what a caller with no cwd gets, and it must not be
    // a weaker gate than `detect`.
    let none = SafeRoots::none();
    for cmd in ["rm -rf target", "rm -rf /tmp/foo", "find . -name x -delete"] {
        assert_still_denied(cmd, scoped_with(cmd, &none));
    }
}

#[test]
fn ordinary_commands_are_still_allowed() {
    // The floor: this feature must not turn quiet allows into asks.
    for cmd in [
        "cargo test -p blastguard",
        "rm target/x.o",
        "ls -la target",
        "git status",
        "cat target/log.txt",
        "echo hi >> target/log.txt",
        // Neither recursive nor a wildcard, so it never reached the
        // destructive-shape branch even before the location axis existed.
        "rm -d empty-dir",
    ] {
        assert_eq!(
            scoped(cmd),
            Decision::Allow,
            "`{cmd}` must stay a silent Allow"
        );
    }
}
