// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The gates' operator escape hatch must be SESSION-SCOPED, reason-required,
//! one-shot and recorded — not a file in the shared project root.
//!
//! ## What is wrong with the thing this replaces
//!
//! `gate::run::consume_skip(root, ".<gate>-skip")` reads a marker from the
//! PROJECT ROOT and deletes it, so it applies once. `root` is shared by every
//! concurrent session working in that checkout, and the marker carries no
//! attribution at all. So the skip session A created is consumed by whichever
//! session's Stop hook fires next — waving THAT session's legitimate gate
//! through on an exception it never asked for. CLAUDE.md §5 names this exact
//! mechanism as forbidden under parallel sessions:
//!
//! > 並行セッションがあるときは project root の共有 skip ファイルを使わない
//! > (一度だけ消費されるため、他セッションの正当なゲートを素通りさせる)
//!
//! It was nevertheless the ONLY in-session hatch that existed: the documented
//! `*_DISABLE` env vars never reach a Stop hook (the hook is a child of the
//! Claude Code app and inherits ITS environment, not the Bash tool's) and
//! `~/.claude/settings.json` is permission-denied for editing.
//!
//! ## What these tests pin
//!
//! The replacement is STRICTER than the thing it replaces, not a relaxation:
//! an unattributable, unexplained, unrecorded shared file becomes an
//! attributed, reason-carrying, recorded, session-limited one. Every test
//! below asserts a *narrowing*; none of them opens a way past a gate that did
//! not already exist.
//!
//! Written by a DISINTERESTED author (CLAUDE.md §2(a)): the implementer of the
//! fix did not write these. They therefore assert only what the target
//! behaviour states, and deliberately do NOT assert the on-disk layout of the
//! marker or the spelling of the error type — an implementation is free to
//! choose those.
//!
//! ## Isolation
//!
//! Every test operates on its own scratch directory under `std::env::temp_dir()`
//! and passes that directory in explicitly as the state dir. Nothing here reads
//! or writes `$HOME`, `~/.donegate`, `~/.overwatch` or any live session state,
//! and no process-global environment variable is mutated.

use std::path::{Path, PathBuf};

use harness_core::gate::run::{consume_session_skip, issue_session_skip};

static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// A fresh, empty state directory owned by one test.
fn state_dir(tag: &str) -> PathBuf {
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "harness-core-skip-{tag}-{}-{n}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create state dir");
    dir
}

