// テスト内の unwrap/expect/panic は意図的な assert であって fail-open ではないので許可する。
// production 側は workspace の [workspace.lints.clippy] で deny のまま。
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
//! taintguard as a library: the pure taint-state store, path classifier, and
//! interactivity check reused by `src/main.rs` (the hooks) and by the
//! integration tests.
//!
//! # What this crate does
//!
//! Provenance-scoped least privilege: once a turn consumes untrusted-
//! provenance content (a `WebFetch`/`WebSearch` result, or a `Read` outside
//! the project root), write-class tools (`Bash`/`Write`/`Edit`/`MultiEdit`/
//! `NotebookEdit`) are downgraded to `ask` (interactive) or `deny` (headless)
//! for the rest of that turn. A clean `Stop` (the turn ends without further
//! taint) restores the session to normal.
//!
//! Three hooks, three subcommands:
//!   * `mark`  (PostToolUse, matcher `WebFetch|WebSearch|Read`) — records the
//!     taint.
//!   * `gate`  (PreToolUse, matcher `Bash|Write|Edit|MultiEdit|NotebookEdit`) —
//!     consumes it.
//!   * `clear` (Stop) — resets it.
//!
//! # Operating postures
//!
//! `gate` has two postures, resolved from `TAINTGUARD_OBSERVE_ONLY` (see
//! [`observe`]): the default **enforce** posture described above, and an opt-in
//! **observe-only** measurement posture that runs the same check, reports the
//! same finding, but emits no `permissionDecision` and instead records the
//! suppressed enforcement so its fire-rate can be counted. Observe-only never
//! turns a `Tainted`/`Undetermined` check into a `Clean` one — the two live on
//! separate axes ([`state::Check`] vs [`observe::Posture`]) exactly so that
//! "suppressed" stays distinguishable from "nothing found".

pub mod classify;
pub mod hookio;
pub mod interactive;
pub mod observe;
pub mod state;
