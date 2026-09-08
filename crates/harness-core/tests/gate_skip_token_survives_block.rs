//! RED guard: a one-shot skip token must not be burned by a stop that never
//! happened.
//!
//! # The defect being pinned
//!
//! `harness_core::gate::run::consume_skip` deletes the marker file the instant
//! *its own* gate decides to allow:
//!
//! ```ignore
//! let reason = ...;                    // read
//! let _ = std::fs::remove_file(&p);    // burned, unconditionally
//! Some(reason)
//! ```
//!
//! But a Stop is adjudicated by **four independent processes** — donegate
//! (`.donegate-skip`, `crates/donegate/src/main.rs:198,439`), reviewgate
//! (`.reviewgate-skip`, `crates/reviewgate/src/main.rs:157`), propguard
//! (`.propguard-skip`, `crates/propguard/src/main.rs:200`) and tdd
//! (`.tdd-skip`, `crates/tdd/src/main.rs:206`). Each burns its own token
//! knowing only its own verdict. If donegate burns `.donegate-skip` and allows
//! while reviewgate blocks the same stop, **the stop did not happen** and the
//! operator's one-shot escape was spent on nothing: they must place it again on
//! every re-entry, for every gate, until the whole four-way conjunction goes
//! green at once. That is precisely the standing pressure toward a *permanent*
//! bypass that CLAUDE.md 第5節 forbids ("skip 機構は理由を書いて一度だけ").
//!
//! # The seam (observed, not assumed)
//!
//! Whether the *previous* stop attempt was blocked is already known to every
//! gate: Claude Code sets `stop_hook_active` on the stop that follows a block
//! (`crates/harness-core/src/hook.rs:77`), and all four gates already have that
//! field in scope at the `consume_skip` call site — each body takes the parsed
//! `HookInput` (`donegate:152 let input = hook.unwrap_or_default();`, likewise
//! `reviewgate:134`, `tdd:186`, `propguard:174`) and `run_guarded` is already
//! fed the same bit (`donegate:143`). So the contract below is expressible
//! without inventing any new plumbing — only `consume_skip` itself is blind to
//! it today.
//!
//! # Target API (DOES NOT EXIST YET — see [`consume_skip_for_stop`])
//!
//! ```ignore
//! pub fn consume_skip(root: &Path, marker: &str, stop_hook_active: bool) -> Option<String>
//! ```
//!
//! with: honour the token while it is owed; burn it only once a stop it
//! authorised actually completed. The tests below go through one shim so the
//! implementer has a single call site to repoint.
//!
//! # Not pinned here (stated rather than silently dropped)
//!
//! `consume_skip`'s third fail-open shape — `if !p.exists() { return None; }`,
//! where `Path::exists()` reports `false` for EACCES exactly as it does for
//! ENOENT, so an *unreadable* token reads as "no token" — is **left to a
//! follow-up**. Pinning it needs permission manipulation (`chmod 000`), which
//! is a silent no-op for uid 0; such a test would pass vacuously wherever the
//! suite runs as root, i.e. it would be the "何も検証していないテスト" that
//! CLAUDE.md 第2節 warns about. It needs a real seam (an injectable metadata
//! probe) or an explicit human decision, not a conditionally-skipped assert.
//! The *second* shape (an IO failure becoming the honoured reason
//! `"(no reason given)"`) IS pinned below, via a case that needs no permission
//! games at all.

use std::path::{Path, PathBuf};

use harness_core::gate::run::consume_skip;

/// The single seam every test below goes through.
///
/// **IMPLEMENTER: this shim is the target of the fix.** Today it drops
/// `stop_hook_active` on the floor and calls the current two-argument
/// `consume_skip`, which is why the contract tests fail at *runtime* rather
/// than failing to compile (a test author must not hand the implementer an
/// unbuildable tree). Repoint this body at the real API once it exists:
///
/// ```ignore
/// fn consume_skip_for_stop(root: &Path, marker: &str, stop_hook_active: bool) -> Option<String> {
///     consume_skip(root, marker, stop_hook_active)
/// }
/// ```
///
/// If the fix instead lands as a *new* function beside the old one, this shim
/// keeps testing the old one and the contract tests stay red — loudly, never
/// vacuously.
fn consume_skip_for_stop(root: &Path, marker: &str, stop_hook_active: bool) -> Option<String> {
    consume_skip(root, marker, stop_hook_active)
}

