//! RED (layer 1) for backlog `1e44bfd9` — "reviewgate attributes a peer
//! session's edits to this session".
//!
//! # What this file pins
//!
//! A new harness-core API that does not exist yet:
//!
//! ```ignore
//! pub fn files_edited_by_session(path: &str)
//!     -> harness_core::verdict::Determination<std::collections::BTreeSet<String>>
//! ```
//!
//! `path` is the Stop hook's `transcript_path`
//! ([`harness_core::hook::HookInput::transcript_path`]). The function streams the
//! JSONL transcript forward and collects the `file_path` (or `notebook_path`)
//! argument of every `Edit` / `Write` / `MultiEdit` / `NotebookEdit` `tool_use`
//! block it sees. That set is *this session's* edit footprint — the thing a git
//! working tree provably cannot tell you, because a working tree carries no
//! session identity.
//!
//! The line shape used by the fixtures below is the real one, taken verbatim
//! from a live transcript in
//! `~/.claude/projects/<project>/<session>.jsonl`:
//!
//! ```json
//! {"message":{"role":"assistant","content":[
//!    {"type":"tool_use","id":"toolu_…","name":"Edit",
//!     "input":{"file_path":"/abs/path/x.rs","old_string":"a","new_string":"b"}}]}}
//! ```
//!
//! (`harness_core::hook::HookInput::target`, `src/hook.rs:130-145`, already
//! encodes the same `file_path` / `notebook_path` shape for the PostToolUse
//! payload — this is the transcript-side twin of it.)
//!
//! # Two design points these tests deliberately fix
//!
//! 1. **`Known(∅)` and `Undetermined` are different answers.** A transcript with
//!    no edit tool calls really did observe "this session edited nothing" →
//!    `Known(empty)`. A transcript that *could not be read* (empty path, missing
//!    file) observed nothing at all → `Undetermined`. Collapsing the second into
//!    the first is the whole feature's fail-open: layer 2 narrows the review set
//!    to this set, so `Known(∅)` means "review nothing". CLAUDE.md 第3節.
//!    `missing_transcript_file_is_undetermined_not_known_empty` is the single
//!    most load-bearing assertion in this file.
//!
//! 2. **Paths are returned verbatim.** The transcript records ABSOLUTE paths;
//!    normalising them against a repo root is layer 2's job (it is the only
//!    layer that knows the root), so this layer must not guess. Pinned by
//!    `paths_are_returned_verbatim_not_normalised`.
//!
//! # Streaming
//!
//! `src/transcript.rs:1-6` states the standing policy that a transcript is never
//! loaded whole into memory. Nothing here requires a whole-file API — every test
//! passes a path and reads a `Determination` back. **These tests do NOT prove
//! the implementation streams**; a memory bound is not observable from a
//! `#[test]` without instrumentation this crate does not have. See
//! `large_transcript_still_answers_correctly` for the honest, weaker claim that
//! is actually made.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use harness_core::verdict::Determination;

// The seam every test below goes through. It was a deliberately-wrong stub
// (ignore `path`, always answer `Known(∅)`) while the API did not exist; it now
// forwards to the real function. Nothing else in this file was touched — the
// tests are the contract, and the RED they produced against the stub is
// recorded verbatim in the commit message of b7b4266b.
fn files_edited_by_session(path: &str) -> Determination<BTreeSet<String>> {
    harness_core::transcript::files_edited_by_session(path)
}

// ── fixture helpers ─────────────────────────────────────────────────────────

/// Write `lines` as a JSONL transcript inside `dir` and return its path.
fn transcript(dir: &Path, lines: &[String]) -> PathBuf {
    let p = dir.join("transcript.jsonl");
    let mut body = String::new();
    for l in lines {
        body.push_str(l);
        body.push('\n');
    }
    std::fs::write(&p, body).expect("write transcript");
    p
}

