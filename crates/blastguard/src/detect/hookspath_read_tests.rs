//! `git config … core.hooksPath`: tell the READ apart from the WRITE by git's
//! own argv grammar (independent RED author; the fix is not part of this file).
//!
//! # What git actually does (observed, not assumed)
//!
//! Measured 2026-11-07 with `git version 2.50.1 (Apple Git-155)` in a scratch
//! repo. The experiments used a stand-in key `core.zzz` (git's option parsing
//! does not depend on the key name; the stand-in kept the experiment itself
//! from being a hooksPath write). Each row: argv, then the key's values in
//! `.git/config` afterwards (it was `ORIG` before every row).
//!
//! | argv                                  | effect                         |
//! |---------------------------------------|--------------------------------|
//! | `git config K`                        | prints `ORIG`; unchanged (READ)|
//! | `git config get K`                    | prints `ORIG`; unchanged (READ)|
//! | `git config --local K`                | prints `ORIG`; unchanged (READ)|
//! | `git config --show-origin K`          | prints origin; unchanged (READ)|
//! | `git config K /tmp/x`                 | `/tmp/x` (WRITE)               |
//! | `git config K ''`                     | empty string (WRITE)           |
//! | `git config set K x`                  | `x` (WRITE)                    |
//! | `git config unset K`                  | removed (WRITE)                |
//! | `git config --unset-all K`            | removed (WRITE)                |
//! | `git config K --add`                  | `--add` (WRITE)                |
//! | `git config K --unset`                | `--unset` (WRITE)              |
//! | `git config K --local`                | `--local` (WRITE)              |
//! | `git config K --`                     | `--` (WRITE)                   |
//! | `git config K -- x`                   | `ORIG` + `--` (WRITE)          |
//! | `git config K --get`                  | `--get` (WRITE)                |
//! | `git config K --get-all`              | `--get-all` (WRITE)            |
//! | `git config K --get-regexp`           | `--get-regexp` (WRITE)         |
//! | `git config K --list` / `K -l`        | `--list` / `-l` (WRITE)        |
//! | `git config K x --get` / `--get-all`  | `ORIG` + `x` (WRITE)           |
//! | `git config --get K x` / `K --unset`  | exit 1; unchanged (READ)       |
//!
//! The rule that explains every row: in the legacy (no-subcommand) form,
//! git STOPS option parsing at the first non-option word. Every word after
//! the key is a positional (value / value-pattern), even when it looks like a
//! flag. So:
//!
//!   * exactly one positional (the key) and only non-mode options BEFORE it
//!     → a read. `git config core.hooksPath` is a READ, and blastguard's
//!     refusal of it is a false positive (the bug this file's first test
//!     pins RED). The new-style `git config get core.hooksPath` is a read
//!     too; on a git older than the `get` subcommand it is the legacy
//!     two-positional form with the invalid key `get` (no section), which git
//!     rejects without writing — a read-or-error in both versions, never a
//!     write, so it is specified as Allow, not left ambiguous.
//!   * a "read" flag (`--get`, `--get-all`, `-l`, …) only selects a read
//!     when it comes BEFORE the key. AFTER the key it is the value that gets
//!     written. HEAD's `config` arm checks for those flags ANYWHERE in the
//!     argv, so `git config core.hooksPath --get` and `git config
//!     core.hooksPath x --get` are Allow at HEAD while git writes them —
//!     a fail-open (second RED test below).
//!
//! What these tests do NOT prove: they exercise `detect` on a plain one-line
//! Bash command only; wrapper / quoting / compound-line routing into the
//! `config` arm is covered (or not) elsewhere. The controls pin `is_deny()` —
//! the verdict observed at HEAD for every one of them — not the reason text.
use super::*;
use serde_json::json;

fn bash(cmd: &str) -> Decision {
    detect("Bash", Some(&json!({ "command": cmd })))
}

/// RED at HEAD: every one of these is a READ in git (table above) and is
/// denied with "the bare one-argument form is refused because it is not told
/// apart from a write".
#[test]
#[ignore = "backlog 8fe3a5c3: open defect (false positive), remove ignore when fixed"]
fn git_config_hookspath_one_positional_read_is_allowed() {
    let reads = [
        "git config core.hooksPath",
        "git config core.hookspath",
        "git config CORE.HOOKSPATH",
        "git config --local core.hooksPath",
        "git config --show-origin core.hooksPath",
        "git config get core.hooksPath",
    ];
    let wrong: Vec<(&str, Decision)> = reads
        .into_iter()
        .map(|c| (c, bash(c)))
        .filter(|(_, d)| *d != Decision::Allow)
        .collect();
    assert!(
        wrong.is_empty(),
        "reads of core.hooksPath not Allow: {wrong:#?}"
    );
}

/// RED at HEAD (fail-open): git WRITES each of these (a read flag placed after
/// the key is a value, see the table), but HEAD allows them because it looks
/// for `--get`/`--list`/… anywhere in the argv.
#[test]
fn git_config_hookspath_read_flag_after_key_is_a_write_and_not_allowed() {
    let writes = [
        "git config core.hooksPath --get",
        "git config core.hooksPath --get-all",
        "git config core.hooksPath --get-regexp",
        "git config core.hooksPath --list",
        "git config core.hooksPath -l",
        "git config core.hooksPath x --get",
        "git config core.hooksPath x --get-all",
    ];
    let allowed: Vec<&str> = writes
        .into_iter()
        .filter(|c| bash(c) == Decision::Allow)
        .collect();
    assert!(
        allowed.is_empty(),
        "hooksPath writes reached Allow: {allowed:?}"
    );
}

/// Control (green at HEAD, must stay green after the fix): every argv git
/// treats as a write — or that sets hooksPath inline — stays denied. Pinned to
/// the verdict HEAD returns for each (Deny).
#[test]
fn git_config_hookspath_writes_stay_denied() {
    let writes = [
        "git config core.hooksPath /tmp/x",
        "git config core.hooksPath ''",
        "git config --global core.hooksPath x",
        "git config --unset core.hooksPath",
        "git config --unset-all core.hooksPath",
        "git config --replace-all core.hooksPath x",
        "git config --add core.hooksPath x",
        "git config set core.hooksPath x",
        "git config unset core.hooksPath",
        "git -c core.hooksPath=x commit",
        // Flag-looking words AFTER the key are values git writes.
        "git config core.hooksPath --add",
        "git config core.hooksPath --unset",
        "git config core.hooksPath --local",
        "git config core.hooksPath --",
        "git config core.hooksPath -- x",
    ];
    let not_denied: Vec<(&str, Decision)> = writes
        .into_iter()
        .map(|c| (c, bash(c)))
        .filter(|(_, d)| !d.is_deny())
        .collect();
    assert!(
        not_denied.is_empty(),
        "hooksPath writes not denied: {not_denied:#?}"
    );
}

/// Control (green at HEAD): the explicit read spelling stays Allow.
#[test]
fn git_config_get_hookspath_stays_allowed() {
    assert_eq!(bash("git config --get core.hooksPath"), Decision::Allow);
}
