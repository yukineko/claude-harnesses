//! Lexical guard: this crate's **code** must never name the repo's gate
//! verdict type, and `Advice` must never grow an approving variant.
//!
//! # Why a lexical test and not a type-level one
//!
//! The property is an absence ("no path from a jev answer to a gate
//! decision"), and absences are not expressible as a Rust signature. What a
//! type *can* do here is make the collapse awkward, which `Advice` already
//! does by having no approving variant. This test covers the remaining way the
//! property could be lost: someone adding `impl From<Advice> for Verdict`, or
//! a `Clean` variant, in a diff that otherwise looks reasonable.
//!
//! # Honest limits of this check
//!
//! It strips whole-line `//` comments and then searches the remaining text. It
//! therefore does **not** understand block comments, string literals, or
//! macros, and it would miss a `Verdict` mentioned only inside a `/* … */`
//! block. It is a ratchet against the ordinary case, not a proof. The
//! substantive guarantee is the one in `advice.rs`: there is no variant that
//! means "fine", so there is nothing for a conversion to usefully produce.
//!
//! For the approving-variant half, the compiler is in fact stricter than
//! this test. Measured 2026-10-02 by injecting `Clean` into the enum:
//! because `Advice` is not `#[non_exhaustive]` and `Advice::render`
//! matches it exhaustively, the injection failed to BUILD with
//! `error[E0004]: non-exhaustive patterns` before any test could run.
//! `advice_has_no_approving_variant` below is the backstop for a variant
//! added together with the arms that would silence that error.
//!
//! # What is deliberately allowed
//!
//! `harness_core::verdict::Determination` and `Required` — note the lowercase
//! module path, which is not the type token this test looks for. Using them is
//! convergence on the repo's shared three-valued container (CLAUDE.md §3 asks
//! crates not to reinvent it), and neither carries a verdict: the only
//! `Verdict` reachable from a `Required::Blocked` is `Verdict::Undetermined`,
//! which blocks. There is no route to `Verdict::Clean`, which needs
//! harness-core's private `Evidence` witness plus findings a check actually
//! collected.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn rust_sources() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![src_dir()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let p = entry.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|e| e == "rs") {
                out.push(p);
            }
        }
    }
    out.sort();
    assert!(
        !out.is_empty(),
        "found no sources to scan under {:?}",
        src_dir()
    );
    out
}

/// Drop whole-line `//` comments (doc comments included) so prose that
/// *discusses* the contract does not trip the guard that enforces it.
fn code_only(text: &str) -> String {
    text.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn no_source_file_names_the_gate_verdict_type() {
    // Assembled at runtime so this file is not its own counterexample.
    let needle = format!("{}{}", "Verd", "ict");
    let mut offenders = Vec::new();
    for path in rust_sources() {
        let text = std::fs::read_to_string(&path).unwrap();
        let code = code_only(&text);
        for (i, line) in code.lines().enumerate() {
            if line.contains(&needle) {
                offenders.push(format!("{}:{}: {}", path.display(), i + 1, line.trim()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "jev must not couple to the gate verdict type (CLAUDE.md §7 — an external \
         service never holds block/allow authority). Offending lines:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn no_source_file_converts_anything_into_a_verdict() {
    let forbidden = [
        format!("{}{}", "into_", "verdict"),
        format!("{}{}", "from_", "findings"),
        "adjudicate".to_string(),
    ];
    let mut offenders = Vec::new();
    for path in rust_sources() {
        let code = code_only(&std::fs::read_to_string(&path).unwrap());
        for f in &forbidden {
            if code.contains(f.as_str()) {
                offenders.push(format!("{}: {}", path.display(), f));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "jev must not mint or forward a gate verdict:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn advice_has_no_approving_variant() {
    let advice = std::fs::read_to_string(src_dir().join("advice.rs")).unwrap();
    let code = code_only(&advice);
    // Variant declarations only ever appear as `Name {` or `Name,` inside the
    // enum; these spellings would all mean "jev approved".
    for banned in ["Clean", "Approved", "Pass {", "Ok {", "Allow"] {
        assert!(
            !code.contains(banned),
            "advice.rs must not grow an approving variant ({banned:?}). \
             \"jev says it is fine\" and \"jev never ran\" are the same value on purpose."
        );
    }
}

#[test]
fn the_scan_is_not_vacuous() {
    // A guard against the guards: if `code_only` ever started returning
    // nothing, every assertion above would pass for the wrong reason.
    let advice = std::fs::read_to_string(src_dir().join("advice.rs")).unwrap();
    let code = code_only(&advice);
    assert!(
        code.contains("pub enum Advice"),
        "code_only stripped real code; the other tests in this file would be vacuous"
    );
    // And the token the first test looks for must genuinely be findable when
    // it is present.
    let needle = format!("{}{}", "Verd", "ict");
    let sample = format!("let v: {needle} = x;");
    assert!(code_only(&sample).contains(&needle));
    // ...while a comment mentioning it is correctly ignored.
    let commented = format!("// a comment about {needle}");
    assert!(!code_only(&commented).contains(&needle));
}
