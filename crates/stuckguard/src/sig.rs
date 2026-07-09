//! Turn a tool call into a stable *signature* (so identical actions collide) and
//! pull out edit details (file + before/after hashes) so we can spot revert
//! thrash. Hashing uses `DefaultHasher` (fixed-seed, deterministic across
//! processes) — we only need collision-equality, not crypto.

use std::collections::hash_map::DefaultHasher;
use std::collections::BTreeSet;
use std::hash::{Hash, Hasher};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One observed tool event, persisted in the per-session ring buffer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub seq: u64,
    pub tool: String,
    /// Normalized signature: identical actions share it.
    pub sig: String,
    /// Token bag derived from the same normalized body that produced `sig`
    /// (whitespace-split, deduplicated). Used for near-repeat detection via
    /// Jaccard similarity when two events share the same `tool` but hash to
    /// different `sig`s. `#[serde(default)]` so events persisted by older
    /// stuckguard builds (before this field existed) still deserialize.
    #[serde(default)]
    pub tokens: BTreeSet<String>,
    /// For edit-like tools, the target file (for thrash detection).
    #[serde(default)]
    pub file: Option<String>,
    /// Hash of the removed text (Edit.old_string).
    #[serde(default)]
    pub old_h: Option<u64>,
    /// Hash of the inserted text (Edit.new_string / Write.content).
    #[serde(default)]
    pub new_h: Option<u64>,
    /// Best-effort: did the tool response look like a failure?
    #[serde(default)]
    pub error: bool,
    /// Stable hash of the NORMALIZED error text, present only when `error` is
    /// true. Volatile parts (line numbers, absolute paths, addresses/temp
    /// values, timestamps) are stripped before hashing so the *same class* of
    /// error collides to the same digest even when the raw text differs in
    /// those details — this is the key a recurring error class is looked up
    /// by in the cross-project lessons store. `None` for non-error events and
    /// for older persisted events (backward-compatible via serde default).
    #[serde(default)]
    pub failed_test_digest: Option<String>,
}

