// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Independent verification of `taintguard::readonly::is_readonly_bash` after
//! the 0.2.0 inversion (allowlisted programs AND allowlisted arguments,
//! replacing the program-allowlist + flag-denylist hybrid).
//!
//! # Why this file exists and who wrote it
//!
//! CLAUDE.md §2(a): the implementer does not write the tests that verify their
//! own implementation. This file was written by a separate verifier agent that
//! did not write `readonly.rs`. It deliberately lives OUTSIDE `readonly.rs`'s
//! own `mod tests` so that it exercises the crate's public surface
//! (`pub mod readonly` -> `pub fn is_readonly_bash`) and so that no production
//! edit is needed to run it.
//!
//! # What this file is NOT
//!
//! It is not evidence that the inversion is complete. The measured fact that
//! motivated it is the opposite: all 118 pre-existing taintguard tests passed
//! *unmodified* across the inversion, which means the pre-existing suite never
//! discriminated the old design from the new one and therefore never covered
//! any of the thirteen holes. "The suite is green" was evidence of nothing.
//! Every assertion below is here because the property it pins was previously
//! unpinned.
//!
//! Two confirmed live defects found during this verification are NOT in this
//! file — they are in `readonly_known_defects.rs`, which is RED on purpose.
//! Read that file before treating this one's GREEN as reassuring.
//!
//! # Attribution of the thirteen holes
//!
//! `readonly.rs`'s module docs state that thirteen reachable write/exec paths
//! were found in the old hybrid: seven by the implementer's own review and six
//! by an independent verifier. The docs enumerate the verifier's six by name
//! (`uniq IN OUT`, `rg --pre`, `rg --hostname-bin`, `sort --compress-program`,
//! `git grep -O`/`--open-files-in-pager`, `git ls-remote`) but do NOT enumerate
//! the implementer's seven. The "round 1" attributions below are therefore
//! INFERRED from the removal rationales that `readonly.rs` records inline
//! (`READONLY_PROGRAMS`' four removed programs, the reflog/worktree split into
//! `GIT_SUBCOMMANDS_WITH_ONE_READONLY_SHAPE`, and the `-c` decision) rather
//! than read off a source of record. The regression value of each test does not
//! depend on the attribution being right; the attribution is stated as an
//! inference so a later reader does not mistake it for a citation.

use taintguard::readonly::is_readonly_bash;

fn assert_readonly(command: &str) {
    assert!(
        is_readonly_bash(command),
        "expected {command:?} to be recognised as read-only"
    );
}

fn assert_gated(command: &str) {
    assert!(
        !is_readonly_bash(command),
        "expected {command:?} NOT to be recognised as read-only"
    );
}

// ---------------------------------------------------------------------------
// (a) The thirteen known holes, one test each.
//
// One test per hole rather than one loop over thirteen strings: a loop reports
// the first failure and hides the rest, and these are precisely the assertions
// whose individual identity matters.
// ---------------------------------------------------------------------------

/// Hole 1/13 — round 1 (implementer review, inferred; `readonly.rs`
/// `READONLY_PROGRAMS` docs). `env` prints the environment when bare, but
/// `env FOO=bar <cmd>` EXECS `<cmd>`: a general-purpose executor wearing a
/// read-only-looking name. Under the old table this made `env rm -rf x`
/// read-only.
#[test]
fn hole_01_env_execs_its_operand() {
    assert_gated("env FOO=bar shred x");
    assert_gated("env shred");
    // The bare form is pinned too: it really is read-only, which is exactly why
    // re-adding "just the harmless shape" is the tempting regression. The unit
    // of decision is the program, not the invocation.
    assert_gated("env");
}

/// Hole 2/13 — round 1 (inferred). `date -s`/`--set` writes the system clock.
#[test]
fn hole_02_date_sets_the_system_clock() {
    assert_gated("date -s 2020-01-01");
    assert_gated("date --set=2020-01-01");
    assert_gated("date");
}

/// Hole 3/13 — round 1 (inferred). `hostname <name>` SETS the host name from a
/// BARE ARGUMENT — no flag table could have caught it, the same shape as the
/// `uniq` hole below.
#[test]
fn hole_03_hostname_sets_the_host_name_from_a_bare_argument() {
    assert_gated("hostname newname");
    assert_gated("hostname");
}

