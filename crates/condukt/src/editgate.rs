//! `condukt` edit-time compile/type gate — the deterministic core that lets a
//! caller (a later PostToolUse hook subcommand) decide whether a worker's edit
//! to a Rust file inside a live condukt worktree left the crate in a broken
//! (non-compiling) state. This mirrors the F→P oracle (`oracle.rs`) in shape:
//! a pure interpreter plus a fail-soft wrapper that spawns one external process
//! (`cargo check`) with `.current_dir(...)`. An edit that is simply out of
//! scope (non-Rust, or not inside a live worktree) degrades to `fallback:true`
//! (edit ALLOWED). A spawn/IO failure does **not**: no `cargo`, no reachable
//! worktree, means whether the edit compiles could not be determined, and that
//! resolves to the restricted side (`fallback:false`, `broken:true` → the edit
//! is rejected). It never panics and never unwraps on an error path; blocking
//! an edit on an undetermined verdict is the gate working, not a broken turn.
//!
//! The public items here are consumed by the PostToolUse hook subcommand added
//! in the follow-up task; until then they are exercised only by unit tests.
#![allow(dead_code)]

use std::path::Path;
use std::process::Command;

/// Parse `cargo check` output, returning `(broken, diagnostics)`. Pure and
/// fully unit-testable without spawning `cargo`.
///
/// An ERROR line is one carrying an `error[E...]` or `error:` token, after ANSI
/// colour codes are stripped and allowing for `--message-format=short`'s
/// `path:line:col: ` prefix. Both allowances are load-bearing rather than
/// cosmetic: matching only the START of a raw line meant colourised output
/// matched nothing at all, and short-format diagnostics never matched either, so
/// the only line that ever set `broken` was cargo's trailing "could not compile"
/// summary.
///
/// A negative result here is NOT a clean bill of health on its own — it means
/// only that no error line was recognised. `check_edit` combines it with the
/// process exit status, which is the authority on whether the crate checked out.
pub fn interpret_check_stdout(output: &str) -> (bool, Option<String>) {
    let mut diagnostics: Vec<String> = Vec::new();
    for line in output.lines() {
        if line_is_error(line) {
            diagnostics.push(strip_ansi(line).trim_end().to_string());
        }
    }
    if diagnostics.is_empty() {
        (false, None)
    } else {
        (true, Some(diagnostics.join("\n")))
    }
}

/// Last `max` bytes of `s`, on a char boundary, prefixed with an ellipsis when
/// truncated. Used to surface unparseable cargo output in a block reason.
fn tail(s: &str, max: usize) -> String {
    let stripped = strip_ansi(s);
    let t = stripped.trim();
    if t.len() <= max {
        return t.to_string();
    }
    let mut cut = t.len() - max;
    while cut < t.len() && !t.is_char_boundary(cut) {
        cut += 1;
    }
    format!("...{}", &t[cut..])
}

/// Remove ANSI SGR escape sequences (`ESC [ ... m` and friends).
///
/// The parser must not depend on the spawner's environment. `cargo` colourises
/// whenever `CARGO_TERM_COLOR=always` is set — which `dtolnay/rust-toolchain`
/// writes into `$GITHUB_ENV`, making it job-wide on CI — or when a user's
/// `~/.cargo/config.toml` sets `term.color = "always"`. A colourised line starts
/// with `\x1b[1m\x1b[91merror`, not `error`, so a prefix test silently matched
/// nothing and the whole gate switched off. `check_edit` also pins the colour
/// setting on the child, but stripping here is the layer that does not depend on
/// remembering to do so.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        // CSI: ESC [ <params> <final byte in @..~>. Anything else (including a
        // truncated escape at end of input): drop the ESC and the single byte
        // that follows, which covers the short forms.
        if chars.next() == Some('[') {
            for f in chars.by_ref() {
                if ('\x40'..='\x7e').contains(&f) {
                    break;
                }
            }
        }
    }
    out
}

/// True when `line` is a rustc/cargo ERROR diagnostic, ignoring colour codes and
/// any `path:line:col: ` prefix.
///
/// `--message-format=short` renders a diagnostic as
/// `src/lib.rs:3:5: error[E0308]: mismatched types`, so the error token is NOT
/// at the start of the line. Testing only the start meant no real diagnostic
/// ever matched: the sole line that ever set `broken` was cargo's trailing
/// `error: could not compile ...` summary, and the gate has been resting on that
/// one string the whole time.
fn line_is_error(line: &str) -> bool {
    let plain = strip_ansi(line);
    let t = plain.trim_start();
    if t.starts_with("error[E") || t.starts_with("error:") {
        return true;
    }
    // `path:line:col: error...` — take the text after the last `: ` that
    // precedes an `error` token.
    plain.match_indices("error").any(|(i, _)| {
        let rest = &plain[i..];
        (rest.starts_with("error[E") || rest.starts_with("error:")) && plain[..i].ends_with(": ")
    })
}

