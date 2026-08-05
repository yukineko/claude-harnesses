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
//! **One carve-out, added in 0.2.0 (backlog a4b59893):** a `Bash` invocation
//! that [`readonly::is_readonly_bash`] positively recognises as write-free does
//! not consult the taint state at all. The sentence above was the promise the
//! crate always made ("*write-class* tools"), while the hook matched the whole
//! `Bash` tool — so a tainted turn could not run `git status` to diagnose
//! itself, and a non-interactive worker had no route back except a human
//! re-invocation. Allowing a command that cannot write is not a hole in the
//! invariant: it is the invariant, finally applied to what it says. Everything
//! `is_readonly_bash` does not positively recognise is gated exactly as before.
//!
//! The taint marker is keyed by **session id alone** since 0.2.0; see
//! [`state::state_dir`] for the fail-open that removing the `cwd` dimension
//! closes (backlog 90d1ca1d).
//!
//! Three hooks, plus a fourth subcommand that is NOT a hook:
//!   * `mark`  (PostToolUse, matcher `WebFetch|WebSearch|Read`) — records the
//!     taint.
//!   * `gate`  (PreToolUse, matcher `Bash|Write|Edit|MultiEdit|NotebookEdit`) —
//!     consumes it.
//!   * `clear` (Stop) — resets it.
//!   * `tally` — operator readout: prints the observe-only ledger totals for the
//!     project in the process cwd. Reads no stdin, is deliberately NOT wrapped
//!     in `harness_core::hook::run_hook` (whose terminal `exit(0)` would make
//!     "could not read the tally" and "the tally is zero" share an exit
//!     status), and exits non-zero when the tally could not be read.
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
//!
//! Observe-only suppresses **`Tainted` only** (changed in 0.1.6). A
//! [`state::Check::Undetermined`] — the taint state could not be determined —
//! resolves to `ask`/`deny` in either posture, as does a panic in the gate's
//! barrier: cannot-determine always resolves to the restricted side (CLAUDE.md
//! §3), and suppressing a finding that names no sources would have measured
//! nothing anyway. Such an enforced `Undetermined` therefore writes **no** ledger
//! line, so the ledger counts *suppressed enforcements* rather than *gate
//! firings*; see [`observe`]'s module docs for the full argument.

pub mod classify;
pub mod hookio;
pub mod interactive;
pub mod observe;
pub mod readonly;
pub mod state;