/// Hole 4/13 — round 1 (inferred). `file -C -m <magfile>` COMPILES a magic file
/// and writes `<magfile>.mgc` next to it.
#[test]
fn hole_04_file_compiles_and_writes_a_magic_file() {
    assert_gated("file -C -m magic");
    assert_gated("file x.txt");
}

/// Hole 5/13 — round 1 (inferred). `git reflog` was in the always-read-only
/// subcommand table, so `git reflog expire` — which DESTROYS reflog entries,
/// i.e. destroys the recovery path for every other mutation — was admitted by
/// its subcommand name alone. "The bare form reads" is not "every form reads".
#[test]
fn hole_05_git_reflog_expire_destroys_reflog_entries() {
    assert_gated("git reflog expire");
    assert_gated("git reflog expire --all");
    assert_gated("git reflog delete");
}

/// Hole 6/13 — round 1 (inferred). Same collapse as hole 5 for `git worktree`:
/// `list` reads, but `add`/`remove`/`prune`/`repair`/`move` mutate.
#[test]
fn hole_06_git_worktree_mutating_verbs() {
    assert_gated("git worktree add /tmp/x");
    assert_gated("git worktree remove x");
    assert_gated("git worktree prune");
    assert_gated("git worktree repair");
    assert_gated("git worktree move a b");
    // Bare `git worktree` is not the one read-only shape either.
    assert_gated("git worktree");
}

/// Hole 7/13 — round 1 (inferred). `git -c core.pager=<cmd> <subcommand>` runs
/// `<cmd>`. The fix is structural — no option is inspected before the
/// subcommand at all — so several shapes are pinned here rather than just the
/// pager one.
#[test]
fn hole_07_git_options_before_the_subcommand_run_programs() {
    assert_gated("git -c core.pager=shred status");
    assert_gated("git -c core.pager=shred log");
    assert_gated("git -c alias.x=!shred status");
    assert_gated("git --exec-path=/tmp status");
    assert_gated("git -C /tmp status");
    assert_gated("git --no-pager status");
}

/// Hole 8/13 — round 2 (independent verifier; enumerated verbatim in
/// `readonly.rs`'s docs, and demonstrated destructively there).
/// `uniq [INPUT [OUTPUT]]` — the SECOND POSITIONAL is an output file that is
/// truncated. No flag denylist could ever have caught it because it is not a
/// flag. The only defence is `max_operands: 1`.
///
/// See `readonly_known_defects.rs`: this defence is bypassable via glob
/// expansion, which this verification round demonstrated destructively.
#[test]
fn hole_08_uniq_second_operand_is_an_output_file() {
    assert_gated("uniq in.txt victim.txt");
    assert_gated("uniq -c in.txt victim.txt");
}

/// Hole 9/13 — round 2 (verifier, enumerated in `readonly.rs`).
/// `rg --pre <cmd>` hands ripgrep an arbitrary program to run on every file.
#[test]
fn hole_09_rg_pre_runs_an_arbitrary_program() {
    assert_gated("rg --pre shred foo");
    assert_gated("rg --pre=shred foo");
    // `--pre-glob` is the same family and must not be admitted either.
    assert_gated("rg --pre-glob=*.gz foo");
}

/// Hole 10/13 — round 2 (verifier, enumerated in `readonly.rs`).
/// `rg --hostname-bin=<cmd>` runs `<cmd>` to determine the hostname.
#[test]
fn hole_10_rg_hostname_bin_runs_an_arbitrary_program() {
    assert_gated("rg --hostname-bin=shred foo");
    assert_gated("rg --hostname-bin shred foo");
}

/// Hole 11/13 — round 2 (verifier, enumerated in `readonly.rs`).
/// `sort --compress-program=PROG` runs `PROG` on every temporary file.
#[test]
fn hole_11_sort_compress_program_runs_an_arbitrary_program() {
    assert_gated("sort --compress-program=shred f");
    assert_gated("sort --compress-program shred f");
    // `-T`/`--temporary-directory` hands `sort` a write target; `-o` writes the
    // output file outright. Both are refused by absence from `sort`'s tables.
    assert_gated("sort -T /tmp f");
    assert_gated("sort -o out.txt f");
    assert_gated("sort --output=out.txt f");
}