/// One assistant turn carrying a single `tool_use` block, in the real live
/// transcript shape (see the module docstring).
fn tool_use_line(tool: &str, arg_key: &str, arg_value: &str) -> String {
    format!(
        r#"{{"parentUuid":"p","isSidechain":false,"message":{{"model":"claude-opus-5","id":"msg_1","type":"message","role":"assistant","content":[{{"type":"tool_use","id":"toolu_1","name":"{tool}","input":{{"{arg_key}":"{arg_value}","old_string":"a","new_string":"b"}}}}]}}}}"#
    )
}

fn user_line(text: &str) -> String {
    format!(r#"{{"type":"user","message":{{"role":"user","content":"{text}"}}}}"#)
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

/// Unwrap a `Known`, failing loudly (with the reason) on `Undetermined`.
#[track_caller]
fn expect_known(d: Determination<BTreeSet<String>>, what: &str) -> BTreeSet<String> {
    match d {
        Determination::Known(v) => v,
        Determination::Undetermined(why) => {
            panic!("{what}: expected Known(..), got Undetermined({why})")
        }
    }
}

// ── contract ────────────────────────────────────────────────────────────────

/// EXPECTED RED.
///
/// The core positive: every `Edit` in the transcript lands in the set, and a
/// file edited twice appears once (it is a set, not a log).
#[test]
fn edit_tool_uses_are_collected_as_a_known_set() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = transcript(
        dir.path(),
        &[
            user_line("go"),
            tool_use_line("Edit", "file_path", "/repo/src/a.rs"),
            tool_use_line("Edit", "file_path", "/repo/src/b.rs"),
            // same file again — must collapse, not duplicate
            tool_use_line("Edit", "file_path", "/repo/src/a.rs"),
        ],
    );
    let got = expect_known(
        files_edited_by_session(p.to_str().unwrap()),
        "a transcript with Edit tool_use blocks",
    );
    assert_eq!(
        got,
        set(&["/repo/src/a.rs", "/repo/src/b.rs"]),
        "every Edit's file_path must be collected exactly once"
    );
}

/// EXPECTED RED.
///
/// All four edit-shaped tools count, and `NotebookEdit` carries its path under
/// `notebook_path`, not `file_path` — the same split
/// `harness_core::hook::HookInput::target` already makes for PostToolUse
/// (`src/hook.rs:130-145`). Missing `NotebookEdit` here would silently drop a
/// notebook the session really did edit, so layer 2 would then exclude it from
/// review as if a peer had written it.
#[test]
fn write_multiedit_and_notebookedit_are_all_collected() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = transcript(
        dir.path(),
        &[
            tool_use_line("Write", "file_path", "/repo/new.rs"),
            tool_use_line("MultiEdit", "file_path", "/repo/multi.rs"),
            tool_use_line("NotebookEdit", "notebook_path", "/repo/nb.ipynb"),
        ],
    );
    let got = expect_known(
        files_edited_by_session(p.to_str().unwrap()),
        "Write / MultiEdit / NotebookEdit",
    );
    assert_eq!(
        got,
        set(&["/repo/multi.rs", "/repo/nb.ipynb", "/repo/new.rs"]),
        "Write, MultiEdit and NotebookEdit are all edits; NotebookEdit's path \
         lives under `notebook_path`"
    );
}

/// PASSING CONTROL — and the reason `Known` must be a distinct answer.
///
/// A readable transcript that contains no edit tool calls is a real
/// observation: this session edited nothing. That is `Known(∅)`, NOT
/// `Undetermined`. Layer 2 treats the two completely differently (`Known(∅)` →
/// narrow to nothing; `Undetermined` → do not narrow at all), so conflating
/// them here would make layer 2's fail-closed branch unreachable.
///
/// NOTE ON VACUITY: against the stub shim this passes for the wrong reason (the
/// stub answers `Known(∅)` unconditionally). It becomes a real control the
/// moment the shim is repointed, and it is the one test that stops an
/// implementer from "fixing" the RED below by making *everything*
/// `Undetermined`.
#[test]
fn transcript_with_no_edits_is_known_empty_not_undetermined() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = transcript(
        dir.path(),
        &[
            user_line("just talking"),
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"no tools used"}]}}"#.to_string(),
        ],
    );
    let d = files_edited_by_session(p.to_str().unwrap());
    assert!(
        matches!(d, Determination::Known(_)),
        "a READABLE transcript with no edits is an observation (Known), not a \
         failure to observe (Undetermined)"
    );
    let got = expect_known(
        files_edited_by_session(p.to_str().unwrap()),
        "a transcript with no edit tool calls",
    );
    assert!(
        got.is_empty(),
        "no edit tool calls means an empty set, got {got:?}"
    );
}

