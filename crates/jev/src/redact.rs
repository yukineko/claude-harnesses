//! Credential redaction for anything this crate prints.
//!
//! Two layers, because either alone is insufficient:
//!
//! 1. **By exact value.** When the key is known (we are the ones who read it),
//!    the surest redaction is a literal replacement. This catches a key
//!    echoed back in a server error, embedded in a URL, or spliced into a
//!    message in a shape no pattern anticipated.
//! 2. **By pattern.** `curl`'s own diagnostics, or a future code path that
//!    builds a header without going through [`crate::Credential`], can emit a
//!    `Bearer <token>` or a bare `apikey_…` that layer 1 never saw. Patterns
//!    catch those.
//!
//! Both run on every error string that leaves the crate.

/// Prefixes after which the rest of the token is secret.
const SECRET_PREFIXES: [&str; 2] = ["Bearer ", "apikey_"];

/// The replacement. Short and distinctive so it is obvious in a log that
/// redaction happened rather than that a field was empty.
pub const MASK: &str = "[redacted]";

/// Redact `secret` by exact value, then redact anything matching a known
/// credential pattern.
///
/// `secret` may be empty (nothing to replace by value); pattern redaction
/// still runs. A very short `secret` is ignored for exact-value replacement —
/// replacing a 1–3 character string would mangle unrelated text without
/// protecting anything a real key looks like.
pub fn redact(text: &str, secret: &str) -> String {
    let mut out = text.to_string();
    let s = secret.trim();
    if s.chars().count() >= 4 {
        out = out.replace(s, MASK);
    }
    redact_patterns(&out)
}

/// Pattern-only redaction, for text where the key is not in hand.
pub fn redact_patterns(text: &str) -> String {
    let mut out = text.to_string();
    for prefix in SECRET_PREFIXES {
        while let Some(at) = out.find(prefix) {
            let start = at + prefix.len();
            // The token runs to the first whitespace or quote; everything in
            // between is treated as secret.
            let end = out[start..]
                .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == ',')
                .map_or(out.len(), |rel| start + rel);
            if start == end {
                // A bare prefix with nothing after it: mask the prefix itself
                // so the loop terminates and the text still shows that a
                // credential-shaped thing was here.
                out.replace_range(at..end, MASK);
                continue;
            }
            out.replace_range(at..end, MASK);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_value_is_removed() {
        let key = "apikey_abcdefghijklmnop";
        let msg = format!("curl failed talking to https://x/?k={key} (401)");
        let got = redact(&msg, key);
        assert!(!got.contains("abcdefghijklmnop"), "leaked: {got}");
        assert!(got.contains(MASK), "no mask: {got}");
    }

    #[test]
    fn bearer_pattern_is_removed_even_when_the_key_is_unknown() {
        let msg = "sent header Authorization: Bearer sk-live-9f8e7d6c5b4a and got 401";
        let got = redact(msg, "");
        assert!(!got.contains("9f8e7d6c5b4a"), "leaked: {got}");
        assert!(got.contains("and got 401"), "over-redacted: {got}");
    }

    #[test]
    fn apikey_pattern_is_removed_even_when_the_key_is_unknown() {
        let got = redact("unknown token apikey_zzzzzzzzzzzz rejected", "");
        assert!(!got.contains("zzzzzzzzzzzz"), "leaked: {got}");
        assert!(got.contains("rejected"), "over-redacted: {got}");
    }

    #[test]
    fn multiple_occurrences_all_go() {
        let got = redact("Bearer aaaa1111 then Bearer bbbb2222", "");
        assert!(!got.contains("aaaa1111"), "leaked: {got}");
        assert!(!got.contains("bbbb2222"), "leaked: {got}");
    }

    #[test]
    fn a_bare_prefix_terminates_and_does_not_hang() {
        // Regression guard for the search/replace loop: a prefix with an empty
        // token must still make progress.
        let got = redact_patterns("Bearer ");
        assert!(got.contains(MASK), "got {got}");
        let got = redact_patterns("apikey_");
        assert!(got.contains(MASK), "got {got}");
    }

    #[test]
    fn short_secrets_are_not_used_for_exact_replacement() {
        // "ab" must not turn every "ab" in the message into a mask.
        let got = redact("absolutely fine", "ab");
        assert_eq!(got, "absolutely fine");
    }

    #[test]
    fn text_without_credentials_is_unchanged() {
        let msg = "connection timed out after 8s";
        assert_eq!(redact(msg, "apikey_notpresenthere"), msg);
    }
}