/// Hole 12/13 — round 2 (verifier, enumerated in `readonly.rs`).
/// `git grep -O<pager>` / `--open-files-in-pager=<pager>` launches a program.
/// The old denylist compared against `-o` with a CASE-SENSITIVE prefix match,
/// so `-O` walked straight past it. Verified against this machine's git:
/// `git grep -h` lists `-O, --[no-]open-files-in-pager[=<pager>]`.
#[test]
fn hole_12_git_grep_capital_o_opens_a_pager() {
    assert_gated("git grep -Oshred foo");
    assert_gated("git grep -O shred foo");
    assert_gated("git grep --open-files-in-pager=shred foo");
    // The case distinction is the whole point of hole 12: assert BOTH cases so
    // a future edit cannot fix one and reopen the other.
    assert_gated("git grep -O foo");
    assert_gated("git grep -o foo");
}

/// Hole 13/13 — round 2 (verifier, enumerated in `readonly.rs`).
/// `git ls-remote <url>` opens an OUTBOUND network channel out of a turn that
/// is tainted precisely because it consumed untrusted content — exfiltration,
/// not local writing — and `--upload-pack=<exec>` also ran a program.
///
/// The second half of this test is the control for the removal: `ls-remote`
/// had to go without taking `ls-files`/`ls-tree` with it, which a prefix-shaped
/// removal would have done.
#[test]
fn hole_13_git_ls_remote_is_an_outbound_channel() {
    assert_gated("git ls-remote origin");
    assert_gated("git ls-remote https://example.com/r.git");
    assert_gated("git ls-remote --upload-pack=shred origin");
    assert_gated("git ls-remote");

    assert_readonly("git ls-files");
    assert_readonly("git ls-tree HEAD");
}

// ---------------------------------------------------------------------------
// (b) The inversion must not have been tightened into uselessness.
// ---------------------------------------------------------------------------

/// The commands the carve-out exists to permit. A `false` here is not a safety
/// failure, but it IS a failure of this module's stated purpose: a tainted turn
/// that cannot run `git status` cannot diagnose itself, which is the measured
/// problem (backlog a4b59893) that motivated the whole module.
#[test]
fn ordinary_read_only_commands_are_still_recognised() {
    for command in [
        "git log --format=%h",
        "head -5 f",
        "sort -k2 f",
        "grep -o foo f",
        "uniq -c f",
        "git worktree list --porcelain",
        "git status",
        "git status --porcelain",
        "git log --oneline",
        "git diff --stat",
        "git diff --cached --name-only",
        "git rev-parse --abbrev-ref HEAD",
        "git rev-parse --show-toplevel",
        "git show --stat HEAD",
        "git blame f",
        "git ls-files --others --exclude-standard",
        "git reflog",
        "git reflog show",
        "ls -la",
        "pwd",
        "whoami",
        "uname -a",
        "wc -l f",
        "cat f",
        "tail -20 f",
        "grep -rn foo .",
        "rg -n foo",
        "rg --files",
        "du -sh .",
        "df -h",
        "stat f",
        "cut -f1 f",
        "diff a b",
        "cmp a b",
        "basename a b",
        "realpath f",
        "readlink -f f",
        "which git",
        "printenv PATH",
        "nl f",
        "echo hello",
    ] {
        assert_readonly(command);
    }
}

/// Pipelines: read-only exactly when EVERY stage is.
#[test]
fn pipelines_are_read_only_exactly_when_every_stage_is() {
    assert_readonly("git status | wc -l");
    assert_readonly("git log --oneline | head -20 | sort");
    assert_readonly("rg -n foo | cut -f1 | sort | uniq -c");

    assert_gated("git status | tee out.txt");
    assert_gated("git status | xargs shred");
    assert_gated("cat f | python3");
    // `||` survives the `|` split as an empty segment.
    assert_gated("git status || shred x");
}

// ---------------------------------------------------------------------------
// (c) ANTI-VACUITY CONTROL.
// ---------------------------------------------------------------------------