/// A fresh, isolated project root.
fn root(tag: &str) -> PathBuf {
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

const MARKER: &str = ".donegate-skip";

/// THE CONTRACT (expected RED until the fix lands).
///
/// Stop attempt #1 (`stop_hook_active == false`): donegate honours the token
/// and allows. Some *other* gate — reviewgate/tdd/propguard, a different
/// process this one cannot see — blocks the same stop, so the turn does not
/// end. Claude Code re-enters with `stop_hook_active == true`. On that
/// re-entry the token must STILL be owed: the stop it was placed for has not
/// happened yet.
#[test]
fn token_survives_a_stop_that_another_gate_blocked() {
    let root = root("survives-block");
    std::fs::write(root.join(MARKER), "shipping a hotfix\n").unwrap();

    // Attempt #1 — first stop of the chain. donegate honours and allows.
    assert_eq!(
        consume_skip_for_stop(&root, MARKER, false).as_deref(),
        Some("shipping a hotfix"),
        "the token must be honoured on the stop it was placed for"
    );

    // ...another gate blocked that same stop, so Claude Code re-enters with
    // stop_hook_active = true. The stop the operator paid for never happened.
    assert_eq!(
        consume_skip_for_stop(&root, MARKER, true).as_deref(),
        Some("shipping a hotfix"),
        "a stop that some OTHER gate blocked must not burn this gate's one-shot \
         token: no stop happened, so the escape is still owed. Burning it here \
         forces the operator to re-place the marker on every re-entry, which is \
         the pressure toward a permanent bypass (CLAUDE.md 第5節)."
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// ANTI-VACUITY CONTROL #1 (PASSES NOW, MUST KEEP PASSING).
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
    let root = root("exactly-once");
    std::fs::write(root.join(MARKER), "  because\n").unwrap();

    assert_eq!(
        consume_skip_for_stop(&root, MARKER, false).as_deref(),
        Some("because"),
        "present on the first stop"
    );
    assert!(
        consume_skip_for_stop(&root, MARKER, false).is_none(),
        "a one-shot token must be gone on the next stop that is not a re-entry: \
         an escape that survives a completed stop is a permanent bypass"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// ANTI-VACUITY CONTROL #2 for the blocked path (expected RED until the fix).
///
/// Surviving a blocked stop must be BOUNDED. Full lifecycle: honoured on
/// attempt #1, still owed across the blocked re-entry, and gone once a stop it
/// authorised finally completes (the next non-re-entry stop). This is what
/// stops the fix from degenerating into "keep the token forever".
#[test]
fn surviving_a_block_does_not_make_the_token_permanent() {
    let root = root("bounded");
    std::fs::write(root.join(MARKER), "one stop only\n").unwrap();

    // #1 first stop: honoured. Another gate blocks it.
    assert_eq!(
        consume_skip_for_stop(&root, MARKER, false).as_deref(),
        Some("one stop only")
    );
    // #2 re-entry after that block: still owed.
    assert_eq!(
        consume_skip_for_stop(&root, MARKER, true).as_deref(),
        Some("one stop only"),
        "still owed while the stop keeps being blocked"
    );
    // ...this time every gate allowed, so the stop happened and the turn ended.
    // The next stop starts a fresh chain: the token has been spent.
    assert!(
        consume_skip_for_stop(&root, MARKER, false).is_none(),
        "once a stop the token authorised actually completed, the token is spent \
         — surviving a block must not make it permanent"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// A token whose contents cannot be read must NOT be honoured (expected RED).
///
/// `consume_skip` currently maps the read failure to
/// `.ok() ... unwrap_or_else(|| "(no reason given)")`, i.e. 判定不能 → the
/// permissive answer: an unreadable marker is honoured as a valid escape and
/// the stop is allowed. Worse, `remove_file` also fails on it, so the
/// unreadable marker is honoured again on *every* subsequent stop — a
/// permanent bypass created by accident.
///
/// A directory named `.donegate-skip` reproduces this deterministically and
/// with no permission games: `Path::exists()` is true for it, `read_to_string`
/// fails with `EISDIR`, and `remove_file` fails with `EISDIR` too.
///
/// The restrictive resolution (CLAUDE.md 第3節) is `None`: a token we cannot
/// read is not a token we may act on, so the gate proceeds with its check.
#[test]
fn an_unreadable_token_is_not_honoured() {
    let root = root("unreadable");
    let marker = root.join(MARKER);
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
        consume_skip_for_stop(&root, MARKER, false),
        None,
        "a marker whose contents cannot be read is 判定不能, not a valid escape: \
         honouring it as `(no reason given)` allows the stop on an IO failure, \
         and since remove_file fails too it is re-honoured on every later stop \
         (a permanent bypass). Resolve to the restrictive side: do not honour it."
    );

    let _ = std::fs::remove_dir_all(&root);
}