/// EXPECTED RED. **The most important assertion in this file.**
///
/// A transcript we cannot open is 判定不能 (CLAUDE.md 第3節): we did not observe
/// "this session edited nothing", we observed nothing at all. Answering
/// `Known(∅)` here would make layer 2 narrow every review to the empty set on
/// any transcript hiccup — i.e. it would silently disable reviewgate entirely,
/// which is a far worse fail-open than the mis-attribution bug this feature
/// fixes.
#[test]
fn missing_transcript_file_is_undetermined_not_known_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("no-such-transcript.jsonl");
    assert!(!missing.exists(), "fixture precondition");
    let d = files_edited_by_session(missing.to_str().unwrap());
    match d {
        Determination::Undetermined(_) => {}
        Determination::Known(got) => panic!(
            "an unreadable transcript must be Undetermined (判定不能), not \
             Known({got:?}) — Known(∅) here narrows every downstream review to \
             nothing, turning 'I cannot tell' into 'nothing is mine'"
        ),
    }
}

/// EXPECTED RED.
///
/// `HookInput::transcript_path` defaults to `""` (`#[serde(default)]`,
/// `src/hook.rs:42`), so an empty path is the *normal* shape of "the hook
/// payload did not carry one" — a manual `reviewgate review` run, an older
/// Claude Code, a truncated payload. Same rule as a missing file: we could not
/// look, so we do not know.
#[test]
fn empty_transcript_path_is_undetermined() {
    match files_edited_by_session("") {
        Determination::Undetermined(_) => {}
        Determination::Known(got) => panic!(
            "an empty transcript_path means the hook gave us nothing to read; \
             that is Undetermined, not Known({got:?})"
        ),
    }
}

/// EXPECTED RED.
///
/// PINNED DECISION, of the two the ticket left open: an unparseable line is
/// **skipped**, and the answer stays **`Known`** — because the *file* was
/// readable, and the surrounding lines were real observations. What must NOT
/// happen is the edits on either side of the garbage being dropped: a partial
/// set is the dangerous outcome here, because layer 2 would then exclude a file
/// this session genuinely edited and never say so.
///
/// The alternative (any bad line ⇒ `Undetermined` for the whole file) was
/// rejected: a transcript is appended to live, so a torn final line is routine,
/// and it would make the feature give up almost always — which is safe for
/// review scope (it stops narrowing) but makes the feature dead code, and hides
/// the fact that it never works. If you disagree, change the *implementation*
/// and this test together, in one commit, with the reasoning — do not quietly
/// relax the assertion.
#[test]
fn malformed_line_is_skipped_and_surrounding_edits_survive() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = transcript(
        dir.path(),
        &[
            tool_use_line("Edit", "file_path", "/repo/before.rs"),
            "this is not json at all {{{".to_string(),
            String::new(), // a blank line, too
            r#"{"unterminated": "#.to_string(),
            tool_use_line("Edit", "file_path", "/repo/after.rs"),
        ],
    );
    let d = files_edited_by_session(p.to_str().unwrap());
    assert!(
        matches!(d, Determination::Known(_)),
        "the FILE was readable, so the answer stays Known; only the individual \
         unparseable lines are skipped"
    );
    let got = expect_known(
        files_edited_by_session(p.to_str().unwrap()),
        "a transcript with garbage lines in the middle",
    );
    assert_eq!(
        got,
        set(&["/repo/after.rs", "/repo/before.rs"]),
        "edits on BOTH sides of an unparseable line must survive — a partial \
         set silently un-attributes a file this session really edited"
    );
}