/// Without this, every `assert_gated` in this file is satisfied by a broken
/// `is_readonly_bash` — `|_| false` passes all of them, and so does a function
/// that panics on nothing and returns `false` on everything. This test fails
/// under `|_| false`, and the `assert_gated` rows fail under `|_| true`, so the
/// two directions together pin that the classifier DISCRIMINATES rather than
/// merely being conservative.
///
/// It is kept separate from `ordinary_read_only_commands_are_still_recognised`
/// on purpose: that test states an intent ("do not over-tighten") that a future
/// author could legitimately narrow, while this one states a structural
/// property of the suite that must never be removed.
#[test]
fn anti_vacuity_the_classifier_must_return_true_for_something() {
    // If this file's negative assertions are ever all that remains, a constant
    // `false` classifier passes the suite. These three rows forbid that.
    assert!(is_readonly_bash("git status"));
    assert!(is_readonly_bash("pwd"));
    assert!(is_readonly_bash("ls -la"));

    // And the complementary direction: a constant `true` classifier is caught.
    assert!(!is_readonly_bash("shred -u /etc/passwd"));

    // Stated as a single property so the intent survives a careless edit to the
    // rows above: the classifier must not be constant.
    let some_true = ["git status", "pwd", "ls -la"]
        .iter()
        .any(|c| is_readonly_bash(c));
    let some_false = ["shred x", "rm -rf /", "curl http://x"]
        .iter()
        .any(|c| !is_readonly_bash(c));
    assert!(
        some_true && some_false,
        "is_readonly_bash is constant — every other assertion in this file is vacuous"
    );
}

// ---------------------------------------------------------------------------
// (d) The MECHANISM, not just the examples.
//
// The thirteen holes above are instances. If only instances are tested, the
// next hole of the same shape is uncovered. These tests pin the three rules the
// inversion actually consists of.
// ---------------------------------------------------------------------------

/// Rule 1: a `--long` flag must appear BY NAME in that program's table, in both
/// the bare and the `=value` form. The `=value` form is the one that mattered:
/// the old denylist matched prefixes, so a flag it had never heard of passed.
#[test]
fn mechanism_an_unknown_long_flag_is_gated() {
    // Absent from an otherwise-admitted program's table.
    assert_gated("ls --hyperlink");
    assert_gated("ls --hyperlink=auto");
    assert_gated("grep --devices=read f");
    assert_gated("head --nonexistent-flag f");
    assert_gated("rg --type-add=foo:*.x bar");
    assert_gated("sort --files0-from=f");
    assert_gated("tail --follow");
    assert_gated("tail --retry");
    // Absent from the pooled git table.
    assert_gated("git log --textconv");
    assert_gated("git log --ext-diff");
    assert_gated("git diff --output=f");
    assert_gated("git diff --output f");
    assert_gated("git blame --contents=f HEAD");
    // A flag that IS in a DIFFERENT program's table is still absent from this
    // one — the tables are per-program, which is the distinction a single
    // global denylist could not express.
    assert_gated("uniq --lines=3 f");
    assert_gated("sort --only-matching f");
}

/// Rule 2: EVERY letter of a `-abc` bundle must appear in that program's short
/// table. A bundle whose first letters are admitted must not be admitted as a
/// whole — that is the `git grep -O` failure mode generalised.
#[test]
fn mechanism_an_unknown_short_letter_inside_a_bundle_is_gated() {
    assert_readonly("ls -la");
    assert_gated("ls -laY"); // trailing unknown
    assert_gated("ls -Yla"); // leading unknown
    assert_gated("ls -lYa"); // interior unknown

    assert_readonly("head -nq f");
    assert_gated("head -nqZ f");

    assert_readonly("grep -in foo f");
    assert_gated("grep -inO foo f");

    // Pooled git short table, same rule.
    assert_readonly("git log -p");
    assert_gated("git log -pO");
    assert_gated("git status -o");
    assert_gated("git log -c");
}

/// Rule 2, per-program corollary — the single most load-bearing claim of the
/// inversion, and the one a uniform flag rule provably could not make: the SAME
/// letter is admitted for one program and refused for another, because for
/// `grep` `-o` is `--only-matching` and for `sort` it is the OUTPUT FILE.
#[test]
fn mechanism_the_same_short_letter_is_per_program_not_global() {
    assert_readonly("grep -o foo f");
    assert_readonly("rg -o foo");
    assert_gated("sort -o out.txt");
    assert_gated("sort -o out.txt f");
    // `-c` in the other direction: read-only for several programs, and its
    // removal from the old denylist must not have loosened git.
    assert_readonly("wc -c f");
    assert_readonly("grep -c foo f");
    assert_readonly("sort -c f");
    assert_readonly("uniq -c f");
    assert_gated("git -c core.pager=shred status");
}

