//! A one-shot skip token must not be burned by a stop that never happened.
//!
//! # The defect being pinned
//!
//! `harness_core::gate::run::consume_session_skip` used to delete the marker
//! file the instant *its own* gate decided to allow:
//!
//! ```ignore
//! let reason = std::fs::read_to_string(&path).ok()?.trim().to_string();
//! let _ = std::fs::remove_file(&path);   // burned, unconditionally
//! ```
//!
//! But a Stop is adjudicated by **four independent processes** — donegate
//! (`crates/donegate/src/main.rs`, two call sites), reviewgate, propguard and
//! tdd. Each consumes the session's token knowing only its own verdict. If
//! donegate consumes it and allows while reviewgate blocks the same stop,
//! **the stop did not happen** and the operator's one-shot escape was spent on
//! nothing: they must re-issue it on every re-entry, for every gate, until the
//! whole four-way conjunction goes green at once. That is precisely the
//! standing pressure toward a *permanent* bypass that CLAUDE.md 第5節 forbids
//! ("skip 機構は理由を書いて一度だけ").
//!
//! Scoping the marker to the issuing session (`<state_dir>/skips/<id>.skip`,
//! landed separately) fixed *whose* token gets consumed. It did not touch
//! *when* — the four-way race above is entirely within one session.
//!
//! # The seam (observed, not assumed)
//!
//! Whether the *previous* stop attempt was blocked is already known to every
//! gate: Claude Code sets `stop_hook_active` on the stop that follows a block
//! ([`harness_core::hook::HookInput`]), and all four gates already have that
//! field in scope at the call site — each body takes the parsed `HookInput`,
//! and `run_guarded` is already fed the same bit. So the contract below is
//! expressible without inventing any new plumbing.
//!
//! # Not pinned here (stated rather than silently dropped)
//!
//! A token the process cannot even *see* — `Path::exists()` reports `false`
//! for `EACCES` exactly as it does for `ENOENT`, so an unreadable-by-permission
//! token reads as "no token" — is left to a follow-up. Pinning it needs
//! permission manipulation (`chmod 000`), which is a silent no-op for uid 0;
//! such a test would pass vacuously wherever the suite runs as root, i.e. it
//! would be the "何も検証していないテスト" that CLAUDE.md 第2節 warns about. It
//! needs a real seam (an injectable metadata probe) or an explicit human
//! decision, not a conditionally-skipped assert. The *readable-but-not-parsable*
//! shape IS pinned below, via a case that needs no permission games at all.

use std::path::{Path, PathBuf};

use harness_core::gate::run::consume_session_skip;

/// The single seam every test below goes through, so the whole contract has one
/// call site to repoint if the API moves again.
fn consume_skip_for_stop(state_dir: &Path, stop_hook_active: bool) -> Option<String> {
    consume_session_skip(state_dir, SESSION, stop_hook_active)
}