/// Everything under `dir`, as display strings. Used to prove a refused issue
/// wrote nothing, and that a rejected session id could not escape the state dir.
fn tree(dir: &Path) -> Vec<String> {
    fn walk(dir: &Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            out.push(p.display().to_string());
            if p.is_dir() {
                walk(&p, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, &mut out);
    out.sort();
    out
}

/// The gate event log the primitive writes into, as raw text ("" when absent).
fn log(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("log.jsonl")).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// THE HEADLINE: cross-session non-consumption.
// ---------------------------------------------------------------------------

/// A skip issued by session A must be INVISIBLE to session B, and must still
/// be there for A afterwards.
///
/// This is the whole defect in one assertion. The shared project-root marker
/// could satisfy neither half: B consumed it (so B's legitimate gate was waved
/// through) and by consuming it destroyed A's (so A never got the exception it
/// asked for). One shared file, two wrong outcomes.
#[test]
fn a_skip_issued_by_one_session_is_invisible_to_another() {
    let dir = state_dir("cross-session");
    issue_session_skip(&dir, "session-A", "landing a doc-only fix")
        .expect("issuing a well-formed skip must succeed");

    let stolen = consume_session_skip(&dir, "session-B");
    assert!(
        stolen.is_none(),
        "session B consumed a skip that session A issued: B's own gate is now waved through on an \
         exception B never asked for. This is the shared-project-root hatch CLAUDE.md §5 forbids. \
         got: {stolen:?}"
    );

    let mine = consume_session_skip(&dir, "session-A");
    assert_eq!(
        mine.as_deref(),
        Some("landing a doc-only fix"),
        "session A's skip must survive another session's stop — a hatch another session can \
         destroy is not an escape hatch, it is a race"
    );
}

// ---------------------------------------------------------------------------
// Same-session behaviour.
// ---------------------------------------------------------------------------

/// The hatch still works for the session that asked for it, and returns the
/// reason it carried (the reason is what makes the bypass reviewable).
#[test]
fn my_own_skip_is_consumed_and_returns_its_reason() {
    let dir = state_dir("same-session");
    issue_session_skip(&dir, "sess-1", "flaky check, tracked in backlog 1234").unwrap();

    assert_eq!(
        consume_session_skip(&dir, "sess-1").as_deref(),
        Some("flaky check, tracked in backlog 1234"),
        "the issuing session must be able to consume its own skip, and get the reason back"
    );
}

/// ONE-SHOT: consuming removes it, so the same session's NEXT stop is gated
/// again. A hatch that persists is not an exception, it is a disabled gate.
#[test]
fn a_skip_is_one_shot_for_its_own_session_too() {
    let dir = state_dir("one-shot");
    issue_session_skip(&dir, "sess-1", "one stop only").unwrap();

    assert!(
        consume_session_skip(&dir, "sess-1").is_some(),
        "apparatus: the first consume must find the skip, else the emptiness below is vacuous"
    );
    let second = consume_session_skip(&dir, "sess-1");
    assert!(
        second.is_none(),
        "the skip survived its own consumption: the same session's next stop would be waved \
         through too, turning a one-stop exception into an open-ended gate bypass. got: {second:?}"
    );
}

// ---------------------------------------------------------------------------
// ANTI-VACUITY CONTROL: with no skip issued, there is nothing to consume.
// ---------------------------------------------------------------------------

/// Without this, every `is_none()` assertion above would also hold against an
/// implementation whose consume always returns `None` — i.e. against a hatch
/// that does not work at all.
#[test]
fn consuming_when_nothing_was_issued_finds_nothing() {
    let dir = state_dir("no-skip");
    assert!(
        consume_session_skip(&dir, "sess-1").is_none(),
        "a state dir with no skip in it must yield no skip"
    );
    assert!(
        log(&dir).is_empty(),
        "and must not record a consumption that did not happen: log={:?}",
        log(&dir)
    );
}

// ---------------------------------------------------------------------------
// Reason required.
// ---------------------------------------------------------------------------

/// An unexplained bypass is invisible to review even when it is recorded, so
/// the reason is not optional. Refusal must leave NOTHING consumable — a
/// refusal that still wrote the marker would be a refusal in name only.
#[test]
fn a_skip_with_no_reason_is_refused_and_records_nothing() {
    for (tag, reason) in [("empty", ""), ("blank", "   "), ("newline", "\n\t ")] {
        let dir = state_dir(&format!("no-reason-{tag}"));
        let r = issue_session_skip(&dir, "sess-1", reason);
        assert!(
            r.is_err(),
            "a skip with reason {reason:?} must be REFUSED: the hatch is reason-required, and an \
             unexplained bypass is exactly the unattributable exception this design removes"
        );
        assert!(
            consume_session_skip(&dir, "sess-1").is_none(),
            "a refused skip must leave nothing consumable — otherwise the refusal is cosmetic and \
             the next stop is still waved through. tree={:?}",
            tree(&dir)
        );
    }
}

// ---------------------------------------------------------------------------
// Attribution required.
// ---------------------------------------------------------------------------

/// No session id ⇒ no skip. An unattributable skip is precisely the shared
/// marker: it belongs to nobody, so it applies to everybody. CLAUDE.md §3 —
/// "cannot determine whose this is" resolves to the restrictive side (no skip),
/// never to a placeholder bucket that every session would collide in.
#[test]
fn a_skip_with_no_session_id_is_refused() {
    let dir = state_dir("no-session");
    assert!(
        issue_session_skip(&dir, "", "no session to attribute this to").is_err(),
        "an unattributable skip must be refused, not filed under a shared placeholder"
    );
    assert!(
        consume_session_skip(&dir, "").is_none(),
        "and the empty session id must consume nothing"
    );
    assert!(
        tree(&dir).is_empty(),
        "a refused issue must not create any marker at all: tree={:?}",
        tree(&dir)
    );
}

/// `"_local"` is the shared marker wearing a new name, and must be unusable.
///
/// `HookInput::session_key()` substitutes `"_local"` whenever the payload
/// carries no session id — a hook payload that fails to parse, or one missing
/// the field. That is the SAME key for every such run, so a skip stored under
/// it would be consumable by any session whose payload happened to degrade:
/// one file, no attribution, whoever stops next takes it. Exactly the
/// project-root marker this API deletes, rebuilt inside the state dir.
///
/// Both directions are pinned. Issuing must be refused, and — because a
/// `_local` marker can arrive by other means than this API (a hand-written
/// file, a migration, a future caller) — consuming must refuse an existing one
/// rather than trusting that issuing is the only way it could have appeared.
#[test]
fn the_local_fallback_bucket_can_neither_be_issued_nor_consumed() {
    let dir = state_dir("local-bucket");

    assert!(
        issue_session_skip(&dir, "_local", "no session id available").is_err(),
        "`_local` is what every unattributed run collapses onto, so a skip keyed on it is shared \
         by all of them — it must be refused, not filed"
    );

    // A `_local` marker planted directly, as a caller other than this API could
    // leave it. Written through the same layout a real skip uses so the test
    // fails if the file is simply unreadable rather than deliberately ignored.
    let planted = dir.join("skips").join("_local.skip");
    std::fs::create_dir_all(planted.parent().expect("skips dir")).unwrap();
    std::fs::write(&planted, "planted in the shared bucket\n").unwrap();

    let consumed = consume_session_skip(&dir, "_local");
    assert!(
        consumed.is_none(),
        "a skip in the shared `_local` bucket was consumed: every session whose payload lacks an \
         id lands on that key, so this is the cross-session leak restored under a new name. \
         got: {consumed:?}"
    );

    // APPARATUS: the same state dir, same layout, a real session id — this must
    // work, or the refusal above is indistinguishable from a store that simply
    // cannot read anything.
    issue_session_skip(&dir, "sess-real", "a real attributed skip").unwrap();
    assert_eq!(
        consume_session_skip(&dir, "sess-real").as_deref(),
        Some("a real attributed skip"),
        "apparatus: an attributed skip in this very state dir must still work"
    );
}

/// A session id is a key, not a path. One that is not a single safe path
/// component must be refused outright, and must never cause a write outside
/// the state dir. (Session ids are read from a hook payload / an env var, so
/// this is untrusted input.)
#[test]
fn a_session_id_that_is_not_a_path_component_cannot_escape_the_state_dir() {
    let parent = state_dir("traversal");
    let dir = parent.join("state");
    std::fs::create_dir_all(&dir).unwrap();

    for bad in ["../escapee", "a/b", "..", ".", "with\\backslash"] {
        let r = issue_session_skip(&dir, bad, "trying to escape");
        assert!(
            r.is_err(),
            "session id {bad:?} is not a single path component and must be refused; accepting it \
             either writes the marker outside the state dir or lands two different sessions on \
             one file"
        );
    }

    let all = tree(&parent);
    assert!(
        all.contains(&dir.display().to_string()),
        "apparatus: the walk must actually see the state dir, else the emptiness below is vacuous; \
         saw {all:?}"
    );
    let outside: Vec<String> = all
        .into_iter()
        .filter(|p| !Path::new(p).starts_with(&dir))
        .collect();
    assert!(
        outside.is_empty(),
        "nothing may be written outside the state dir; stray entries: {outside:?}"
    );
}

// ---------------------------------------------------------------------------
// The bypass is RECORDED.
// ---------------------------------------------------------------------------

/// A bypass nobody can see afterwards is indistinguishable from a gate that
/// passed. The CONSUMPTION specifically (not merely the issuing) must be
/// recorded: the issue is an intention, the consumption is the gate actually
/// being waved through.
///
/// Asserted as "the log GREW across the consume, and the new text names the
/// session and the reason" rather than by matching an event name, so the
/// implementation is free to choose its own schema.
#[test]
fn consuming_a_skip_is_recorded_in_the_gate_log() {
    let dir = state_dir("recorded");
    issue_session_skip(&dir, "sess-rec", "temporarily red while I bisect").unwrap();
    let before = log(&dir);

    assert!(
        consume_session_skip(&dir, "sess-rec").is_some(),
        "apparatus: the skip must actually have been consumed"
    );

    let after = log(&dir);
    assert!(
        after.len() > before.len(),
        "consuming a skip wrote nothing new to the gate log: an untracked bypass is invisible to \
         review, i.e. indistinguishable from the gate having passed. before={before:?} \
         after={after:?}"
    );
    let added = &after[before.len()..];
    assert!(
        added.contains("sess-rec"),
        "the consumption record must name the session that was let through; added={added:?}"
    );
    assert!(
        added.contains("temporarily red while I bisect"),
        "the consumption record must carry the reason — that is the entire point of requiring \
         one; added={added:?}"
    );
}

// ---------------------------------------------------------------------------
// State dirs do not bleed into each other.
// ---------------------------------------------------------------------------

/// Each gate owns its own state dir, so a skip issued for donegate must not
/// also wave reviewgate's stop through. (The old marker was per-gate by
/// filename; the replacement must not lose that.)
#[test]
fn a_skip_in_one_gates_state_dir_is_not_visible_in_anothers() {
    let a = state_dir("gate-a");
    let b = state_dir("gate-b");
    issue_session_skip(&a, "sess-1", "only this gate").unwrap();

    assert!(
        consume_session_skip(&b, "sess-1").is_none(),
        "a skip issued for one gate must not apply to a different gate"
    );
    assert!(
        consume_session_skip(&a, "sess-1").is_some(),
        "apparatus: and it must still be there for the gate it was issued for"
    );
}
