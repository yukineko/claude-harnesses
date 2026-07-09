# stuckguard

> **Stuck-loop detector + escalation for Claude Code**, written in Rust.

Agents get stuck: they rerun the same failing command, or edit a file back and
forth without converging. `stuckguard` is a **PostToolUse** hook that watches the
stream of tool calls, spots these loops deterministically, and injects an
escalating nudge — first "step back and try another approach", then "stop and ask
the user". It is the confidence/ask-for-help reflex Devin's harness has, as a
small local binary.

It only ever **injects advice**. It cannot block a tool call or end a turn, so a
false positive costs at most one extra line of context. No API key.

## What it detects

| Signal | Trips when |
|---|---|
| **repeat** | the same normalized `(tool, input)` runs `repeat_threshold` times in the recent window (e.g. the same `cargo test` 3×). Flags "（毎回失敗しています）" if each also errored. |
| **oscillation** | edit thrash: a file is edited X→Y then Y→X repeatedly (`oscillation_threshold` reversals), i.e. a change keeps getting undone and redone. |

Repeat detection also supports **near-repeat matching**: with `similarity_threshold`
set below `1.0`, two calls of the same tool count as a repeat once their Jaccard
token-bag similarity crosses the threshold, even if their exact signature differs.
The default `1.0` preserves the original exact-match-only behavior.

Below the hard repeat/oscillation escalation there is an optional, softer
**progress-score advisory** (off by default via `progress_advisory_enabled`). It
combines 3 signals over the recent window — action diversity, state-hash
stability, and error-digest recurrence — into a `progress_score` in `[0, 1]`; once
the window is at least `progress_min_window` long and the score reaches
`progress_score_threshold`, it injects an early "progress may be stalling"
nudge. It never replaces or blocks the hard escalation above.

## How it works

`stuckguard watch` is wired to the **PostToolUse** hook. On each tool call it:

1. builds a stable **signature** of the call (normalized command / file+before/after
   hashes for edits) — `DefaultHasher`, deterministic across processes;
2. appends it to a per-session **ring buffer** (`window` events) on disk;
3. runs the detectors over the window; oscillation outranks repeat;
4. on a trip, unless the pattern is in **cooldown**, injects a nudge via
   `additionalContext` and bumps that pattern's nudge count;
5. once a pattern has been nudged `escalate_after` times, the message escalates
   to an explicit **"stop and ask the user"**.

Everything is local: state under `~/.stuckguard/state/`, one JSONL line per
nudge in `log.jsonl`.

Each recorded event also carries a `failed_test_digest`: a deterministic hash
of the normalized error text (paths, line numbers, and addresses stripped) when
the tool call errored. It feeds the error-digest-recurrence signal above and
doubles as a retrieval key against the cross-project lessons store. On
escalation, stuckguard writes an error-pattern lesson for this stuck pattern and
looks up a relevant prior lesson to include in the escalation message — both
steps are fail-soft (a missing/corrupt lessons store never blocks the nudge).

## Install

```sh
cargo install --path .
cd your/project
stuckguard init        # optional: writes a starter stuckguard.toml
stuckguard install     # merges the PostToolUse hook into ~/.claude/settings.json (backs it up)
stuckguard status      # show resolved config
```

Remove with `stuckguard uninstall`. Kill switch: `STUCKGUARD_DISABLE=1`.

## Config

See [`stuckguard.example.toml`](stuckguard.example.toml).

| key | meaning | default |
|---|---|---|
| `window` | recent tool events inspected per session | 12 |
| `repeat_threshold` | identical actions in window ⇒ nudge | 3 |
| `similarity_threshold` | Jaccard token-bag similarity (`[0, 1]`) above which two same-tool calls count as a near-repeat even if their exact signature differs; `1.0` = exact-match only (legacy behavior) | 1.0 |
| `oscillation_threshold` | edit reversals on one file ⇒ nudge | 2 |
| `cooldown_events` | suppress re-nudging a pattern within N events | 6 |
| `escalate_after` | nudges before "ask the user" | 2 |
| `ignore_tools` | tools excluded from detection | `["TodoWrite"]` |
| `progress_advisory_enabled` | enable the soft, 3-signal progress-score stall advisory (fires below the hard repeat/oscillation escalation) | false |
| `progress_min_window` | minimum recent-window length before the advisory can fire | 6 |
| `progress_score_threshold` | `progress_score` (`[0, 1]`) at or above which the advisory fires | 0.75 |

## Relation to the other harnesses

- `ctxrot` — keeps context healthy over long sessions.
- `donegate` — won't let the agent declare done until checks pass.
- **`stuckguard`** — won't let the agent grind forever; makes it escalate.

## License

MIT