/// A bare `--` (end of options) is refused: after it, a different tokenizer's
/// rules apply than the one this module models.
///
/// This is over-tight — `git diff -- path` is an ordinary read-only command and
/// is gated — but over-tight is the safe direction (CLAUDE.md §3), and pinning
/// it means a future author who relaxes it does so deliberately.
#[test]
fn mechanism_a_bare_double_dash_is_gated() {
    assert_gated("ls --");
    assert_gated("git diff -- path");
    assert_gated("git log -- .");
    assert_gated("grep -- foo f");
}

/// A bare `-` means stdin to some programs and nothing to others; unmodelled,
/// so refused.
#[test]
fn mechanism_a_bare_single_dash_is_gated() {
    assert_gated("cat -");
    assert_gated("wc -");
    assert_gated("git log -");
    assert_gated("sort -");
}

/// ASCII digits are admitted inside a bundle for every program without
/// appearing in any table (a digit is an argument to the preceding letter,
/// never a verb).
#[test]
fn mechanism_digits_are_admitted_inside_a_bundle() {
    assert_readonly("head -5 f");
    assert_readonly("head -n5 f");
    assert_readonly("tail -20 f");
    assert_readonly("sort -k2 f");
    assert_readonly("grep -5 foo f");
    assert_readonly("git log -3");
    assert_readonly("rg -A2 foo");
    // A digit does not launder a non-digit, non-tabled character.
    assert_gated("sort -k=2 f");
    assert_gated("head -5Y f");
    assert_gated("ls -1Y");
}

/// Rule 3: non-flag operands are COUNTED. Tested at exactly the cap and at one
/// over, for every capped program, because "the cap exists" and "the cap is the
/// right number" are different claims and only the second one is a defence.
#[test]
fn mechanism_operand_counting_at_the_cap_and_one_over() {
    // cap 0
    assert_readonly("pwd");
    assert_gated("pwd x");
    assert_readonly("whoami");
    assert_gated("whoami x");
    assert_readonly("uname -a");
    assert_gated("uname x");

    // cap 1 — `uniq`, where the cap IS the defence (hole 8)
    assert_readonly("uniq f");
    assert_gated("uniq in.txt victim.txt");
    assert_gated("uniq a b c");

    // cap 2
    assert_readonly("basename a b");
    assert_gated("basename a b c");
    assert_readonly("tr a b");
    assert_gated("tr a b c");
    assert_readonly("diff a b");
    assert_gated("diff a b c");
    assert_readonly("cmp a b");
    assert_gated("cmp a b c");

    // uncapped
    assert_readonly("cat a b c d e");
    assert_readonly("git log a b c d e");
}

/// The operand budget is PER PIPELINE STAGE. A shared counter would make
/// `uniq a | uniq b` gated (harmless over-tightening) — but a counter that was
/// reset in the wrong place, or never incremented for later stages, would make
/// `cat f | uniq in.txt victim.txt` READ-ONLY, which is hole 8 reopened behind
/// a pipe.
#[test]
fn mechanism_the_operand_budget_is_per_pipeline_stage() {
    assert_readonly("uniq a | uniq b");
    assert_gated("uniq a | uniq b c");
    assert_gated("cat f | uniq in.txt victim.txt");
    assert_gated("git status | uniq in.txt victim.txt");
}

/// A flag's SEPARATE value is counted as an operand, because this tokenizer
/// does not model which flags take values. Recorded as an observation, not an
/// endorsement: it is why `uniq -f 2 f` is gated. Fail-closed, and pinned so
/// that a future author who teaches the tokenizer about flag arity notices they
/// are removing part of `uniq`'s defence.
#[test]
fn mechanism_a_separate_flag_value_counts_against_the_operand_budget() {
    assert_gated("uniq -f 2 f");
    assert_gated("uniq -w 3 f");
    // Attached values do not consume budget, so the equivalent spelling passes.
    assert_readonly("uniq -f2 f");
}

