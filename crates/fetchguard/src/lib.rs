// テスト内の unwrap/expect/panic は意図的な assert であって fail-open ではないので許可する。
// production 側は workspace の [workspace.lints.clippy] で deny のまま。
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
//! fetchguard — a runtime, content-level prompt-injection scanner for a
//! Claude Code `PostToolUse` hook (matcher `WebFetch|WebSearch`).
//!
//! # Why this crate exists
//!
//! `scripts/check-prompt-injection.py` (injectguard) scans COMMITTED prompt
//! assets (skills, agent definitions, `CLAUDE.md`, docs, …) at commit time.
//! Its own docstring names the gap this crate closes: "the runtime reflux
//! defense (condukt 0.7.6) covers untrusted *execution output*, not text
//! planted in the repo itself" — and, symmetrically, that runtime reflux
//! defense covers execution OUTPUT (a Bash/tool result echoed back), not
//! text a `WebFetch`/`WebSearch` call pulls in from an external, adversarial
//! source at runtime. Nothing screens THAT content before it lands in the
//! model's context. This crate is that missing screen.
//!
//! `taintguard` (the sibling crate) already exists for the ORTHOGONAL
//! provenance question — WHERE did this content come from (marks a session
//! tainted after any `WebFetch`/`WebSearch`/external `Read`, then downgrades
//! write-class tools for the rest of the turn). fetchguard adds the CONTENT
//! signal — WHAT does the text SAY — by scanning the actual response text
//! for the same concealment / verification-bypass / instruction-override /
//! egress phrasings `check-prompt-injection.py` already recognises, and
//! surfacing a hit (or an undecidable response) as `additionalContext` on
//! the same `PostToolUse` turn so the model is told, in the moment, that the
//! fetched span is untrusted DATA whose embedded directives must not be
//! followed. The two crates are meant to run together, not as substitutes
//! for each other.
//!
//! External-file `Read` is DEFERRED here (see `gate::WEB_TOOLS`'s doc
//! comment): classifying whether a path is "external" is a path-trust
//! judgment `taintguard::classify` already owns, and duplicating it here
//! would be a second, driftable copy of the same decision.
//!
//! # Modules
//!
//! * [`scan`] — the pure four-category pattern scanner + defense-context
//!   suppression (mirrors `check-prompt-injection.py`'s `MALICIOUS` list;
//!   see its module docs for the single-source-of-truth decision).
//! * [`gate`] — the `PostToolUse` decision built on top of `scan`, with the
//!   fail-closed contract for an undecidable `tool_response` and the panic
//!   barrier `main`'s `scan` subcommand calls.
//! * [`hookio`] — the `hookSpecificOutput` JSON serializer.

pub mod gate;
pub mod hookio;
pub mod scan;