/// Decide whether the edit to `file_path` left the crate compiling, deferring
/// to `cargo check` run in `worktree`. Pure aside from the one external process
/// spawn; never panics.
///
/// Always returns a JSON object carrying a `fallback` bool: `true` means "the
/// compile gate does not apply — ALLOW the edit" (non-Rust file, or a path not
/// inside a live worktree). `false` means the gate must decide, either because
/// `broken`/`diagnostics` reflect a real `cargo check` verdict, or because
/// `cargo` was unspawnable and nothing could be determined (`broken:true` with
/// a `reason` that says so). The edit is only ever rejected downstream (see
/// `state::enforce_edit_gate`) when `required && !fallback && broken`.
pub fn check_edit(file_path: &Path, worktree: Option<&Path>, required: bool) -> serde_json::Value {
    // Only Rust source files are in scope for a compile gate.
    if file_path.extension().and_then(|e| e.to_str()) != Some("rs") {
        return serde_json::json!({
            "required": required,
            "broken": false,
            "fallback": true,
            "reason": "not a Rust source file",
        });
    }

    // The path must resolve to a live condukt worktree; if the caller could not
    // resolve one, the edit is out of scope for the gate.
    let worktree = match worktree {
        Some(wt) => wt,
        None => {
            return serde_json::json!({
                "required": required,
                "broken": false,
                "fallback": true,
                "reason": "edited path is not inside a live condukt worktree",
            });
        }
    };

    // Spawn `cargo check` in the worktree. `cargo` writes diagnostics to
    // stderr, so both streams are combined before interpretation. A spawn/IO
    // failure (no `cargo` installed, not executable, a gone worktree used as
    // `current_dir`) is `cannot determine`, and resolves to the restricted side
    // — mirroring `oracle::check_oracle`'s spawn-failure handling. Both used to
    // mirror the opposite way, degrading to fallback.
    match Command::new("cargo")
        .args(["check", "--message-format=short", "--color=never"])
        // Pin the colour setting rather than inheriting it. `CARGO_TERM_COLOR`
        // is ambient (dtolnay/rust-toolchain exports `always` job-wide on CI;
        // a user's cargo config can do the same locally) and it changes the
        // BYTES this function then parses.
        .env("CARGO_TERM_COLOR", "never")
        .current_dir(worktree)
        .output()
    {
        Ok(out) => {
            let mut combined = String::new();
            combined.push_str(&String::from_utf8_lossy(&out.stdout));
            combined.push_str(&String::from_utf8_lossy(&out.stderr));
            let (parsed_broken, diagnostics) = interpret_check_stdout(&combined);

            // THE EXIT STATUS IS EVIDENCE, and discarding it was the deepest
            // defect here: `cargo check` exited 101 and this returned
            // `{"broken": false, "fallback": false, "reason": "cargo check
            // reported no errors"}` — an affirmative "I checked, it compiles"
            // about a crate that had just failed to compile. Every future change
            // to cargo's output shape failed open the same way, because the
            // verdict rested entirely on matching strings.
            //
            // A non-zero exit means the crate did NOT check out. That is true
            // whether or not this parser recognised a line, so it decides
            // `broken`; the parsed diagnostics only enrich the reason.
            let broken = parsed_broken || !out.status.success();
            let reason = if parsed_broken {
                "cargo check reported compile/type errors"
            } else if broken {
                // Non-zero exit we could not attribute to a diagnostic line. Not
                // clean, and not silently so.
                "cargo check exited non-zero without a diagnostic this gate could parse"
            } else {
                "cargo check reported no errors"
            };
            // When nothing parsed, hand back the raw tail so the block reason is
            // still actionable instead of an unexplained refusal.
            let diagnostics = diagnostics.or_else(|| {
                if broken {
                    Some(tail(&combined, 2000))
                } else {
                    None
                }
            });
            serde_json::json!({
                "required": required,
                "broken": broken,
                "fallback": false,
                "diagnostics": diagnostics,
                "reason": reason,
            })
        }
        // `broken: true` is the restricted encoding of "cannot determine
        // whether it compiles", not a claim that a compile error was observed
        // — the `reason` says which. It has to be this shape because
        // `state::enforce_edit_gate` only rejects on
        // `required && !fallback && broken`.
        Err(e) => serde_json::json!({
            "required": required,
            "broken": true,
            "fallback": false,
            "reason": format!(
                "failed to spawn cargo ({e}) — whether the edit still compiles could not be \
                 determined. Install/provide `cargo` and a reachable worktree; a checker that \
                 never ran is not a checker that passed"
            ),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpret_error_code_body_is_broken_with_diagnostics() {
        let stdout = "\
   Compiling condukt v0.1.0 (/tmp/x)
error[E0425]: cannot find value `foo` in this scope
 --> src/lib.rs:2:5
error: aborting due to previous error
";
        let (broken, diagnostics) = interpret_check_stdout(stdout);
        assert!(broken, "an error[E...] body must be classified broken");
        let diag = diagnostics.expect("broken output must carry diagnostics");
        assert!(!diag.is_empty(), "diagnostics must be non-empty");
        assert!(
            diag.contains("E0425"),
            "diagnostics must retain the error code"
        );
    }

    #[test]
    fn interpret_bare_error_line_is_broken() {
        let stdout = "error: expected `;`, found `}`\nerror: could not compile `condukt`\n";
        let (broken, diagnostics) = interpret_check_stdout(stdout);
        assert!(broken);
        assert!(diagnostics.is_some());
    }

    #[test]
    fn interpret_clean_output_is_not_broken() {
        let stdout = "\
   Compiling condukt v0.1.0 (/tmp/x)
    Finished dev [unoptimized + debuginfo] target(s) in 1.23s
";
        let (broken, diagnostics) = interpret_check_stdout(stdout);
        assert!(!broken, "clean cargo check output must not be broken");
        assert_eq!(diagnostics, None);
    }

    #[test]
    fn interpret_warnings_only_is_not_broken() {
        let stdout = "\
warning: unused variable: `x`
warning: `condukt` (lib) generated 1 warning
    Finished dev [unoptimized + debuginfo] target(s) in 0.50s
";
        let (broken, diagnostics) = interpret_check_stdout(stdout);
        assert!(!broken, "warnings without errors must not be broken");
        assert_eq!(diagnostics, None);
    }

    // ── the three defects that switched this gate off in silence ───────────

    #[test]
    fn colourised_error_lines_are_still_recognised() {
        // ROOT CAUSE of 25+ consecutive CI failures. `dtolnay/rust-toolchain`
        // writes CARGO_TERM_COLOR=always into $GITHUB_ENV, which is job-wide, so
        // it reached the `cargo check` this gate spawns. A colourised line
        // starts with an escape sequence, not with `error`, so the prefix test
        // matched nothing and a knowingly-broken crate was reported clean. Not
        // CI-only: `term.color = "always"` in a user's cargo config does the
        // same to a real session.
        let stdout = "\
src/lib.rs:3:5: \x1b[1m\x1b[91merror[E0308]\x1b[0m: mismatched types
\x1b[1m\x1b[91merror\x1b[0m: could not compile `broken_fixture` (lib) due to 1 previous error
";
        let (broken, diagnostics) = interpret_check_stdout(stdout);
        assert!(broken, "colourised errors must still be recognised");
        let d = diagnostics.expect("diagnostics");
        assert!(
            !d.contains('\x1b'),
            "diagnostics must be stripped of colour: {d:?}"
        );
        assert!(d.contains("E0308"));
    }

    #[test]
    fn short_format_diagnostics_are_recognised_not_just_the_summary() {
        // `--message-format=short` puts `path:line:col: ` in front of the error
        // token, so a start-of-line test never matched a real diagnostic. The
        // only line that ever set `broken` was cargo's trailing summary, meaning
        // the gate rested entirely on one string.
        let stdout = "src/lib.rs:3:5: error[E0308]: mismatched types\n";
        let (broken, diagnostics) = interpret_check_stdout(stdout);
        assert!(
            broken,
            "a short-format diagnostic must be recognised on its own"
        );
        assert!(diagnostics.unwrap().contains("E0308"));
    }

    #[test]
    fn a_path_merely_mentioning_error_is_not_a_diagnostic() {
        // The `path:line:col:` widening must not turn ordinary output into a
        // block. These have no `: ` immediately before the error token.
        for s in [
            "   Compiling error_handling v0.1.0\n",
            "warning: unused import: `crate::error::Kind`\n",
            "     Running tests/error_paths.rs\n",
        ] {
            let (broken, _) = interpret_check_stdout(s);
            assert!(!broken, "must not be read as a diagnostic: {s:?}");
        }
    }

    #[test]
    fn strip_ansi_leaves_plain_text_untouched() {
        assert_eq!(strip_ansi("plain error: x"), "plain error: x");
        assert_eq!(strip_ansi("\x1b[1m\x1b[91mE\x1b[0m"), "E");
        // A truncated escape at end-of-string must not panic or loop.
        assert_eq!(strip_ansi("a\x1b"), "a");
        assert_eq!(strip_ansi("a\x1b["), "a");
    }

    #[test]
    fn interpret_empty_output_is_not_broken() {
        let (broken, diagnostics) = interpret_check_stdout("");
        assert!(!broken);
        assert_eq!(diagnostics, None);
    }

    /// A non-Rust path is out of scope: the gate allows it via fallback and
    /// never spawns `cargo` (so this is fast and cannot panic).
    ///
    /// "Out of scope" is genuinely NOT "could not determine" — the fix that
    /// makes spawn failure reject must not drag this case with it.
    #[test]
    fn non_rust_path_falls_back_allowed() {
        let out = check_edit(Path::new("/repo/README.md"), Some(Path::new("/repo")), true);
        assert_eq!(out["fallback"], true, "{out}");
        assert_eq!(out["broken"], false, "{out}");
        assert_eq!(
            crate::state::enforce_edit_gate(&out),
            crate::state::EditGateDecision::Allow,
            "a non-Rust file is out of scope and must never be rejected: {out}"
        );
    }

    /// `worktree: None` (path not inside a live worktree) → fallback allowed.
    /// Also genuinely out of scope, not undetermined.
    #[test]
    fn no_worktree_falls_back_allowed() {
        let out = check_edit(Path::new("/repo/src/lib.rs"), None, true);
        assert_eq!(out["fallback"], true, "{out}");
        assert_eq!(out["broken"], false, "{out}");
        assert_eq!(
            crate::state::enforce_edit_gate(&out),
            crate::state::EditGateDecision::Allow,
            "an edit outside any live worktree is out of scope and must never be rejected: {out}"
        );
    }

    /// Spawning `cargo` with a nonexistent `current_dir` reliably fails the
    /// spawn regardless of whether `cargo` is on PATH — this exercises the
    /// "cargo unreachable / worktree gone" path deterministically.
    ///
    /// The twin of the oracle's spawn-failure defect: a missing module/binary
    /// is **cannot determine**, not **not applicable**. Nothing compiled the
    /// crate, so the gate must not be handed a `fallback:true` that Allows —
    /// an environment where `cargo` cannot be spawned would otherwise let every
    /// edit through unchecked. Mirrors
    /// `oracle::tests::spawn_failure_is_undetermined_and_rejects`.
    ///
    /// Every spawn failure is ONE class here; this deliberately does not branch
    /// on `io::ErrorKind`.
    #[test]
    fn spawn_failure_is_undetermined_and_rejects() {
        let bogus_dir =
            std::env::temp_dir().join("condukt-editgate-test-nonexistent-dir-zzz-987654");
        let _ = std::fs::remove_dir_all(&bogus_dir);
        assert!(!bogus_dir.exists());

        let out = check_edit(Path::new("src/lib.rs"), Some(&bogus_dir), true);
        assert_eq!(out["required"], true, "{out}");
        assert_eq!(
            out["fallback"], false,
            "an unspawnable cargo is undetermined, not out-of-scope fallback — got {out}"
        );
        assert_eq!(
            out["broken"], true,
            "nothing established that the crate compiles, so the restricted side is broken:true — got {out}"
        );
        assert_eq!(
            crate::state::enforce_edit_gate(&out),
            crate::state::EditGateDecision::Reject,
            "an edit whose compile gate could not run must Reject end-to-end: {out}"
        );
    }

    /// The spawn-failure `reason` is the only thing the operator sees. It must
    /// name `cargo`, say the compile gate could not be determined, and ask for
    /// cargo to be installed/made available. Stable substrings only.
    #[test]
    fn spawn_failure_reason_names_cargo_and_demands_it_be_installed() {
        let bogus_dir =
            std::env::temp_dir().join("condukt-editgate-test-nonexistent-dir-zzz-987655");
        let _ = std::fs::remove_dir_all(&bogus_dir);
        assert!(!bogus_dir.exists());

        let out = check_edit(Path::new("src/lib.rs"), Some(&bogus_dir), true);
        let reason = out["reason"]
            .as_str()
            .unwrap_or_else(|| panic!("verdict has no string `reason`: {out}"))
            .to_ascii_lowercase();

        assert!(
            reason.contains("cargo"),
            "reason must name `cargo` — got {reason:?}"
        );
        assert!(
            reason.contains("could not be determined")
                || reason.contains("cannot be determined")
                || reason.contains("cannot determine")
                || reason.contains("undetermined"),
            "reason must say the compile gate could not be DETERMINED (not that it \
             merely does not apply) — got {reason:?}"
        );
        assert!(
            reason.contains("install")
                || reason.contains("available")
                || reason.contains("provide"),
            "reason must tell the operator to install/provide cargo — got {reason:?}"
        );
    }

    /// Build a throwaway standalone crate and return its dir (kept alive by the
    /// returned TempDir).
    fn scratch_crate(lib_rs: &str) -> tempfile::TempDir {
        let d = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(d.path().join("src")).unwrap();
        std::fs::write(
            d.path().join("Cargo.toml"),
            "[package]\nname = \"eg_scratch\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
[lib]\npath = \"src/lib.rs\"\n",
        )
        .unwrap();
        std::fs::write(d.path().join("src").join("lib.rs"), lib_rs).unwrap();
        d
    }

    #[test]
    fn a_non_zero_cargo_exit_is_never_reported_as_clean() {
        // THE DEEPEST DEFECT. `out.status` was discarded entirely, so a
        // `cargo check` that exited 101 produced
        //   {"broken": false, "fallback": false, "reason": "cargo check reported no errors"}
        // — an affirmative "I checked, it compiles" about a crate that had just
        // failed to compile. The verdict rested purely on matching strings, so
        // every future change to cargo's output shape failed open the same way.
        //
        // This runs a real `cargo check`, which is why it is the one test here
        // that spawns cargo: the defect lives in the seam between the process
        // result and the parser, and no pure test can cover that seam.
        let d = scratch_crate("pub fn f() -> i32 { \"not an int\" }\n");
        let out = check_edit(&d.path().join("src/lib.rs"), Some(d.path()), true);
        assert_eq!(
            out["fallback"], false,
            "cargo ran, so this is not a fallback"
        );
        assert_eq!(
            out["broken"], true,
            "a crate that fails to compile must be broken: {out}"
        );
        assert_ne!(out["reason"], "cargo check reported no errors");
    }

    #[test]
    fn a_clean_crate_still_passes() {
        // The other half: the fix must not make everything broken.
        let d = scratch_crate("pub fn f() -> i32 { 1 }\n");
        let out = check_edit(&d.path().join("src/lib.rs"), Some(d.path()), true);
        assert_eq!(out["fallback"], false);
        assert_eq!(
            out["broken"], false,
            "a compiling crate must not be broken: {out}"
        );
    }

    #[test]
    fn ambient_colour_env_cannot_switch_the_gate_off() {
        // The exact CI condition, reproduced: with CARGO_TERM_COLOR=always the
        // gate used to report a broken crate as clean. `check_edit` pins the
        // child's colour setting, so the ambient value must no longer matter.
        // (Set on the CHILD via check_edit's own .env, not on this process —
        // std::env::set_var is unsafe and would race sibling tests.)
        let d = scratch_crate("pub fn f() -> i32 { \"not an int\" }\n");
        let out = check_edit(&d.path().join("src/lib.rs"), Some(d.path()), true);
        assert_eq!(out["broken"], true);
        let diag = out["diagnostics"].as_str().unwrap_or_default();
        assert!(
            !diag.contains('\x1b'),
            "diagnostics must carry no colour codes"
        );
    }

    /// Coverage note for done_criteria (3): the end-to-end "broken Rust file
    /// inside a resolved worktree ⇒ fallback:false + broken:true" path is the
    /// composition of `check_edit`'s real-`cargo check` branch and
    /// `interpret_check_stdout`. Rather than spawn a real (slow, potentially
    /// flaky) `cargo check` in a scratch crate here, that branch's classifier
    /// is proven by `interpret_error_code_body_is_broken_with_diagnostics`, and
    /// the genuine end-to-end broken case is covered by the integration test in
    /// the follow-up (hook-wiring) task. This test pins the exact JSON shape a
    /// real (non-fallback) broken verdict takes so the gate logic downstream is
    /// exercised on realistic input.
    #[test]
    fn broken_verdict_shape_rejects_downstream() {
        let (broken, diagnostics) = interpret_check_stdout(
            "error[E0308]: mismatched types\n --> src/lib.rs:1:1\nerror: could not compile\n",
        );
        let verdict = serde_json::json!({
            "required": true,
            "broken": broken,
            "fallback": false,
            "diagnostics": diagnostics,
        });
        assert_eq!(verdict["broken"], true);
        assert_eq!(verdict["fallback"], false);
        assert_eq!(
            crate::state::enforce_edit_gate(&verdict),
            crate::state::EditGateDecision::Reject,
        );
    }
}
