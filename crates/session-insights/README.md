 Inspired by Devin's Session Insights, rebuilt as a local, no-API-key hook.

- **PostToolUse** records each tool call (per-tool counts, distinct files touched) — subcommand `record`.
- **Stop** counts a turn; if `obsidian_log = true` also (re)writes the dated `sessions/` note, and if `record = true` refreshes the `/record` note — subcommand `stop`.
- **SessionEnd** is opt-in (`record = true`) and writes/updates the `/record` note — subcommand `sessionend`.
- The hooks never block and exit 0, but an empty or unparseable payload is not
  dropped silently: each hook says on stderr that the event was NOT recorded,
  and `stop` also appends an `{"event":"unparseable_payload","hook":"stop",...}`
  line to `<state_dir>/log.jsonl` and prints exactly one short `systemMessage`
  line, `session-insights: セッションを記録できませんでした` (user ruling
  2026-10-27: one-line results at Stop, no logs).
- **size**: XS / S / M / L / XL by total tool events (thresholds configurable)
- **category**: `coding` (Edit/Write/MultiEdit/NotebookEdit), `ops` (Bash), `research`
  (Read/Grep/Glob/WebFetch/WebSearch) when that group is at least half of the categorized
  events; otherwise `mixed` (`empty` when there are none)
- per-session turns, tool events, file count, and top tools

```sh
session-insights report          # latest session
session-insights report --session <id-prefix>
session-insights report --all    # one line per recorded session
session-insights report --context # also join context-governor ledger health
```

If the session store cannot be read, `report` prints `unknown — session store could
not be read: …` to stderr and exits 1 (it does not print "no sessions").
A single session file that cannot be read or parsed is not skipped silently:
`report --all` still lists the readable sessions, then names the unreadable
file(s) on stderr after `incomplete — ` and exits 1;
`report` (latest) and `report --session` exit 1 as `unknown` when an unreadable
file could be the answer.

```
session a1b2c3d4  [2026-06-20T18:00:00+09:00]
  project: playbook
  size: L   category: coding
  turns: 12   tool events: 47   files: 9
  top tools: Edit 18, Bash 12, Read 9, Write 5, Grep 3
```

Set `record = true` (and point `obsidian_vault` at your vault). On every Stop and
on SessionEnd — and on demand via `record-now`, which writes even when
`record = false` — session-insights writes a
Markdown note to `<vault>/<record_dir>/<date>-<project>-<id>.md`, only if the
vault directory already exists.
When `record-now` writes no note it prints nothing on stdout, says why on
stderr, and exits 1. The deterministic `## 数値サマリ` and `## コスト`
blocks (fenced by machine-owned `<!-- si:numeric:* -->` / `<!-- si:cost:* -->`
markers) are filled by the binary; the prose sections stay as `<!-- fill: … -->`
placeholders.

```sh
session-insights record-now              # (re)generate the note, print its path
session-insights record-now --session <id>
```

Re-runs merge in place:
only the machine-owned blocks are refreshed, your prose is preserved.

The cross-session queue is the **standalone [`backlog`](../backlog)
crate** (per repo: `<repo root>/.backlog/tasks.toml`), whose `backlog session-start`
command injects the pending tasks. session-insights no
longer keeps an independent backlog store — its old `session-insights backlog`
subcommands and the `<vault>/backlog.md` / `backlog.json` store have been
removed.

```sh
backlog add --title "rebuild darwin-x86_64 on an x86 Mac" --project harness --repro-test "<cmd>"
backlog list --project harness --status pending
backlog done <id> --test "cargo test -p x" --red-rev <rev>   # close with evidence
```

Store writes (`add` / `done` / …) are refused in the primary working tree — run them
from a linked worktree. A bare `backlog done <id>` is refused.
Closing needs evidence: `--test … --red-rev …`, `--doc-only <commit>` or `--duplicate-of <id>`.
An `add` without a reproducing `--repro-test` is refused and writes nothing to the store.
See the backlog README for the exact rules.

## Enable Obsidian logging (opt-in)

Set `obsidian_log = true` and point `obsidian_vault` at your vault. On every Stop
the session is written/overwritten to `<vault>/sessions/<date>-<id>.md` with
frontmatter (`type: session`, size, category, turns…) — only if the vault dir
already exists (it never creates the vault).

## Install (plugin)

The bundled `hooks/hooks.json` wires all three hooks automatically (PostToolUse
matcher: `Bash|Edit|MultiEdit|Write|Read|Grep|Glob|WebFetch|WebSearch|NotebookEdit`). Drop a
`session-insights.toml` to tune thresholds or enable Obsidian logging.

```sh
cargo install --path .
session-insights install      # merge the PostToolUse + Stop + SessionEnd hooks
session-insights report --all
session-insights status        # resolved config
session-insights init          # create the state dir
session-insights uninstall
```

`install`/`uninstall` are idempotent, back up `settings.json`, and preserve
foreign hook groups.

Drop `./session-insights.toml` (project) or `~/.session-insights/config.toml`
(global); the first that exists wins.
A chosen file that cannot be read or parsed, or a `size_thresholds` that is not
exactly 4 entries, is not silently replaced by defaults: the problem is written
to stderr on every load, and `status` marks the config INVALID and exits 1.

- `enabled` — master switch for the hooks; default `true`
- `size_thresholds` — `[S, M, L, XL]` lower bounds by tool events; default `[5, 15, 40, 100]`
- `ignore_tools` — tools excluded from metrics; default `["TodoWrite"]`
- `obsidian_log` — write the dated `sessions/` note on every Stop; default `false`
- `obsidian_vault` — vault root (notes go under `sessions/` and `records/`); default `~/Documents/vault/yukineko`
- `record` — write/update the `/record` note on Stop and SessionEnd; default `false`
- `record_dir` — vault subdir for record notes; default `records`
- `state_dir` — rollup state directory; default `~/.session-insights/state`
- `price_overrides` — per-model cost overrides for the `## コスト` block; default (built-in rates)

Kill switch: `SESSION_INSIGHTS_DISABLE` set to any non-empty value other than `0`.

```sh
make bins
cargo test
```

Only the `bin/session-insights` launcher is committed; the per-platform
`bin/session-insights-<os>-<arch>` builds are git-ignored. When no build exists
for the host, the launcher does not pretend to have run: `record` writes a stderr
line, `stop` emits a `systemMessage`, `sessionend` exits 1, and any CLI verb exits
127.
