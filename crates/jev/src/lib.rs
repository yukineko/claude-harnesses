// テスト内の unwrap/expect/panic は意図的な assert であって fail-open ではないので許可する。
// production 側は workspace の [workspace.lints.clippy] で deny のまま。
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
//! jev — an **advisory-only** wrapper around jev (TypeSafe AI's "System One"
//! model), which returns typed, calibrated decisions instead of text.
//!
//! # The one thing to understand about this crate
//!
//! jev is a **closed, third-party, metered cloud API**. CLAUDE.md §7 says the
//! authority to stop or pass the flow is never handed to an external service:
//! an outside service may contribute an opinion (advisory), never a verdict.
//! This crate enforces that **with types**, not with prose:
//!
//! 1. **[`Advice`] has no "clean" variant.** It is either
//!    [`Advice::Escalate`] — jev answered, and the answer is a reason to treat
//!    something *more* restrictively — or [`Advice::NoSignal`]. There is
//!    deliberately no `Clean` / `Ok` / `Pass`. Adding one would be the §7
//!    violation, not a convenience.
//!
//! 2. **"jev said it's fine" and "jev was never called" are the same value.**
//!    Not configured, HTTP error, timeout, unparseable body, below threshold,
//!    and a permissive answer all collapse into `NoSignal`. A consumer
//!    therefore *cannot* write code that behaves differently when jev is
//!    absent, which is what makes a missing `TYPESAFE_API_KEY` a **structural**
//!    no-op rather than a promised one. It also kills the mirror-image
//!    fail-open that CLAUDE.md §3 is about: an advisor that cannot clear
//!    anything cannot fail open.
//!
//! 3. **No `Verdict` anywhere in this crate.** `harness_core::verdict::Verdict`
//!    is the repo's gate answer. It is absent from this crate's API in both
//!    directions, and no function here converts an `Advice` into one. No
//!    coupling means no structural ability to flip a gate.
//!    `tests/no_verdict_coupling.rs` scans the sources and fails if the token
//!    reappears.
//!
//!    The transport's three-valued result *is*
//!    [`harness_core::verdict::Determination`], and that is deliberate rather
//!    than an exception: `Determination<T>` is the repo's shared "known /
//!    could-not-determine" container (CLAUDE.md §3 asks crates to converge on
//!    it instead of reinventing a three-valued type), it carries no verdict,
//!    and every `Verdict` reachable from it — via `Required::Blocked`'s
//!    unforgeable `Undet` — is `Verdict::Undetermined`, which **blocks**. There
//!    is no path from anything this crate returns to a `Verdict::Clean`:
//!    `Clean` requires harness-core's private `Evidence` witness plus findings
//!    the caller actually collected.
//!
//! 4. **Nothing is sent without a key.** [`client::Client::ask`] resolves
//!    [`Availability`] *before* it touches the transport and returns without
//!    calling it unless the key is [`Availability::Configured`]. That is
//!    checked by counting transport invocations, not by reading the code:
//!    `tests/no_key_no_network.rs`.
//!
//! # What jev is good at, and what it is not
//!
//! Suited (per TypeSafe's own use-case map): classification, detection,
//! scoring on a rubric, routing, ranking/retrieval, verification against named
//! failure modes, structured extraction.
//!
//! Measured gotchas (2026-10-02, docs.typesafe.ai) that this crate does **not**
//! paper over, because callers must know them:
//!
//! - Counting degrades as the count grows; loop in code instead.
//! - Date arithmetic is unreliable; extract fields and compute in code.
//! - The same question asked as a Noul and as a Choice disagrees — negations do
//!   not sum to 1.0. Do not mix framings and compare.
//! - **Adversarial text that argues for its own classification moves the
//!   answer.** This is why jev is advisory here: its input is often
//!   attacker-influenced.
//! - It answers your literal words, not your intent.
//! - `noul = 0.5` means "equally likely", not "medium". Use a Score for
//!   spectrums.
//! - `jev-latest` drifts across releases; pin a version before tuning a
//!   threshold. See [`config::Config::model`].
//!
//! # Not subscription-native
//!
//! Most of this repo's harnesses run inside the Claude Code subscription with
//! no API key. jev does not: it is billed per request (input $0.042/M tokens,
//! output free, measured 2026-10-02). It is therefore **off unless a key is
//! exported**, and every call is appended to a local usage ledger so the spend
//! is observable. See [`ledger`].

pub mod advice;
pub mod availability;
pub mod client;
pub mod config;
pub mod ledger;
pub mod redact;
pub mod transport;
pub mod wire;

pub use advice::{Advice, NoSignalReason};
pub use availability::{Availability, Credential, NotConfiguredReason, KEY_ENV};
pub use client::Client;
pub use config::Config;
pub use transport::{CurlTransport, RawHttp, RecordingTransport, Transport};
pub use wire::{Answer, JevRequest, Question, RawResponse, Usage};

/// The jev endpoint. Measured 2026-10-02 from docs.typesafe.ai.
pub const ENDPOINT: &str = "https://api.typesafe.ai/v1/systemone";
