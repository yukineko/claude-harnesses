# askrelay

A subagent's permission **Ask is adjudicated in the main session** — user
ruling 2026-11-14 (verbatim): 「sub agentのAskはmainで裁定する」,
「この仕組みはコードで強制する、どうせわすれる」,
「gateにそんな例外は追加してほしくない」,
「gateもconduktも絶対に勝手に他のworktreeを参照してはならない」.

Inside a subagent nobody can answer a permission prompt, so a gate (today
blastguard) hardens an `Ask` to a `Deny` there. `askrelay` makes sure the main
session adjudicates that Ask instead of forgetting it. It is a **GATE crate**
(its Stop hook blocks). It moved out of blastguard on 2026-11-17.

## Procedure

1. **Record** — done by the gate, not by askrelay. blastguard appends a
   `queued` record (id `bgr-<12 hex>`) to `~/.blastguard/relay/<session_id>.jsonl`
   (override: `BLASTGUARD_RELAY_DIR`) and names the id in the deny reason.
2. **Executed** (`askrelay mark-executed`, PostToolUse / PostToolUseFailure on
   Bash) — a main-session Bash call whose command and cwd are byte-identical to
   a pending **Verified** entry (see Trust boundary) marks it executed. A
   subagent's call marks nothing.
3. **Relayed** (`askrelay mark-relayed`, PostToolUse on SendMessage) — a
   main-session SendMessage naming the relay id marks an executed Verified
   entry relayed.
4. **Stop** (`askrelay stop`) — blocks while any Verified entry of the session
   is neither executed-and-relayed nor dismissed, and lists id, agent_id, cwd
   and exact command with the run procedure. A pending Undetermined entry also
   blocks, but WITHOUT the run procedure (inspect or dismiss it); a pending
   Refuted entry does not block and is named in a `systemMessage`. It blocks on
   re-entry too. An unreadable queue, an unparseable line, an event naming an
   unknown id, an unparseable Stop payload, a panic (`run_guarded`) and a
   missing platform binary (launcher) all block (fail-closed). No queue file
   allows.
5. **Dismiss** (`askrelay dismiss <id> --reason "<text>"`) — for a prompt the
   human declined or a command that cannot be re-run. A blank reason, an
   unknown id, or a row whose own `session_id` does not match the queue file it
   sits in (forged provenance; nothing is written) is exit 2; an unreadable
   queue is exit 3. Dismissals are recorded in the queue file and no longer
   shown on later Stops.

It never executes a command and never resolves, stats or enters the stored cwd.

## Trust boundary (backlog #1808)

The queue is not a trusted channel: **any local process can append a row to
any session's queue file** (a synthetic PreToolUse payload with a real session
id and an invented `agent_id` is enough). The writer (blastguard) stays
untrusted; the readers verify each row. **The trust root is the session
transcript** — the `transcript_path` Claude Code puts in the Stop /
PostToolUse payload. A row is:

* **Verified** — its own `session_id` is the session of its file, and its
  `agent_id` has a sidechain `<transcript dir>/<session>/subagents/agent-<agent_id>.jsonl`;
* **Refuted** — a session mismatch, or the subagents directory was listed and
  has no sidechain for its `agent_id`;
* **Undetermined** — no `transcript_path`, an unreadable transcript or
  subagents directory, or no `agent_id`. Never treated as Refuted.

Two Claude processes sharing one session id share its queue file and its
subagents directory, so the subagents of either are Verified (user ruling
2026-11-17). blastguard's main-session denial record (`mark_denied`) has no
transcript, so it skips only session-mismatched rows; a `denied` line never
resolves an entry.

**Known limit**: a forged row that names an `agent_id` which really exists in
that session is classified Verified. This design does not detect it.

## Layout

* `crates/harness-core/src/relay.rs` — the queue format and operations, shared
  with blastguard (which enqueues). It lives in harness-core so blastguard does
  not depend on this crate and this crate does not depend on blastguard.
* `src/main.rs` — the hooks and `dismiss`; `src/protection.rs` — `--protects`.
* `hooks/hooks.json` — Stop, PostToolUse (Bash, SendMessage),
  PostToolUseFailure (Bash).

## Compatibility and fail-closed behaviour

The queue path, the `BLASTGUARD_RELAY_DIR` variable, the `bgr-` id prefix and
the JSONL format are unchanged from when the relay lived in blastguard.
blastguard keeps `harden_for_subagent`, so a subagent's `Ask` is still a
`Deny` when askrelay is absent or disabled; what is lost then is only the
main-session Stop reminder.

Panic barriers: `stop` runs under `harness_core::gate::run::run_guarded`
(a panic blocks). `dismiss` is an operator command whose exit status is its
result. `mark-*` decide nothing (a lost mark leaves the entry pending, the
restrictive side) and report failures on stderr.

## Tests

`cargo test -p askrelay --test <name>`. The relay tests drive both binaries:
`askrelay` for the hooks and `dismiss`, and blastguard (built on demand by
`tests/support/bounded.rs`) for the enqueue and denial-record paths.