/// Unknown programs, and every general-purpose executor the module docs say
/// must stay absent.
#[test]
fn unknown_programs_and_executors_are_gated() {
    for command in [
        "shred -u /etc/passwd",
        "rm -rf /",
        "sed -i s/a/b/ f",
        "find . -delete",
        "find . -exec shred {} ;",
        "xargs shred",
        "python3 -m pip",
        "bash script.sh",
        "sh -c shred",
        "node -e x",
        "perl -e x",
        "awk 1 f",
        "tee out.txt",
        "touch out.txt",
        "cargo test",
        "curl http://example.com",
        "wget http://example.com",
        "nc example.com 80",
        "ssh host",
        "chmod 777 f",
    ] {
        assert_gated(command);
    }
}

/// Unknown and mutating `git` subcommands.
#[test]
fn unknown_and_mutating_git_subcommands_are_gated() {
    for command in [
        "git",
        "git config user.name foo",
        "git config --global user.name foo",
        "git checkout main",
        "git switch main",
        "git restore f",
        "git commit -m msg",
        "git add .",
        "git push",
        "git pull",
        "git fetch",
        "git clean -fd",
        "git gc",
        "git stash",
        "git stash list",
        "git tag v1",
        "git branch -D x",
        "git rebase main",
        "git merge x",
        "git apply p.patch",
        "git bisect run shred",
        "git submodule update",
        "git filter-branch",
    ] {
        assert_gated(command);
    }
}

/// A path-qualified program is never classified, so a local executable named
/// after a table entry cannot borrow that entry's verdict.
#[test]
fn path_qualified_programs_are_gated() {
    assert_gated("./ls");
    assert_gated("/usr/bin/ls");
    assert_gated("bin/git status");
    assert_gated(".\\ls");
    assert_gated("C:\\Windows\\System32\\cmd.exe");
}

/// Metacharacters that redirect, substitute, chain or background. Every one of
/// these has a read-only command as its FIRST token, so a classifier that
/// looked only at the program name would pass all of them.
#[test]
fn metacharacters_are_gated() {
    for command in [
        "git status > out.txt",
        "git status >> out.txt",
        "git status 2> err.txt",
        "git status; shred x",
        "git status && shred x",
        "git status || shred x",
        "git status & shred x",
        "echo $(shred x)",
        "echo `shred x`",
        "echo ${HOME}",
        "cat < f",
        "cat <<EOF",
        "ls {a,b}",
        "ls (x)",
        "ls $HOME",
    ] {
        assert_gated(command);
    }
}

/// Quoting is refused because this tokenizer splits on whitespace. Refusing to
/// classify is the honest answer, not a claim of danger.
#[test]
fn quoted_commands_are_gated() {
    assert_gated("git log --format='%h %s'");
    assert_gated("python3 -c \"import os\"");
    assert_gated("grep 'foo bar' f");
    assert_gated("rg \"foo\" .");
}

/// The empty / whitespace-only command classifies nothing, so the answer is
/// `false` — never a vacuous `true` over an empty token stream, which is the
/// "empty set is not clean" rule of CLAUDE.md §3 applied at the input.
#[test]
fn the_empty_command_is_gated() {
    assert_gated("");
    assert_gated("   ");
    assert_gated("\t");
    assert_gated("\n");
    assert_gated("|");
    assert_gated("| |");
}

/// `GIT_SUBCOMMANDS_WITH_ONE_READONLY_SHAPE` discriminates in BOTH directions.
/// Asserting only the gated side would pass even if the pair table never
/// matched anything (fail-closed, but silently useless); asserting only the
/// read-only side would pass even if the table matched everything.
#[test]
fn git_subcommands_read_only_in_exactly_one_shape_discriminate_both_ways() {
    assert_readonly("git reflog");
    assert_readonly("git reflog show");
    assert_readonly("git reflog show --all");
    assert_readonly("git worktree list");
    assert_readonly("git worktree list --porcelain");
    assert_readonly("git worktree list -v");

    assert_gated("git reflog expire --all");
    assert_gated("git reflog delete");
    assert_gated("git worktree add /tmp/x");
    assert_gated("git worktree prune");
    // An option in the verb position is not the one read-only shape.
    assert_gated("git reflog --all");
    assert_gated("git worktree --porcelain");
}
