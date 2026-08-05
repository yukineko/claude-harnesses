// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Two defects the independent verifier CONFIRMED against the 0.2.0 inversion,
//! now closed — and their F→P oracle.
//!
//! # These tests were RED when written, and that is their whole value
//!
//! Each one asserts a property `readonly.rs` claimed to hold and that the
//! verifier OBSERVED it not to hold. They were committed RED, before any fix
//! existed, so their present GREEN is evidence rather than decoration: the
//! transition was watched, which is what CLAUDE.md §2(b) requires and what a
//! test written after the fix can never supply.
//!
//! * DEFECT 1 was closed by adding `*` `?` `[` `]` to `FORBIDDEN_CHARS`.
//! * DEFECT 2 was closed by removing `z` from `rg`'s `short` table.
//!
//! They stay as regression tests. If either goes red again, the fix has been
//! undone — and the only admissible response is to restore it, never to delete,
//! `#[ignore]`, or `#[should_panic]` the test (CLAUDE.md §4: 赤いから黙らせる is
//! prohibited; doing so would encode a reachable write as the specification,
//! the exact shape §2 records for `checks_verdict(&[]) == true`).
//!
//! Written by the independent verifier (CLAUDE.md §2(a)); the implementer did
//! not write these, and edited only this header after the fixes landed.

use taintguard::readonly::is_readonly_bash;

fn assert_gated(command: &str) {
    assert!(
        !is_readonly_bash(command),
        "expected {command:?} NOT to be recognised as read-only"
    );
}

/// DEFECT 1 — **file destruction, demonstrated destructively.**
///
/// `max_operands` is `uniq`'s ONLY defence (hole 8: `uniq IN OUT` truncates
/// `OUT`), and `readonly.rs` says so verbatim: "Capping at one operand is the
/// only defence, and no flag table has one."
///
/// But `FORBIDDEN_CHARS` does not contain `*`, `?`, `[` or `]`, and the shell
/// expands globs AFTER this classifier has seen the string. `uniq *` is ONE
/// token, so it counts as ONE operand and passes the cap of 1 — and then bash
/// hands `uniq` however many filenames the glob matched.
///
/// OBSERVED (2026-08-05, git-bash on Windows, GNU coreutils `uniq`), not
/// inferred. In a directory containing `aaa.txt` and `zzz_victim.txt`:
///
/// ```text
/// $ md5sum *.txt
/// 827f73cab5bd4d805c8359fb813c7dec *aaa.txt
/// 099ff8e47378eb9bd24f675cdc5b23aa *zzz_victim.txt
/// $ cat zzz_victim.txt
/// PRECIOUS DATA THAT MUST SURVIVE
/// second line of precious data
/// $ uniq *
/// $ md5sum *.txt
/// 827f73cab5bd4d805c8359fb813c7dec *aaa.txt
/// 4fcc82a88ee38e0aa16c17f512c685c9 *zzz_victim.txt   <-- REPLACED
/// $ cat zzz_victim.txt
/// line1
/// line2
/// ```
///
/// This is hole 8 reopened by a different spelling: the round-2 verifier's
/// `uniq in.txt victim.txt` is now correctly gated, and `uniq *` — which does
/// the same destruction — is not. It is also the strongest available argument
/// for the module's own thesis, since an operand *count* is as unable to state
/// "this writes nothing" as a flag denylist was, whenever the operand list is
/// decided after classification.
///
/// Candidate fixes (verifier does not choose; the implementer owns the design):
/// add `*`, `?`, `[`, `]` to `FORBIDDEN_CHARS`; or refuse any operand
/// containing a glob character for programs whose `max_operands` is finite; or
/// drop `uniq` from `READONLY_PROGRAMS` the way `env`/`hostname` were dropped,
/// on the grounds that its write is reachable through bare arguments that no
/// static count can bound.
#[test]
fn defect_1_glob_expansion_defeats_the_uniq_operand_cap() {
    assert_gated("uniq *");
    assert_gated("uniq *.txt");
    assert_gated("uniq ?.txt");
    assert_gated("uniq [ab].txt");
    assert_gated("uniq -c *");
    assert_gated("uniq f*");
    // Same bypass behind a pipe.
    assert_gated("git status | uniq *");
}

/// DEFECT 2 — **process execution via a flag the tables admit.**
///
/// `z` is in `rg`'s `short` table. For ripgrep, `-z` is NOT grep's
/// `--null-data`; it is `--search-zip`, and ripgrep's own `--help` on this
/// machine (ripgrep 14.1.1) says:
///
/// ```text
///     -z, --search-zip
///         This flag instructs ripgrep to search in compressed files. Currently
///         gzip, bzip2, xz, LZ4, LZMA, Brotli and Zstd files are supported. This
///         option expects the decompression binaries (such as gzip) to be
///         available in your PATH.
/// ```
///
/// i.e. it SPAWNS an external program resolved through `PATH`. `readonly.rs`
/// refuses `--pre` and `--hostname-bin` precisely because they "hand rg an
/// arbitrary program to run"; `-z` hands it a program too. The difference is
/// only that the program's NAME comes from a fixed internal list — its
/// resolution to an executable still goes through `PATH`.
///
/// OBSERVED (2026-08-05), differentially rather than by reading the docs:
///
/// ```text
/// # A: normal PATH, real gzip reachable
/// $ rg -z hello data.txt.gz
/// hello world
/// exit=0
/// # B: a directory placed first on PATH containing a `gzip` that Windows
/// #    cannot CreateProcess
/// $ PATH="...shim:$PATH" rg -z hello data.txt.gz
/// exit=1
/// ```
///
/// The match disappears when `PATH`'s `gzip` changes, which is only possible if
/// `rg -z` executes a `PATH`-resolved binary. The comment in `readonly.rs`
/// asserting the short tables contain nothing that executes is therefore
/// contradicted by the `z` entry (CLAUDE.md §6, LLM 職務 1: docstring ↔ 実挙動).
///
/// The likely provenance of the entry is that `-z` means the harmless
/// `--null-data` for `grep`, and the letter was carried across to `rg`'s table
/// — which is itself the argument for per-program tables working only when each
/// table is derived from THAT tool's `--help`.
///
/// Severity is lower than defect 1 (the attacker does not choose the program
/// name, and reaching it needs a writable `PATH` entry or a planted
/// `gzip`/`xz`/`zstd`), but the module's stated admission rule is "no
/// write-capable form at all", not "unlikely".
#[test]
fn defect_2_rg_dash_z_search_zip_executes_external_binaries() {
    assert_gated("rg -z foo .");
    assert_gated("rg -nz foo .");
    assert_gated("rg -z foo");
}
