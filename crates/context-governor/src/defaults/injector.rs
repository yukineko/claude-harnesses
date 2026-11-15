//! Default [`ContextInjector`] — heading-addressable retrieval injection
//! (UserPromptSubmit). On each user turn the injector scores spec sections by
//! heading–prompt term overlap and either injects the matched section body or,
//! when no heading scores, a cheap table-of-contents sentinel. Pinned/normative
//! content is never dropped — the injector only *adds* `additionalContext`
//! beside the prompt, never replacing it (I1-adjacent).
//!
//! The implementation is tokenizer-free and deterministic: ASCII word overlap
//! (split on non-alphanumeric, lowercase, len ≥ 2). No model call, no API key,
//! no embeddings. The private `inject_for` core keeps env/process state out of
//! the tested logic; the public trait method resolves the reference doc from
//! `CONTEXT_GOVERNOR_REFERENCE_DOC` and delegates.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashSet;
use std::hash::{Hash, Hasher};

use super::classifier::{split_sections, Section};
use crate::handlers::ContextInjector;
use crate::io::HookOutput;
use crate::ledger::{was_injected_in_session, Action, Ledger, LedgerNode};
use crate::types::ItemId;
use harness_core::hook::HookInput;

// ── tokenizer ─────────────────────────────────────────────────────────────────

/// Tokenize `text` into a set of lowercase ASCII word tokens with length ≥ 2.
/// Non-alphanumeric characters are word separators. The minimum length of 2
/// prevents ultra-common single-letter tokens from polluting the score.
fn tokenize(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 2)
        .map(|w| w.to_lowercase())
        .collect()
}

/// Heading-overlap score: count of prompt tokens that appear in `heading`'s
/// token set. Pure term-overlap — no TF-IDF, no embeddings.
fn score_heading(prompt_tokens: &HashSet<String>, heading: &str) -> usize {
    let heading_tokens = tokenize(heading);
    prompt_tokens
        .iter()
        .filter(|t| heading_tokens.contains(*t))
        .count()
}

// ── ToC builder ───────────────────────────────────────────────────────────────

/// Build a compact table-of-contents string from `sections`. Sections with
/// an empty heading (preamble) are omitted — they have no addressable title.
/// Returns an empty string when no named headings exist.
fn build_toc(sections: &[Section]) -> String {
    let headings: Vec<String> = sections
        .iter()
        .filter(|s| !s.heading.is_empty())
        .map(|s| format!("- {}", s.heading))
        .collect();
    if headings.is_empty() {
        return String::new();
    }
    format!("Reference sections:\n{}", headings.join("\n"))
}

// ── DefaultInjector ───────────────────────────────────────────────────────────

pub struct DefaultInjector;

impl DefaultInjector {
    /// Testable core: given a raw `prompt` string and the full reference `doc`,
    /// select the most relevant section and emit the appropriate `HookOutput`.
    ///
    /// Selection strategy (deterministic, no model call):
    /// 1. Split `doc` into sections via the shared ATX-heading parser.
    /// 2. Score each section by heading–prompt token overlap.
    /// 3. If the best score > 0, inject that section's heading + body as
    ///    `additionalContext` (heading-addressable retrieval).
    /// 4. If no heading scores > 0, inject the table-of-contents sentinel.
    /// 5. If `doc` is empty / produces no sections, return `{}` (no-op).
    ///
    /// The injector only *adds* context; it never replaces the prompt (I1-adjacent).
    pub(crate) fn inject_for(&self, prompt: &str, doc: &str) -> HookOutput {
        let sections = split_sections(doc);
        if sections.is_empty() {
            return HookOutput::default();
        }

        let prompt_tokens = tokenize(prompt);

        // Score every section by heading-token overlap with the prompt.
        // `max_by_key` is stable on ties (returns the last maximum), giving
        // document-order preference to later sections on ties — deterministic.
        let best = sections
            .iter()
            .map(|s| (score_heading(&prompt_tokens, &s.heading), s))
            .max_by_key(|(score, _)| *score);

        match best {
            Some((score, section)) if score > 0 => {
                // Heading-addressable retrieval: the relevant turn gets the matched section.
                let text = if section.heading.is_empty() {
                    section.body.clone()
                } else {
                    format!("{}\n{}", section.heading, section.body)
                };
                HookOutput::inject("UserPromptSubmit", text)
            }
            _ => {
                // Irrelevant turn: cheap ToC sentinel so the model knows what's
                // retrievable without paying the full reference-body cost.
                let toc = build_toc(&sections);
                if toc.is_empty() {
                    HookOutput::default()
                } else {
                    HookOutput::inject("UserPromptSubmit", toc)
                }
            }
        }
    }
}

