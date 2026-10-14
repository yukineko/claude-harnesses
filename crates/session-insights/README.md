# session-insights

Per-session **work metrics** for Claude Code. Two hooks roll up what actually
happened in a session — tool calls, turns, files touched — and derive a **size
class (XS–XL)** and a **work category** (coding / ops / research / mixed). View
it with `session-insights report`, or have each session logged as a dated note
in your **Obsidian vault**. Inspired by Devin's Session Insights, rebuilt as a
local, no-API-key hook.

Subscription-native: one bundled Rust binary, no daemon. Recording only writes
to its own state dir (and, opt-in, the vault) and always exits 0 — it never
blocks a turn.

## What it measures

| Hook | Records |
|---|---|
| **PostToolUse** | each tool call (per-tool counts, distinct files touched) — subcommand `record` |
| **Stop** | counts a turn; if `obsidian_log = true` also (re)writes the dated `sessions/` note, and if `record = true` refreshes the `/record` note — subcommand `stop` |
| **SessionEnd** | opt-in (`record = true`), writes/updates the `/record` note — subcommand `sessionend` |

From that it derives:
- **size**: XS / S / M / L / XL by total tool events (thresholds configurable)
- **category**: `coding` (Edit/Write/MultiEdit/NotebookEdit), `ops` (Bash), `research`
  (Read/Grep/Glob/WebFetch/WebSearch) when that group is at least half of the categorized
  events; otherwise `mixed` (`empty` when there are none)
- per-session turns, tool events, file count, and top tools

## Report

```sh
session-insights report          # latest session
session-insights report --session <id-prefix>
session-insights report --all    # one line per recorded session
session-insights report --context # also join context-governor ledger health
```

If the session store cannot be read, `report` prints `unknown — session store could
not be read: …` to stderr and exits 1 (it does not print "no sessions").

```
session a1b2c3d4  [2026-06-20T18:00:00+09:00]
  project: playbook
  size: L   category: coding
  turns: 12   tool events: 47   files: 9
  top tools: Edit 18, Bash 12, Read 9, Write 5, Grep 3
```

## `/record` note (Obsidian, opt-in)

Set `record = true` (and point `obsidian_vault` at your vault). On every Stop and
on SessionEnd — and on demand via the `/record` command, which writes even when
`record = false` — session-insights writes an AEGIS-style
Markdown note to `<vault>/<record_dir>/<date>-<project>-<id>.md`, only if the
vault directory already exists. The deterministic `## 数値サマリ` and `## コスト`
blocks (fenced by machine-owned `<!-- si:numeric:* -->` / `<!-- si:cost:* -->`
markers) are filled by the binary; the prose sections stay as `<!-- fill: … -->`
placeholders.

```sh
session-insights record-now              # (re)generate the note, print its path
session-insights record-now --session <id>
```

The `/record` slash command runs `record-now`, has a Sonnet subagent distill the
transcript into the prose sections, then reconciles the standalone `backlog`
(closes finished items, adds open `## 残課題` follow-ups). Re-runs merge in place:
only the machine-owned blocks are refreshed, your prose is preserved.

## Backlog (cross-session open issues)

The durable cross-session queue is the **standalone [`backlog`](../backlog)
crate** (per repo: `<repo root>/.backlog/tasks.toml`), which is now the *single canonical* queue and
injects pending tasks at SessionStart via its own hook. session-insights no
longer keeps an independent backlog store — its old `session-insights backlog`
subcommands and the `<vault>/backlog.md` / `backlog.json` store have been
removed.

```sh
backlog add --title "rebuild darwin-x86_64 on an x86 Mac" --project harness
backlog list --project harness --status pending
backlog done <id> --test "cargo test -p x" --red-rev <rev>   # close with evidence
```

Store writes (`add` / `done` / …) are refused in the primary working tree — run them
from a linked worktree. A bare `backlog done <id>` is refused: closing needs evidence
(`--test … --red-rev …`, `--doc-only <commit>` or `--duplicate-of <id>`), and an
`add` without `--repro-test` lands as `unconfirmed`, outside the workable queue. See
the backlog README for the exact rules.