/// A fresh, isolated gate state dir.
fn state_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let d = std::env::temp_dir().join(format!(
        "hc-skip-token-{}-{tag}-{nanos}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

const SESSION: &str = "sess-token-lifecycle";

/// Where the issuing side puts the token. Written directly rather than through
/// `issue_session_skip` so these tests pin the CONSUMER's contract alone: an
/// issuer bug must not be able to make them pass.
fn place_token(state_dir: &Path, reason: &str) -> PathBuf {
    let skips = state_dir.join("skips");
    std::fs::create_dir_all(&skips).unwrap();
    let p = skips.join(format!("{SESSION}.skip"));
    std::fs::write(&p, reason).unwrap();
    p
}

/// THE CONTRACT.
///
/// Stop attempt #1 (`stop_hook_active == false`): donegate honours the token
/// and allows. Some *other* gate — reviewgate/tdd/propguard, a different
/// process this one cannot see — blocks the same stop, so the turn does not
/// end. Claude Code re-enters with `stop_hook_active == true`. On that
/// re-entry the token must STILL be owed: the stop it was placed for has not
/// happened yet.
#[test]
fn token_survives_a_stop_that_another_gate_blocked() {
    let dir = state_dir("survives-block");
    place_token(&dir, "shipping a hotfix\n");

    // Attempt #1 — first stop of the chain. donegate honours and allows.
    assert_eq!(
        consume_skip_for_stop(&dir, false).as_deref(),
        Some("shipping a hotfix"),
        "the token must be honoured on the stop it was placed for"
    );

    // ...another gate blocked that same stop, so Claude Code re-enters with
    // stop_hook_active = true. The stop the operator paid for never happened.
    assert_eq!(
        consume_skip_for_stop(&dir, true).as_deref(),
        Some("shipping a hotfix"),
        "a stop that some OTHER gate blocked must not burn this gate's one-shot \
         token: no stop happened, so the escape is still owed. Burning it here \
         forces the operator to re-issue the skip on every re-entry, which is \
         the pressure toward a permanent bypass (CLAUDE.md 第5節)."
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// ANTI-VACUITY CONTROL #1.
///
/// The ordinary path: nothing blocks, the stop completes, and the next stop
/// belongs to a new chain (`stop_hook_active == false` again). The token must
/// be gone by then — consumed EXACTLY ONCE.
///
/// Without this control, "never delete the token" would satisfy the contract
/// test above while converting a one-shot escape into a permanent bypass —
/// the exact failure mode CLAUDE.md 第5節 names.
#[test]
fn token_is_consumed_exactly_once_on_the_ordinary_path() {
    let dir = state_dir("exactly-once");
    place_token(&dir, "  because\n");

    assert_eq!(
        consume_skip_for_stop(&dir, false).as_deref(),
        Some("because"),
        "present on the first stop"
    );
    assert!(
        consume_skip_for_stop(&dir, false).is_none(),
        "a one-shot token must be gone on the next stop that is not a re-entry: \
         an escape that survives a completed stop is a permanent bypass"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// ANTI-VACUITY CONTROL #2 for the blocked path.
///
/// Surviving a blocked stop must be BOUNDED. Full lifecycle: honoured on
/// attempt #1, still owed across the blocked re-entry, and gone once a stop it
/// authorised finally completes (the next non-re-entry stop). This is what
/// stops the fix from degenerating into "keep the token forever".
#[test]
fn surviving_a_block_does_not_make_the_token_permanent() {
    let dir = state_dir("bounded");
    place_token(&dir, "one stop only\n");

    // #1 first stop: honoured. Another gate blocks it.
    assert_eq!(
        consume_skip_for_stop(&dir, false).as_deref(),
        Some("one stop only")
    );
    // #2 re-entry after that block: still owed.
    assert_eq!(
        consume_skip_for_stop(&dir, true).as_deref(),
        Some("one stop only"),
        "still owed while the stop keeps being blocked"
    );
    // ...this time every gate allowed, so the stop happened and the turn ended.
    // The next stop starts a fresh chain: the token has been spent.
    assert!(
        consume_skip_for_stop(&dir, false).is_none(),
        "once a stop the token authorised actually completed, the token is spent \
         — surviving a block must not make it permanent"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A re-issued skip supersedes an outstanding honour, and is itself bounded.
///
/// The honoured token is parked under a DIFFERENT name (`<id>.skip.honoured`)
/// precisely so this case is unambiguous: an operator who issues a fresh skip
/// while an old honour is still outstanding gets one stop's worth of escape,
/// not two. Without this the rename would be a way to bank tokens.
#[test]
fn a_reissued_token_supersedes_an_outstanding_honour() {
    let dir = state_dir("reissue");
    place_token(&dir, "first reason");

    assert_eq!(
        consume_skip_for_stop(&dir, false).as_deref(),
        Some("first reason"),
        "honoured on the stop it was issued for"
    );
    // Some other gate blocked; the honour is outstanding. The operator now
    // issues a NEW skip rather than waiting for the re-entry.
    place_token(&dir, "second reason");
    assert_eq!(
        consume_skip_for_stop(&dir, true).as_deref(),
        Some("second reason"),
        "a freshly issued skip wins over the parked honour: the newer reason is \
         the one the operator meant, and it must not read as the older one"
    );
    assert!(
        consume_skip_for_stop(&dir, false).is_none(),
        "and it is still one-shot: the re-issue does not bank a second escape"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A hand-made EMPTY token must not be honoured, and must not survive.
///
/// `issue_session_skip` refuses an empty reason, so a blank marker can only
/// have been placed by hand. The removed inline test `skip_marker_empty_gives_
/// default_reason` pinned the OPPOSITE for the old project-root marker: an empty
/// file was honoured as `"(no reason given)"`. That was a fail-open — an
/// unexplained bypass is invisible to review, which is the whole reason the
/// hatch is reason-required — and it is inverted here rather than dropped.
///
/// Removing the file matters as much as returning `None`: a blank marker that
/// is left in place is consulted again on every later stop, and any future
/// change that starts honouring it would make it a permanent hatch rather than
/// a one-shot.
#[test]
fn an_empty_token_is_not_honoured_and_is_cleared() {
    let dir = state_dir("empty-token");
    let marker = place_token(&dir, "   \n\t ");

    assert_eq!(
        consume_skip_for_stop(&dir, false),
        None,
        "a marker with no reason is not a valid escape: the hatch is \
         reason-required, and an unexplained bypass is exactly the \
         unattributable exception this design removes"
    );
    assert!(
        !marker.exists(),
        "and it must not be left lying there: a blank marker that survives is \
         consulted on every later stop, i.e. a standing hatch rather than a \
         one-shot. tree={:?}",
        std::fs::read_dir(dir.join("skips"))
            .map(|it| it
                .filter_map(|e| e.ok().map(|e| e.file_name()))
                .collect::<Vec<_>>())
            .unwrap_or_default()
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A token whose contents cannot be read must NOT be honoured.
///
/// Honouring 判定不能 as a valid escape would allow the stop on an IO failure;
/// worse, `remove_file` fails on the same shape, so it would be re-honoured on
/// *every* subsequent stop — a permanent bypass created by accident.
///
/// A directory at the marker path reproduces this deterministically and with no
/// permission games: `Path::exists()` is true for it, `read_to_string` fails
/// with `EISDIR`, and `remove_file` fails with `EISDIR` too.
///
/// The restrictive resolution (CLAUDE.md 第3節) is `None`: a token we cannot
/// read is not a token we may act on, so the gate proceeds with its check.
#[test]
fn an_unreadable_token_is_not_honoured() {
    let dir = state_dir("unreadable");
    let marker = dir.join("skips").join(format!("{SESSION}.skip"));
    std::fs::create_dir_all(&marker).unwrap();

    // Sanity: the fixture really does reproduce the shape (exists, unreadable).
    assert!(marker.exists(), "fixture: the marker path exists");
    assert!(
        std::fs::read_to_string(&marker).is_err(),
        "fixture: its contents cannot be read"
    );
    assert!(
        std::fs::remove_file(&marker).is_err(),
        "fixture: `remove_file` cannot clear it either — which is why honouring \
         it is not a one-shot but a standing bypass (observed, not assumed)"
    );

    assert_eq!(
        consume_skip_for_stop(&dir, false),
        None,
        "a marker whose contents cannot be read is 判定不能, not a valid escape: \
         honouring it allows the stop on an IO failure, and since remove_file \
         fails too it is re-honoured on every later stop (a permanent bypass). \
         Resolve to the restrictive side: do not honour it."
    );

    let _ = std::fs::remove_dir_all(&dir);
}