/// EXPECTED RED (anti-vacuity, against an over-broad implementation).
///
/// `Read`, `Grep`, `Bash` and friends are not edits. An implementation that
/// merely greps the transcript for `"file_path"` would attribute a file this
/// session only *looked at* — which re-opens the exact bug, because reading a
/// peer's file is precisely what happens when two sessions share a tree.
///
/// Note the third assertion: without it, this test would pass by returning
/// nothing at all, which is why it is red rather than green against the stub
/// shim (the two exclusion assertions alone would be satisfied by `Known(∅)`).
/// That pairing is the point — an anti-vacuity control has to be able to fail
/// in BOTH directions.
#[test]
fn read_tool_use_is_not_an_edit() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = transcript(
        dir.path(),
        &[
            tool_use_line("Read", "file_path", "/repo/peer_only_read.rs"),
            tool_use_line("Edit", "file_path", "/repo/edited.rs"),
            tool_use_line("Grep", "file_path", "/repo/grepped.rs"),
        ],
    );
    let got = expect_known(
        files_edited_by_session(p.to_str().unwrap()),
        "a transcript mixing Read/Grep with Edit",
    );
    assert!(
        !got.contains("/repo/peer_only_read.rs"),
        "a Read is not an edit; got {got:?}"
    );
    assert!(
        !got.contains("/repo/grepped.rs"),
        "a Grep is not an edit; got {got:?}"
    );
    assert!(
        got.contains("/repo/edited.rs"),
        "the Edit in the same transcript must still be collected — otherwise \
         this test would pass by returning nothing at all; got {got:?}"
    );
}

/// EXPECTED RED.
///
/// Paths come back exactly as the transcript recorded them — absolute, and NOT
/// resolved against any root. This layer has no repo root to resolve against;
/// inventing one (e.g. `std::env::current_dir`) would silently mis-resolve
/// under a `git worktree`, which is the very situation CLAUDE.md 第8節 pushes
/// every session into. Normalisation belongs to layer 2, which knows the root.
#[test]
fn paths_are_returned_verbatim_not_normalised() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = transcript(
        dir.path(),
        &[tool_use_line(
            "Edit",
            "file_path",
            "/abs/elsewhere/crates/x/src/lib.rs",
        )],
    );
    let got = expect_known(
        files_edited_by_session(p.to_str().unwrap()),
        "an absolute transcript path",
    );
    assert_eq!(
        got,
        set(&["/abs/elsewhere/crates/x/src/lib.rs"]),
        "the absolute path must survive verbatim; resolving/relativising it here \
         would guess a root this layer does not have"
    );
}

/// EXPECTED RED — but read the caveat.
///
/// A 5,000-line transcript with edits at the very start and the very end. This
/// pins only that the whole file is traversed and the answer is still correct
/// at a realistic size.
///
/// **It does NOT prove the implementation streams.** A memory bound is not
/// observable from a plain `#[test]`, and writing an assertion that *looked*
/// like it proved streaming (a timing bound, a file-size heuristic) would be a
/// test pretending to check something it cannot — CLAUDE.md 第2節's "what does
/// this test NOT prove". The streaming policy in `src/transcript.rs:1-6` is
/// enforced by review of the implementation, not by this test.
#[test]
fn large_transcript_still_answers_correctly() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut lines = vec![tool_use_line("Edit", "file_path", "/repo/first.rs")];
    for i in 0..5_000 {
        lines.push(user_line(&format!("chatter {i}")));
    }
    lines.push(tool_use_line("Write", "file_path", "/repo/last.rs"));
    let p = transcript(dir.path(), &lines);

    let got = expect_known(
        files_edited_by_session(p.to_str().unwrap()),
        "a 5k-line transcript",
    );
    assert_eq!(
        got,
        set(&["/repo/first.rs", "/repo/last.rs"]),
        "edits at both ends of a long transcript must both be found (the pass \
         must reach the end, and must not forget the beginning)"
    );
}