The `/record` command reconciles this backlog: it closes what the session finished
and adds genuinely-open follow-ups from `## 残課題` (`backlog add`). Note that the
command text (`commands/record.md`) still tells the model to close items with a
bare `backlog done <id>`, which the current backlog CLI refuses.

### One-time migration from the old session-insights backlog

If you used the old `session-insights backlog` and have a `backlog.json` in your
state dir, migrate its open items into the standalone backlog **once**, from a
linked worktree of the target repo. The script below is idempotent (the
standalone `backlog add` refuses a duplicate title+project) and is a no-op when
`backlog.json` is empty or absent. Migrated items have no repro test, so they
land as `unconfirmed`:

```sh
# Default state dir is ~/.session-insights/state (override = state_dir in
# session-insights.toml; adjust BACKLOG_JSON below if you set a custom one).
STATE_DIR="$HOME/.session-insights/state"
BACKLOG_JSON="$STATE_DIR/backlog.json"
if [ -s "$BACKLOG_JSON" ]; then
  jq -r '.[] | select(.status=="open") | [.project, .text] | @tsv' "$BACKLOG_JSON" \
    | while IFS=$'\t' read -r project text; do
        [ -n "$text" ] && backlog add --title "$text" --project "${project:-default}"
      done
else
  echo "no backlog.json to migrate (nothing to do)"
fi
```

`backlog add` now REFUSES (non-zero, message on stderr) a duplicate, or when its
cross-session claim check cannot be made (use `--force` to add anyway); the loop
above does not stop on a refusal. Before deleting `backlog.json`, verify that
every open item actually migrated (no refusal messages, and `backlog list`
shows each one) — a refused item exists only in `backlog.json`.

After migrating and verifying, you can delete `backlog.json` (and the auto-generated
`<vault>/backlog.md`, which was a session-insights render artifact and is no
longer produced).

## Obsidian logging (opt-in)

Set `obsidian_log = true` and point `obsidian_vault` at your vault. On every Stop
the session is written/overwritten to `<vault>/sessions/<date>-<id>.md` with
frontmatter (`type: session`, size, category, turns…) — only if the vault dir
already exists (it never creates the vault).

## Install (plugin)

The bundled `hooks/hooks.json` wires all three hooks automatically (PostToolUse
matcher: `Bash|Edit|MultiEdit|Write|Read|Grep|Glob|WebFetch|WebSearch|NotebookEdit`). Drop a
`session-insights.toml` to tune thresholds or enable Obsidian logging.

## Standalone (cargo)

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

## Config

Drop `./session-insights.toml` (project) or `~/.session-insights/config.toml`
(global); the first that exists wins. A file that fails to parse is ignored as a
whole and the built-in defaults apply (no warning is printed — check
`session-insights status`).

| key | meaning | default |
|---|---|---|
| `enabled` | master switch for the hooks | `true` |
| `size_thresholds` | `[S, M, L, XL]` lower bounds by tool events (must have exactly 4 entries, else ignored) | `[5, 15, 40, 100]` |
| `ignore_tools` | tools excluded from metrics | `["TodoWrite"]` |
| `obsidian_log` | write the dated `sessions/` note on every Stop | `false` |
| `obsidian_vault` | vault root (notes go under `sessions/` and `records/`) | `~/Documents/vault/yukineko` |
| `record` | write/update the `/record` note on Stop and SessionEnd | `false` |
| `record_dir` | vault subdir for record notes | `records` |
| `state_dir` | rollup state directory | `~/.session-insights/state` |
| `price_overrides` | per-model cost overrides for the `## コスト` block | (built-in rates) |

Kill switch: `SESSION_INSIGHTS_DISABLE` set to any non-empty value other than `0`.

## Build

```sh
make bins     # refresh bin/session-insights-darwin-<arch> and -linux-x86_64
cargo test
```

Only the `bin/session-insights` launcher is committed; the per-platform
`bin/session-insights-<os>-<arch>` builds are git-ignored. In this repo the
installed plugin gets its binary from `scripts/rollout-plugins.sh` (which builds
from source and copies into the plugin cache), not from git. When no build exists
for the host, the launcher does not pretend to have run: `record` writes a stderr
line, `stop` emits a `systemMessage`, `sessionend` exits 1, and any CLI verb exits
127. No API key is needed at runtime.
