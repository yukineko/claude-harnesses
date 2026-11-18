//! UTF-8 boundary safety regressions (multi-byte text must not panic).
//! Moved out of `tests.rs` unchanged, which is at its file-growth cap.

use super::*;

// ---- UTF-8 boundary safety (regression: multi-byte text must not panic) ----

#[test]
fn multibyte_after_redirect_marker_does_not_panic() {
    // '頻' = U+983B = E9 A0 BB. Byte 0xA0, cast `as char`, is U+00A0
    // (no-break space / whitespace) — the old byte-scan broke mid-char here
    // and `seg[start..j]` panicked ("not a char boundary"). A `>` adjacent
    // to Japanese prose (common in this repo's task text) must classify
    // without crashing.
    let _ = redirect_targets("band>=1・低頻度で再注入");
    let _ = redirect_targets("値を >低頻度 に絞る");
    let _ = bash("echo band>=1・低頻度で再注入");
}

#[test]
fn split_segments_preserves_multibyte_text() {
    // Byte-as-char split used to mangle non-ASCII into Latin-1 mojibake.
    let segs = split_segments("echo 低頻度 && ls 監査");
    assert_eq!(segs.len(), 2);
    assert!(segs[0].contains("低頻度"));
    assert!(segs[1].contains("監査"));
}

#[test]
fn redirect_target_after_multibyte_is_extracted_intact() {
    // A genuine truncating redirect whose target follows multi-byte text is
    // still read, and the extracted token is valid UTF-8 (no mid-char cut).
    assert_eq!(
        single_redirect_target("低頻度 > out.txt"),
        Some("out.txt".to_string())
    );
}

#[test]
fn kebab_case_angle_placeholder_is_not_a_redirect() {
    // `<run-id>` / `<pdo-unit-id>` are placeholder prose, not redirects — the
    // hyphen must not break placeholder recognition (regression: these were
    // force-denied as `>` truncating redirects).
    assert!(single_redirect_target("overwatch begin --key <pdo-unit-id> --title x").is_none());
    assert!(single_redirect_target("condukt heartbeat --run <run-id>").is_none());
    assert!(!bash("echo state --run <session-id> done").is_deny());
}

#[test]
fn hyphenated_real_redirect_still_detected() {
    // A real truncating redirect with a hyphenated target (no preceding `<`)
    // is still caught — allowing hyphens only matches true `<...>` pairs.
    assert_eq!(
        single_redirect_target("foo-bar > out-file.txt"),
        Some("out-file.txt".to_string())
    );
    assert!(bash("cat x > important-data.txt").is_deny());
}