fn hash(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// Collapse runs of whitespace to a single space and trim, so cosmetically
/// different invocations of the same command still collide.
fn norm(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

/// Split the same normalized body used to compute `sig` into a deduplicated
/// token bag (whitespace-split on the raw body, plus the `\u{1}` field
/// separators used to join multi-field signatures like Edit's
/// `file\u{1}old\u{1}new`). Pure/deterministic — no embeddings, no RAG. Used
/// for Jaccard-similarity near-repeat detection.
fn tokenize(body: &str) -> BTreeSet<String> {
    body.split(|c: char| c.is_whitespace() || c == '\u{1}')
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

/// Build an Event (minus `seq`) from a tool call. `None` for tools we can't
/// signature (no name).
///
/// `near_repeat_enabled` should be `cfg.similarity_threshold < 1.0` at the
/// call site. Near-repeat detection is the only consumer of `tokens`
/// (`detect::is_repeat_of` never inspects it once `similarity_threshold >=
/// 1.0`, returning `false` before touching the token bag), so at the default
/// (disabled) setting there is no need to pay the tokenize cost — nor to
/// persist a token bag nobody reads into the per-session state file — on
/// every single tool call. When `false`, `tokens` is left empty and `sig`
/// (used by exact-repeat detection) is computed exactly as before, so
/// default behavior is unchanged.
pub fn build(
    tool: &str,
    input: Option<&Value>,
    response: Option<&Value>,
    near_repeat_enabled: bool,
) -> Option<Event> {
    if tool.trim().is_empty() {
        return None;
    }
    let inp = input.cloned().unwrap_or(Value::Null);
    let mut file = None;
    let mut old_h = None;
    let mut new_h = None;

    let sig_body = match tool {
        "Bash" => norm(field(&inp, "command").unwrap_or("")),
        "Edit" | "MultiEdit" => {
            let f = field(&inp, "file_path").unwrap_or("").to_string();
            let old = field(&inp, "old_string").unwrap_or("");
            let new = field(&inp, "new_string").unwrap_or("");
            old_h = Some(hash(old));
            new_h = Some(hash(new));
            file = Some(f.clone());
            format!("{f}\u{1}{old}\u{1}{new}")
        }
        "Write" => {
            let f = field(&inp, "file_path").unwrap_or("").to_string();
            let content = field(&inp, "content").unwrap_or("");
            old_h = Some(hash("")); // write replaces wholesale
            new_h = Some(hash(content));
            file = Some(f.clone());
            format!("{f}\u{1}{content}")
        }
        "Read" => field(&inp, "file_path").unwrap_or("").to_string(),
        "Grep" => format!(
            "{}\u{1}{}",
            field(&inp, "pattern").unwrap_or(""),
            field(&inp, "path").unwrap_or("")
        ),
        "Glob" => field(&inp, "pattern").unwrap_or("").to_string(),
        // Fallback: hash the whole input blob.
        _ => serde_json::to_string(&inp).unwrap_or_default(),
    };

    let error = response.map(looks_error).unwrap_or(false);
    let failed_test_digest = if error {
        response.map(|r| error_digest(&error_text(r)))
    } else {
        None
    };

    // Early-exit branch (finding 16): skip the tokenize pass — and therefore
    // the extra bytes that would otherwise be persisted per event in the
    // session state file — whenever near-repeat detection is disabled.
    let tokens = if near_repeat_enabled {
        tokenize(&sig_body)
    } else {
        BTreeSet::new()
    };

    Some(Event {
        seq: 0,
        tool: tool.to_string(),
        sig: format!("{tool}:{:016x}", hash(&sig_body)),
        tokens,
        file,
        old_h,
        new_h,
        error,
        failed_test_digest,
    })
}

/// Best-effort failure detection from a tool response.
fn looks_error(resp: &Value) -> bool {
    match resp {
        Value::Object(m) => {
            if m.get("is_error").and_then(Value::as_bool) == Some(true) {
                return true;
            }
            if m.get("error").map(|e| !e.is_null()).unwrap_or(false) {
                return true;
            }
            if let Some(code) = m.get("exit_code").and_then(Value::as_i64) {
                if code != 0 {
                    return true;
                }
            }
            false
        }
        _ => false,
    }
}

/// Best-effort extraction of the human-readable error text from a tool
/// response, so it can be normalized and hashed into a `failed_test_digest`.
/// Looks at the fields tool responses actually use for failure text
/// (`error`, `message`, `stderr`, `output`, `content`), falling back to the
/// whole response blob so no error is left un-digested just because its
/// shape doesn't match a known field.
fn error_text(resp: &Value) -> String {
    match resp {
        Value::Object(m) => {
            for key in ["error", "message", "stderr", "output", "content"] {
                if let Some(v) = m.get(key) {
                    match v {
                        Value::String(s) if !s.is_empty() => return s.clone(),
                        Value::Array(_) | Value::Object(_) => {
                            let s = serde_json::to_string(v).unwrap_or_default();
                            if !s.is_empty() && s != "null" {
                                return s;
                            }
                        }
                        _ => {}
                    }
                }
            }
            serde_json::to_string(resp).unwrap_or_default()
        }
        _ => serde_json::to_string(resp).unwrap_or_default(),
    }
}

/// Strip the volatile parts of an error message (absolute filesystem paths,
/// line/column numbers, hex addresses/temp-value-like tokens, ISO-ish
/// timestamps, and other bare numbers) so that two error strings from the
/// *same class* of failure — differing only in those details — normalize to
/// identical text. Deterministic and pure: same input always yields the same
/// output. Order matters: paths are collapsed first (so digits embedded in a
/// path don't get half-stripped by the numeric pass), then remaining numeric
/// tokens (line numbers, timestamps, hex addresses, temp values) are
/// collapsed, then whitespace is normalized.
fn normalize_error_text(s: &str) -> String {
    use std::sync::OnceLock;

    // Absolute-ish paths: a run starting with `/` (or `\`, for completeness)
    // followed by path-segment characters, e.g. `/home/user/proj/src/f.rs`
    // or `/tmp/foo-12345/bar`.
    static PATH_RE: OnceLock<regex::Regex> = OnceLock::new();
    let path_re = PATH_RE.get_or_init(|| regex::Regex::new(r"[/\\][A-Za-z0-9_.\-/\\]+").unwrap());

    // Any run of digits (with optional embedded `:`, `-`, `.`, or hex `a-f`
    // after a `0x` prefix) — covers line numbers, column numbers, addresses
    // (`0x7f3a…`), timestamps (`12:34:56`, `2026-07-09`), and generic temp
    // values/offsets.
    static NUM_RE: OnceLock<regex::Regex> = OnceLock::new();
    let num_re =
        NUM_RE.get_or_init(|| regex::Regex::new(r"0[xX][0-9a-fA-F]+|\d[\d:._\-]*\d|\d").unwrap());

    let no_paths = path_re.replace_all(s, "<PATH>");
    let no_nums = num_re.replace_all(&no_paths, "<NUM>");
    norm(&no_nums)
}

/// Deterministic hash of the normalized error text — the `failed_test_digest`
/// value. Same class of error (post-normalization) → same digest; a
/// different error → a different digest.
fn error_digest(raw: &str) -> String {
    format!("{:016x}", hash(&normalize_error_text(raw)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn event_without_failed_test_digest_field_deserializes_as_none() {
        // Simulates a persisted Event written by an older stuckguard build,
        // before `failed_test_digest` existed — the field must be absent
        // from the JSON and deserialization must still succeed, defaulting
        // to None (serde `#[serde(default)]`).
        let old_json = r#"{
            "seq": 1,
            "tool": "Bash",
            "sig": "Bash:0000000000000001",
            "error": true
        }"#;
        let e: Event = serde_json::from_str(old_json).expect("old-shape Event must still parse");
        assert_eq!(e.failed_test_digest, None);
        assert!(e.error);
        assert_eq!(e.file, None);
    }

    #[test]
    fn bash_normalizes_whitespace() {
        let a = build("Bash", Some(&json!({"command": "cargo  test"})), None, true).unwrap();
        let b = build("Bash", Some(&json!({"command": "cargo test"})), None, true).unwrap();
        assert_eq!(a.sig, b.sig);
    }

    #[test]
    fn edit_captures_hashes_and_file() {
        let e = build(
            "Edit",
            Some(&json!({"file_path": "a.rs", "old_string": "A", "new_string": "B"})),
            None,
            true,
        )
        .unwrap();
        assert_eq!(e.file.as_deref(), Some("a.rs"));
        assert_eq!(e.old_h, Some(hash("A")));
        assert_eq!(e.new_h, Some(hash("B")));
    }

    #[test]
    fn error_detected_from_response() {
        let e = build(
            "Bash",
            Some(&json!({"command": "x"})),
            Some(&json!({"exit_code": 1})),
            true,
        )
        .unwrap();
        assert!(e.error);
    }

    #[test]
    fn non_error_event_has_no_digest() {
        let e = build(
            "Bash",
            Some(&json!({"command": "cargo test"})),
            Some(&json!({"exit_code": 0})),
            true,
        )
        .unwrap();
        assert!(!e.error);
        assert_eq!(e.failed_test_digest, None);

        // Even with no response at all.
        let e2 = build("Bash", Some(&json!({"command": "cargo test"})), None, true).unwrap();
        assert!(!e2.error);
        assert_eq!(e2.failed_test_digest, None);
    }

    #[test]
    fn error_event_has_digest() {
        let e = build(
            "Bash",
            Some(&json!({"command": "cargo test"})),
            Some(&json!({
                "exit_code": 1,
                "stderr": "thread 'main' panicked at src/main.rs:42:5:\nassertion failed"
            })),
            true,
        )
        .unwrap();
        assert!(e.error);
        assert!(e.failed_test_digest.is_some());
    }

    #[test]
    fn same_class_error_differing_only_in_volatile_details_has_same_digest() {
        // Same panic class, different line number and different absolute path.
        let a = build(
            "Bash",
            Some(&json!({"command": "cargo test"})),
            Some(&json!({
                "exit_code": 1,
                "stderr": "thread 'main' panicked at /home/alice/proj/src/main.rs:42:5:\nassertion `left == right` failed"
            })),
            true,
        )
        .unwrap();
        let b = build(
            "Bash",
            Some(&json!({"command": "cargo test"})),
            Some(&json!({
                "exit_code": 1,
                "stderr": "thread 'main' panicked at /home/bob/other/src/main.rs:917:12:\nassertion `left == right` failed"
            })),
            true,
        )
        .unwrap();
        assert_eq!(
            a.failed_test_digest, b.failed_test_digest,
            "same error class (differing only in path/line number) must hash equal"
        );

        // A genuinely different error class must hash differently.
        let c = build(
            "Bash",
            Some(&json!({"command": "cargo test"})),
            Some(&json!({
                "exit_code": 1,
                "stderr": "error[E0308]: mismatched types"
            })),
            true,
        )
        .unwrap();
        assert_ne!(
            a.failed_test_digest, c.failed_test_digest,
            "a genuinely different error must hash differently"
        );
    }

    #[test]
    fn normalize_error_text_absorbs_line_numbers_and_paths_and_temp_values() {
        // Two superficially-different-but-same-class raw strings: different
        // line/column numbers, different absolute paths (different users /
        // tmp dirs), different hex address, different timestamp — must
        // normalize to byte-identical text.
        let a = "panic at /home/alice/repo/src/lib.rs:10:3 (addr 0x7f3a1b2c) at 2026-07-09T12:00:00Z during teardown";
        let b = "panic at /home/bob/otherrepo/src/lib.rs:284:19 (addr 0x55ffee00) at 2026-01-01T00:00:00Z during teardown";
        let na = normalize_error_text(a);
        let nb = normalize_error_text(b);
        assert_eq!(
            na, nb,
            "normalization must absorb line/path/addr/timestamp variation:\na={na}\nb={nb}"
        );
        assert_eq!(error_digest(a), error_digest(b));

        // Sanity: the normalized text still contains the stable words
        // (only the volatile path/line/addr/timestamp tokens are stripped).
        assert!(na.contains("panic at"));
        assert!(na.contains("during teardown"));
    }

    #[test]
    fn tokenize_skipped_when_near_repeat_disabled() {
        // finding 16: at the default (near-repeat disabled) setting, `tokens`
        // must stay empty — the tokenize pass (and the bytes it would add to
        // the persisted session state) is only paid when near-repeat
        // detection is actually enabled.
        let e = build(
            "Bash",
            Some(&json!({"command": "cargo test -p stuckguard foo"})),
            None,
            false,
        )
        .unwrap();
        assert!(
            e.tokens.is_empty(),
            "tokens must be empty when near_repeat_enabled is false: {:?}",
            e.tokens
        );
        // `sig` (exact-repeat detection) must be computed exactly the same
        // regardless of the near-repeat flag.
        let e_enabled = build(
            "Bash",
            Some(&json!({"command": "cargo test -p stuckguard foo"})),
            None,
            true,
        )
        .unwrap();
        assert_eq!(e.sig, e_enabled.sig);
    }

    #[test]
    fn tokenize_populated_when_near_repeat_enabled() {
        let e = build(
            "Bash",
            Some(&json!({"command": "cargo test -p stuckguard foo"})),
            None,
            true,
        )
        .unwrap();
        assert!(
            !e.tokens.is_empty(),
            "tokens must be populated when near_repeat_enabled is true"
        );
    }
}
