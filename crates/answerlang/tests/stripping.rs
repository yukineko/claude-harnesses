#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod common;
use common::*;

/// A Japanese answer (60 JP letters, so the 40-letter rule cannot excuse it)
/// plus an English payload that is only English *inside a stripped construct*.
/// If stripping were missing, the English letters would dominate and block.
fn assert_stripped(name: &str, payload: String) {
    let env = Env::new();
    env.write_config(JA_CONFIG);
    let text = format!("{}\n{}\n", ja(60), payload);
    let t = env.answer_transcript("t.jsonl", &text);
    let out = env.stop("s1", Some(&t), false);
    assert_eq!(out.code, 0);
    assert!(
        !out.is_block(),
        "{name}: should be stripped, got {}",
        out.stdout
    );
}

/// Control: the same English outside any stripped construct does block.
fn assert_counted(name: &str, payload: String) {
    let env = Env::new();
    env.write_config(JA_CONFIG);
    let text = format!("{}\n{}\n", ja(60), payload);
    let t = env.answer_transcript("t.jsonl", &text);
    let out = env.stop("s1", Some(&t), false);
    assert!(
        out.is_block(),
        "{name}: control should block, got {}",
        out.stdout
    );
}

#[test]
fn control_plain_english_counts() {
    assert_counted("plain", en(300));
}

#[test]
fn fenced_backtick_block_is_stripped() {
    assert_stripped("```", format!("```rust\n{}\n```", en(300)));
}

#[test]
fn fenced_tilde_block_is_stripped() {
    assert_stripped("~~~", format!("~~~\n{}\n~~~", en(300)));
}

#[test]
fn multi_line_fenced_block_is_stripped_whole() {
    let body: Vec<String> = (0..10).map(|_| en(40)).collect();
    assert_stripped("fence-multi", format!("```\n{}\n```", body.join("\n")));
}

#[test]
fn inline_code_is_stripped() {
    let spans: Vec<String> = (0..10).map(|_| format!("`{}`", en(30))).collect();
    assert_stripped("inline", spans.join(" "));
}

#[test]
fn blockquote_lines_are_stripped() {
    let lines: Vec<String> = (0..6).map(|_| format!("> {}", en(50))).collect();
    assert_stripped("blockquote", lines.join("\n"));
}

#[test]
fn urls_are_stripped() {
    let urls: Vec<String> = (0..8)
        .map(|_| "https://example.com/documentation/getting-started/overview".to_string())
        .collect();
    assert_stripped("urls", urls.join(" "));
}

#[test]
fn paths_are_stripped() {
    let paths: Vec<String> = (0..15)
        .map(|_| "crates/answerlang/src/detection/language.rs".to_string())
        .collect();
    assert_stripped("paths", paths.join(" "));
    let home: Vec<String> = (0..15).map(|_| "~configdirectory".to_string()).collect();
    assert_stripped("tilde paths", home.join(" "));
}

#[test]
fn quote_lines_are_stripped() {
    assert_stripped("$ line", format!("$ {}", en(300)));
    assert_stripped("test result", format!("test result: ok. {}", en(300)));
    assert_stripped("error:", format!("error: {}", en(300)));
    assert_stripped("error[", format!("error[E0308]: {}", en(300)));
    assert_stripped("warning:", format!("warning: {}", en(300)));
    assert_stripped("indented $", format!("   $ {}", en(300)));
}

#[test]
fn fully_quoted_lines_are_stripped() {
    assert_stripped("dq", format!("\"{}\"", en(300)));
    assert_stripped("corner brackets", format!("「{}」", en(300)));
}

#[test]
fn hex_ids_are_not_letters() {
    let ids: Vec<String> = (0..40).map(|i| format!("a1fdc6{:02}", i)).collect();
    assert_stripped("hex ids", ids.join(" "));
}

#[test]
fn run_ids_are_not_letters() {
    let ids: Vec<String> = (0..150)
        .map(|_| "run-20261112-072718-37987".to_string())
        .collect();
    assert_stripped("run ids", ids.join(" "));
}

#[test]
fn name_line_tokens_are_not_letters() {
    let ids: Vec<String> = (0..40).map(|_| "main.rs:42".to_string()).collect();
    assert_stripped("name:line", ids.join(" "));
    let ids2: Vec<String> = (0..40).map(|_| "lib.rs:10:5".to_string()).collect();
    assert_stripped("name:line:col", ids2.join(" "));
}

#[test]
fn version_tokens_are_not_letters() {
    let v: Vec<String> = (0..300).map(|_| "v1.2".to_string()).collect();
    assert_stripped("versions", v.join(" "));
}

#[test]
fn markdown_markers_do_not_count_as_letters() {
    // Smoke test only: markdown markers (-, *, #) are never alphabetic under
    // char::is_alphabetic, so this cannot discriminate a stripping rule; it documents
    // that bullets/headings add no letters.
    assert_stripped("bullets", format!("- {}\n## {}", "*", "#"));
}

#[test]
fn japanese_only_in_stripped_parts_does_not_save_an_english_answer() {
    let env = Env::new();
    env.write_config(JA_CONFIG);
    let text = format!("{}\n```\n{}\n```\n`{}`\n", en(200), ja(300), ja(100));
    let t = env.answer_transcript("t.jsonl", &text);
    assert!(env.stop("s1", Some(&t), false).is_block());
}
