# jev — advisory-only wrapper for TypeSafe AI's System One model

`jev` returns **typed, calibrated decisions instead of text**: a probability, a
chosen option with its distribution, or a position on a rubric. This crate
wraps it for the harness family under one rule.

> **jev is advisory. It never holds the authority to pass or stop anything.**

That is CLAUDE.md §7 (an external service never owns a gate), and here it is
enforced with types rather than promised in prose.

## Not subscription-native — and off by default

Almost every harness in this repo runs inside the Claude Code subscription with
no API key. jev does not: it is a metered third-party cloud API (input
**$0.042/M tokens**, output free; measured 2026-10-02). So:

- **With no `TYPESAFE_API_KEY`, this plugin does nothing at all.** Not "degrades
  gracefully" — it performs zero network calls and returns a value that cannot
  influence any decision.
- Every call is appended to a local usage ledger, so spend is observable
  (`jev ledger`).

## The two properties that make it safe to add

### 1. The answer type cannot say "fine"

```rust
pub enum Advice {
    Escalate { question: String, probability: f64, detail: String },
    NoSignal { reason: NoSignalReason },
}
```

There is no `Clean` / `Ok` / `Pass`, and there will not be one. `NoSignal`
absorbs *all* of: no key, unreadable environment, HTTP error, timeout,
unparseable body, probability below the caller's threshold, **and jev actively
answering "no, this is fine"**.

That last one is the load-bearing choice. "jev approved" is deliberately
**indistinguishable** from "jev never ran", so a consumer cannot be written
that behaves differently when the key is missing. The absence is therefore
*structural*, not a documented intention. It also closes the mirror-image
fail-open: an advisor that cannot clear anything cannot fail open.

Adding a `Clean` variant does not merely violate a convention — it fails to
compile, because `Advice` is not `#[non_exhaustive]` and `Advice::render`
matches it exhaustively (measured 2026-10-02: the injection died on
`error[E0004]: non-exhaustive patterns`).

### 2. No coupling to the gate verdict

`harness_core::verdict::Verdict` does not appear in this crate's code in either
direction, and nothing here converts an `Advice` into one.
`tests/no_verdict_coupling.rs` scans the sources and fails if the token
returns.

The transport's three-valued result *is*
`harness_core::verdict::Determination`, which is deliberate: it is the repo's
shared "known / could-not-determine" container (CLAUDE.md §3 asks crates to
converge on it rather than reinvent one), it carries no verdict, and the only
`Verdict` reachable from it — through `Required::Blocked`'s unforgeable
`Undet` — is `Verdict::Undetermined`, which **blocks**. There is no route to
`Verdict::Clean`.

## "No key means no socket", as a counted fact

The central requirement is a negative, so it is measured rather than read off
the happy path. The transport is injected; `RecordingTransport` sends nothing
and counts invocations. `tests/no_key_no_network.rs` pins an exact count for
four environments:

| `TYPESAFE_API_KEY` | availability | exit code | transport calls |
|---|---|---|---|
| unset | `NotConfigured` | 1 | **0** |
| `""` / whitespace | `NotConfigured` | 1 | **0** |
| not valid UTF-8 | `Undetermined` | 10 | **0** |
| a real key | `Configured` | 0 | **1** |

Codes `1` and `10` both mean *do not use jev*, and they are separate on
purpose: a deliberate opt-out and a broken environment are different facts, and
folding the second into the first would report a malfunction as a choice.

**F→P oracle.** Those cases were observed RED before they were GREEN. With the
availability guard removed from `Client::ask` (the naive version that reads the
env into a bearer and calls anyway), all four absent-key assertions failed with
`left: 1, right: 0`. A test that has never failed proves nothing.

## Credential hygiene

- **Environment only.** There is no `--api-key` flag: a flag lands in shell
  history.
- **Never in argv.** beacon's `curl` pattern passes its request on the command
  line; that is fine for a webhook URL and not fine for a bearer token, because
  argv is readable through `ps` and `/proc/<pid>/cmdline` for the life of the
  request. This transport writes the URL, headers and body to a `curl -K -`
  **stdin config** instead.
- **Redacted twice** — by exact value (we hold the key) and by pattern
  (`Bearer …`, `apikey_…`), on every string that leaves the crate. The
  dangerous paths are error paths: a 401 body that echoes the key is the case
  `tests/no_credential_leak.rs` exercises.
- `Credential`'s `Debug` prints only the last four characters, so a `{:?}`
  added later by someone chasing a bug cannot leak it.
- The ledger records tokens, model, latency, status and **question keys** —
  never the evaluated `state`, which would turn an accounting file into a copy
  of everything the harness has looked at.

## Usage

```sh
jev check                 # three-valued availability as JSON; exit 0 / 1 / 10
echo '{"state": "...", "questions": {"q": {"type":"noul","instructions":"..."}}}' \
  | jev ask               # answers as JSON; no network call without a key
jev ledger                # records, tokens, estimated USD
```

Configuration lives in `~/.jev/config.toml` (`JEV_CONFIG` to relocate):

```toml
model = "jev-1.13.0"   # pin: see below
max_time_secs = 8
```

Env overrides: `JEV_MODEL`, `JEV_MAX_TIME_SECS`, `JEV_LEDGER`, `JEV_ENDPOINT`.
The key is **not** configurable here — environment only.

## Gotchas worth knowing before you use it

Measured 2026-10-02 from docs.typesafe.ai. This crate does not paper over any
of them, because callers need to know:

- **Counting degrades** as the count grows. Loop in code instead.
- **Date arithmetic is unreliable.** Extract fields as Choices, compute in code.
- **Framings disagree.** The same question as a Noul and as a Choice gives
  different answers, and negations do not sum to 1.0. Do not mix and compare.
- **Adversarial text moves the answer** — content that argues for its own
  classification can shift it. This is a core reason jev is advisory here: its
  input is often attacker-influenced.
- It answers your **literal** words, not your intent.
- `noul = 0.5` means "equally likely", not "medium". Use a Score for spectrums.
- **`jev-latest` drifts.** It resolves to a concrete version that changes, and a
  threshold tuned under one version drifts with it. Pin a version before tuning.
  The ledger records the model the *response* reported, so what actually
  answered is observable rather than assumed.

## Where it may and may not be wired

See [`docs/jev-integration-scenes.md`](../../docs/jev-integration-scenes.md).
Short version: ranking, dedup and human-facing annotation are fair game; gates
(donegate / reviewgate / propguard / tdd / parallelguard / precommit-audit /
budgetguard / condukt's completion gate / the F→P oracle) and anything under
`.githooks/` are permanently out of scope.

## Tests

```sh
cargo test -p jev     # offline; the networked transport is never constructed
```
