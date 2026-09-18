//! t3 done_criteria (4): "harness-core の interrogate.rs に IO (ファイル読み/
//! subprocess/ネットワーク) が 1 箇所も追加されていない — IO は hypothesis 側の
//! 呼び出し層にある".
//!
//! `interrogate.rs`'s own doc comment already states a purity invariant ("no
//! file I/O, no network, no LLM, no `AskUserQuestion`") — see
//! `crates/harness-core/src/interrogate.rs:18-20`. This is a lexical regression
//! guard for that invariant: it greps the *source text* of `interrogate.rs` for
//! tokens that would indicate IO was added directly to this module (as opposed
//! to being added in a caller, e.g. the hypothesis crate's `draft` command),
//! and fails if any appear outside the module's own doc comment.
//!
//! Unlike most other t3 tests, this one is NOT expected to fail today:
//! `interrogate.rs` currently has zero IO tokens, so this starts green. It is a
//! regression trap for the implementation phase (t3 must add `draft`/`scope`
//! support to the `hypothesis` crate WITHOUT reaching into this module to do
//! it), not evidence of a missing feature.
//!
//! What this test does NOT prove:
//! - It does not detect IO performed *indirectly* — e.g. a helper function in
//!   another `harness_core` module that `interrogate.rs` calls, or IO reached
//!   via a trait method whose implementation lives elsewhere and is invoked
//!   through the `RigorGates` trait object. Lexical scanning only sees tokens
//!   literally written in this file's own text.
//! - It does not detect IO smuggled in via a macro expansion, a `build.rs`, or
//!   a proc-macro attribute that isn't spelled out as one of the literal
//!   tokens below.
//! - It does not distinguish "IO in a doc example" from "IO in real code" any
//!   more precisely than skipping `//!`/`///` comment lines; a `#[doc(hidden)]`
//!   test module inside this file that legitimately needs IO (there is none
//!   today) would trip this guard and require an explicit exception, not a
//!   silent pass.
//! - Passing this test says nothing about whether `evaluate`/`apply` remain
//!   otherwise pure (e.g. reading global mutable state, using a non-monotonic
//!   clock) — only that the specific IO surface (files/process/network) named
//!   in item 4 is absent from this file's literal text.

use std::path::PathBuf;

/// Tokens that would indicate direct IO if they appear in `interrogate.rs`'s
/// non-comment source text. Deliberately narrow (named APIs/paths), not a
/// blanket ban on the word "io" (which would also flag legitimate uses like
/// `std::fmt` formatting or the word "authority").
const FORBIDDEN_IO_TOKENS: [&str; 11] = [
    "std::fs",
    "std::net",
    "std::process::Command",
    "tokio::fs",
    "tokio::net",
    "reqwest",
    "hyper::",
    "ureq::",
    "File::open",
    "File::create",
    "TcpStream",
];

fn interrogate_rs_source() -> String {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let path = manifest_dir.join("src").join("interrogate.rs");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("must be able to read {}: {e}", path.display()))
}

/// Strip `//!` and `///` doc-comment lines so a token mentioned *in prose*
/// (like this module's own purity-invariant doc comment, which names "file
/// I/O" and "network" as things it forbids) doesn't self-trigger the guard.
fn strip_doc_comments(src: &str) -> String {
    src.lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            !(trimmed.starts_with("//!") || trimmed.starts_with("///"))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn interrogate_rs_has_no_direct_io_tokens_outside_doc_comments() {
    let src = interrogate_rs_source();
    let code_only = strip_doc_comments(&src);

    let offenders: Vec<&str> = FORBIDDEN_IO_TOKENS
        .iter()
        .copied()
        .filter(|tok| code_only.contains(tok))
        .collect();

    assert!(
        offenders.is_empty(),
        "interrogate.rs must stay IO-free (file/process/network) per its own \
         purity invariant doc comment; found forbidden token(s) in non-comment \
         source: {offenders:?}. IO for a `draft`/scope feature belongs in the \
         hypothesis crate's calling layer, not in this pure evaluate/apply core."
    );
}

/// Guard the guard: prove the token list and comment-stripping actually catch
/// something, using an in-memory fixture (NOT interrogate.rs itself) so this
/// test doesn't depend on interrogate.rs staying clean to pass.
#[test]
fn forbidden_token_scan_detects_planted_io_and_ignores_doc_comments() {
    let planted = "//! std::fs would be mentioned here in prose, that's fine\n\
                    fn f() {\n    std::fs::read_to_string(\"x\").unwrap();\n}\n";
    let code_only = strip_doc_comments(planted);
    assert!(
        !code_only.contains("//!"),
        "doc-comment line must be stripped"
    );
    let hit = FORBIDDEN_IO_TOKENS
        .iter()
        .any(|tok| code_only.contains(tok));
    assert!(hit, "planted std::fs call in code must be detected");
}