/// Stable, cross-process content id for an injected text — the key for the
/// ledger seen-state used to dedup repeated reference injection. `DefaultHasher`
/// uses fixed keys (not the randomized `RandomState`), so identical text hashes
/// to the same id across separate hook invocations within a session.
fn injection_id(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

impl ContextInjector for DefaultInjector {
    /// Resolve the reference doc from `CONTEXT_GOVERNOR_REFERENCE_DOC` (a file
    /// path). If unset or unreadable, returns `{}` — never break a turn (hook
    /// invariant). Otherwise delegates to `inject_for` which contains all
    /// testable logic.
    ///
    /// When `inject_for` produces an `additionalContext` envelope (either the
    /// heading-match path or the ToC-sentinel path), this method emits exactly
    /// one `Action::Injected` ledger row as a pure side effect. The `{}` no-op
    /// paths (env unset, unreadable file, empty doc, no sections, no headings)
    /// never emit a row. The dedup and the row are keyed on the hook input's
    /// `session_id`; an input with an empty `session_id` returns `{}` and writes
    /// no row, because no session ledger can answer its dedup. Except for those
    /// `{}` cases (empty `session_id`, or a dedup hit), the returned `HookOutput`
    /// is byte-identical to what `inject_for` would return — the ledger write is
    /// an invisible side effect.
    fn inject(&self, input: &HookInput) -> HookOutput {
        let path = match std::env::var("CONTEXT_GOVERNOR_REFERENCE_DOC") {
            Ok(p) if !p.is_empty() => p,
            _ => return HookOutput::default(),
        };
        let doc = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => return HookOutput::default(),
        };
        let out = self.inject_for(&input.prompt, &doc);

        // Emit one ledger row iff the call actually injected something.
        // Both the heading-match path and the ToC-sentinel path set
        // `additional_context` to a non-empty `Some`; the `{}` no-op leaves it
        // `None`. Reading the field directly avoids fragile string matching on
        // the serialised output and keeps this check O(1).
        if let Some(text) = out
            .specific
            .as_ref()
            .and_then(|s| s.additional_context.as_deref())
            .filter(|t| !t.is_empty())
        {
            // Dedup (I6 observe→act): if this exact text was already injected in
            // this session — observed via the ledger seen-state — skip the
            // re-injection entirely and write no new row, so the model never pays
            // for the same reference body twice.
            //
            // The dedup reads and the row is written in the ledger of the session
            // named by the hook input's `session_id` — not `CLAUDE_CODE_SESSION_ID`
            // (backlog 04f53dbe). An input with no `session_id` cannot name the
            // ledger that would answer the dedup, so the injector does not inject
            // rather than guess a session (no env fallback, no `default` bucket).
            if input.session_id.is_empty() {
                return HookOutput::default();
            }
            let id = injection_id(text);
            if was_injected_in_session(&input.cwd, &input.session_id, id) {
                return HookOutput::default();
            }
            let resident = (text.chars().count().div_ceil(4)).max(1) as u32;
            let node = LedgerNode {
                session: input.session_id.clone(),
                hook: "UserPromptSubmit".to_string(),
                item: Some(ItemId(id)),
                action: Action::Injected,
                reason: "reference-injection",
            };
            Ledger::open_session(&input.cwd, &input.session_id).append(&node, resident);
        }

        out
    }
}

// ── unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // Serialise all tests that mutate process-global env vars. Unit tests in
    // this binary run in parallel by default, and `CONTEXT_GOVERNOR_REFERENCE_DOC`
    // is shared state — without a lock, `inject_without_env_var_returns_empty_object`
    // (which removes the var) and `inject_emits_ledger_row_on_injection` (which sets
    // it) would race and flake each other.
    // Env mutation is serialised via the crate-shared `acquire_env_lock()` (in
    // defaults::guard) so injector's env tests cannot race groomer / snapshot /
    // backing, all of which mutate the same process-global vars.
    use crate::defaults::guard::acquire_env_lock as ENV_MUTEX_LOCK;

    /// Spec doc with one normative section (API Contracts), two reference
    /// sections (Endpoints, Glossary) and one example section.
    const DOC: &str = "\
# API Contracts
All endpoints must return JSON.
Status codes MUST follow RFC 7231.

# Endpoints
GET /users — list all users
POST /users — create a user

# Examples
```json
{ \"id\": 1, \"name\": \"Alice\" }
```

# Glossary
Token: an opaque string identifying a session.
";

    fn make_input(prompt: &str) -> HookInput {
        serde_json::from_value(serde_json::json!({
            "session_id": "test-session",
            "transcript_path": "",
            "cwd": "",
            "hook_event_name": "UserPromptSubmit",
            "prompt": prompt,
        }))
        .unwrap()
    }

    // ── heading-match path ────────────────────────────────────────────────────

    #[test]
    fn inject_for_matching_heading_returns_section_body() {
        // "endpoints" overlaps with the "Endpoints" heading token set.
        let out = DefaultInjector.inject_for("list all endpoints", DOC);
        let json = out.to_json();

        assert!(
            json.contains("\"additionalContext\""),
            "expected additionalContext envelope; got: {json}"
        );
        assert!(
            json.contains("\"hookEventName\":\"UserPromptSubmit\""),
            "hookEventName must be UserPromptSubmit; got: {json}"
        );
        // The Endpoints section body contains the route lines.
        assert!(
            json.contains("GET /users") || json.contains("POST /users"),
            "matched section body not found in: {json}"
        );
    }

    // ── ToC path ─────────────────────────────────────────────────────────────

    #[test]
    fn inject_for_irrelevant_prompt_returns_toc_not_section_body() {
        // "weather" / "today" do not appear in any heading — ToC path fires.
        let out = DefaultInjector.inject_for("what is the weather like today?", DOC);
        let json = out.to_json();

        assert!(
            json.contains("\"additionalContext\""),
            "expected additionalContext in ToC path; got: {json}"
        );
        assert!(
            json.contains("Reference sections:"),
            "ToC prefix not found in: {json}"
        );
        // Heading names appear in ToC.
        assert!(
            json.contains("API Contracts"),
            "ToC should list headings; got: {json}"
        );
        // Deep section body (route lines) must NOT appear in the ToC path.
        assert!(
            !json.contains("GET /users — list all users"),
            "deep section body must be withheld on the ToC path; got: {json}"
        );
    }

    // ── env-unset path ────────────────────────────────────────────────────────

    #[test]
    fn inject_without_env_var_returns_empty_object() {
        let _g = ENV_MUTEX_LOCK();
        // Guard: ensure the env var is not set for this test.
        std::env::remove_var("CONTEXT_GOVERNOR_REFERENCE_DOC");
        let input = make_input("show me the endpoints");
        let out = DefaultInjector.inject(&input);
        assert_eq!(out.to_json(), "{}", "unset env var must produce no-op {{}}");
    }

    // ── empty doc ─────────────────────────────────────────────────────────────

    #[test]
    fn inject_for_empty_doc_returns_empty_object() {
        let out = DefaultInjector.inject_for("anything", "");
        assert_eq!(out.to_json(), "{}", "empty doc must produce no-op {{}}");
    }

    // ── ledger emission ───────────────────────────────────────────────────────

    /// Verify that calling `inject` (the public trait method) with a matching
    /// prompt appends exactly one `injected` ledger row, while calling it with
    /// `CONTEXT_GOVERNOR_REFERENCE_DOC` unset appends no row at all.
    ///
    /// Design constraints observed here:
    /// * `inject_for` is NOT called directly — it is a pure core with no
    ///   side effects; all ledger assertions go through `inject`.
    /// * A fresh `tempfile::tempdir()` is used for `CONTEXT_GOVERNOR_STATE_DIR`
    ///   so this test's ledger never bleeds into other tests' ledgers.
    /// * A unique `cwd` value makes `rollup(cwd)` read only this test's rows.
    /// * Both branches (inject and no-op) are covered in one test function to
    ///   keep all env-mutation inside the mutex guard.
    #[test]
    fn inject_emits_ledger_row_on_injection() {
        use std::io::Write as _;
        let _g = ENV_MUTEX_LOCK();

        // Write a small reference doc to a named temp file.
        let mut doc_file = tempfile::NamedTempFile::new().expect("temp doc file");
        doc_file
            .write_all(b"# Endpoints\nGET /users - list all users\nPOST /users - create\n")
            .expect("write doc");
        doc_file.flush().expect("flush");

        // Fresh state dir so the ledger is isolated from every other test.
        let state_dir = tempfile::tempdir().expect("temp state dir");

        // Unique cwd: rollup(cwd) will only see rows from this test.
        let cwd = format!("/nonexistent/injector-emit-test-{}", std::process::id());

        // ── injection path ────────────────────────────────────────────────────
        std::env::set_var("CONTEXT_GOVERNOR_REFERENCE_DOC", doc_file.path());
        std::env::set_var("CONTEXT_GOVERNOR_STATE_DIR", state_dir.path());
        // The injector writes the ledger of the input's `session_id`, while
        // `rollup(cwd)` reads the ledger of `CLAUDE_CODE_SESSION_ID`. Pin the env
        // to the input's session so `rollup` reads the ledger the injector wrote
        // (and the no-op check below reads that same session, not an ambient one).
        let prev_sid = std::env::var_os("CLAUDE_CODE_SESSION_ID");
        std::env::set_var("CLAUDE_CODE_SESSION_ID", "ledger-test-session");

        let input: HookInput = serde_json::from_value(serde_json::json!({
            "session_id": "ledger-test-session",
            "transcript_path": "",
            "cwd": cwd,
            "hook_event_name": "UserPromptSubmit",
            "prompt": "show me the endpoints",
        }))
        .expect("build HookInput");

        let out = DefaultInjector.inject(&input);
        // The returned HookOutput must carry additionalContext (heading-match).
        assert!(
            out.to_json().contains("\"additionalContext\""),
            "inject must produce additionalContext for a matching prompt; got: {}",
            out.to_json()
        );

        // Exactly ONE injected row must appear in the ledger for this cwd.
        let summary = crate::ledger::rollup(&cwd);
        assert_eq!(
            summary.per_event.get("injected").copied(),
            Some(1),
            "expected exactly 1 injected ledger row; summary: {summary:?}"
        );

        // ── no-op path (env unset) ────────────────────────────────────────────
        // Use a distinct cwd so rollup only counts rows from the no-op call.
        let cwd_noop = format!("/nonexistent/injector-emit-noop-{}", std::process::id());
        std::env::remove_var("CONTEXT_GOVERNOR_REFERENCE_DOC");

        let input_noop: HookInput = serde_json::from_value(serde_json::json!({
            "session_id": "ledger-test-session",
            "transcript_path": "",
            "cwd": cwd_noop,
            "hook_event_name": "UserPromptSubmit",
            "prompt": "show me the endpoints",
        }))
        .expect("build noop HookInput");

        let out_noop = DefaultInjector.inject(&input_noop);
        assert_eq!(
            out_noop.to_json(),
            "{}",
            "unset env var must produce no-op {{}}"
        );

        // The ledger for the noop cwd must be empty.
        let summary_noop = crate::ledger::rollup(&cwd_noop);
        assert_eq!(
            summary_noop.rows, 0,
            "no-op inject must not append any ledger rows; summary: {summary_noop:?}"
        );

        // Clean up env.
        std::env::remove_var("CONTEXT_GOVERNOR_STATE_DIR");
        match prev_sid {
            Some(v) => std::env::set_var("CLAUDE_CODE_SESSION_ID", v),
            None => std::env::remove_var("CLAUDE_CODE_SESSION_ID"),
        }
    }

    /// Dedup (I6 observe→act): repeating the *same* injection within a session is
    /// skipped to a no-op with no new ledger row, while a *distinct* section
    /// still injects. The ledger is the seen-state.
    #[test]
    fn inject_dedups_repeated_identical_injection() {
        use std::io::Write as _;
        let _g = ENV_MUTEX_LOCK();

        // Two distinct headings so we can hit different sections (different text).
        let mut doc_file = tempfile::NamedTempFile::new().expect("temp doc file");
        doc_file
            .write_all(
                b"# Endpoints\nGET /users - list all users\n\n# Auth\nUse a bearer token in the Authorization header\n",
            )
            .expect("write doc");
        doc_file.flush().expect("flush");

        let state_dir = tempfile::tempdir().expect("temp state dir");
        let cwd = format!("/nonexistent/inject-dedup-{}", std::process::id());

        std::env::set_var("CONTEXT_GOVERNOR_REFERENCE_DOC", doc_file.path());
        std::env::set_var("CONTEXT_GOVERNOR_STATE_DIR", state_dir.path());
        std::env::set_var("CLAUDE_CODE_SESSION_ID", "dedup-test-session");

        let mk = |prompt: &str| -> HookInput {
            serde_json::from_value(serde_json::json!({
                "session_id": "dedup-test-session",
                "transcript_path": "",
                "cwd": cwd,
                "hook_event_name": "UserPromptSubmit",
                "prompt": prompt,
            }))
            .expect("build HookInput")
        };

        let injected_rows = || {
            crate::ledger::rollup(&cwd)
                .per_event
                .get("injected")
                .copied()
        };

        // 1st injection (Endpoints): injects + exactly one row.
        let out1 = DefaultInjector.inject(&mk("show me the endpoints"));
        assert!(
            out1.to_json().contains("additionalContext"),
            "first injection must produce additionalContext"
        );
        assert_eq!(injected_rows(), Some(1));

        // 2nd identical injection (same section text): deduped → {} and no new row.
        let out2 = DefaultInjector.inject(&mk("show me the endpoints"));
        assert_eq!(
            out2.to_json(),
            "{}",
            "a repeated identical injection must dedup to a no-op"
        );
        assert_eq!(
            injected_rows(),
            Some(1),
            "dedup must not append a second injected row"
        );

        // A different prompt hitting a DIFFERENT section (Auth) → distinct text →
        // still injects (the dedup is content-keyed, not blanket suppression).
        let out3 = DefaultInjector.inject(&mk("how does auth and the bearer token work"));
        assert!(
            out3.to_json().contains("additionalContext"),
            "a distinct section must still inject"
        );
        assert_eq!(
            injected_rows(),
            Some(2),
            "a new distinct injection must append a row"
        );

        std::env::remove_var("CONTEXT_GOVERNOR_REFERENCE_DOC");
        std::env::remove_var("CONTEXT_GOVERNOR_STATE_DIR");
        std::env::remove_var("CLAUDE_CODE_SESSION_ID");
    }

    /// An input with an empty `session_id` names no ledger that could answer the
    /// dedup, so the injector returns `{}` and writes no row anywhere — it does
    /// not fall back to the env session or a `default` bucket. CONTROL: the same
    /// prompt with a non-empty `session_id` does inject, so the `{}` is caused by
    /// the missing session, not by the doc or prompt.
    #[test]
    fn inject_without_input_session_id_does_not_inject_or_record() {
        use std::io::Write as _;
        let _g = ENV_MUTEX_LOCK();

        let mut doc_file = tempfile::NamedTempFile::new().expect("temp doc file");
        doc_file
            .write_all(b"# Endpoints\nGET /users - list all users\n")
            .expect("write doc");
        doc_file.flush().expect("flush");

        let state_dir = tempfile::tempdir().expect("temp state dir");
        let cwd = format!("/nonexistent/inject-no-session-{}", std::process::id());
        let prev_sid = std::env::var_os("CLAUDE_CODE_SESSION_ID");
        std::env::set_var("CONTEXT_GOVERNOR_REFERENCE_DOC", doc_file.path());
        std::env::set_var("CONTEXT_GOVERNOR_STATE_DIR", state_dir.path());
        std::env::set_var("CLAUDE_CODE_SESSION_ID", "no-input-session-env");

        let mk = |session: &str| -> HookInput {
            serde_json::from_value(serde_json::json!({
                "session_id": session,
                "transcript_path": "",
                "cwd": cwd,
                "hook_event_name": "UserPromptSubmit",
                "prompt": "show me the endpoints",
            }))
            .expect("build HookInput")
        };

        let out = DefaultInjector.inject(&mk(""));
        assert_eq!(out.to_json(), "{}", "empty session_id must not inject");
        let any_ledger = walk_has_ledger(state_dir.path());
        assert!(
            !any_ledger,
            "empty session_id must not write a ledger row in any session"
        );

        let control = DefaultInjector.inject(&mk("no-input-session-control"));
        assert!(
            control.to_json().contains("additionalContext"),
            "control: a non-empty session_id must inject; got {}",
            control.to_json()
        );
        assert!(
            walk_has_ledger(state_dir.path()),
            "control: a non-empty session_id must write a ledger row"
        );

        std::env::remove_var("CONTEXT_GOVERNOR_REFERENCE_DOC");
        std::env::remove_var("CONTEXT_GOVERNOR_STATE_DIR");
        match prev_sid {
            Some(v) => std::env::set_var("CLAUDE_CODE_SESSION_ID", v),
            None => std::env::remove_var("CLAUDE_CODE_SESSION_ID"),
        }
    }

    /// True iff any `ledger.jsonl` exists under `dir` (recursive).
    fn walk_has_ledger(dir: &std::path::Path) -> bool {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return false;
        };
        entries.flatten().any(|e| {
            let p = e.path();
            if p.is_dir() {
                walk_has_ledger(&p)
            } else {
                p.file_name().is_some_and(|n| n == "ledger.jsonl")
            }
        })
    }

    // ── proptest invariant ────────────────────────────────────────────────────

    proptest! {
        /// inject_for never panics and never emits `updatedToolOutput` (which
        /// would replace the prompt — correctness violation). The output must be
        /// either `{}` (no-op) or an `additionalContext` envelope.
        #[test]
        fn inject_for_never_panics_or_replaces_prompt(prompt in ".*", doc in ".*") {
            let out = DefaultInjector.inject_for(&prompt, &doc);
            let json = out.to_json();
            prop_assert!(
                !json.contains("updatedToolOutput"),
                "inject_for must never emit updatedToolOutput (would replace prompt): {json}"
            );
        }
    }
}
