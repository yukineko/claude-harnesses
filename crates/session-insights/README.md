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
| **Stop** | counts a turn — subcommand `stop` |
| **SessionEnd** | opt-in, writes the Obsidian session note and the `/record` note — subcommand `sessionend` |

From that it derives:
- **size**: XS / S / M / L / XL by total tool events (thresholds configurable)
- **category**: `coding` (Edit/Write), `ops` (Bash), `research` (Read/Grep/Web), or `mixed`
- per-session turns, tool events, file count, and top tools

## Report

```sh
session-insights report          # latest session
session-insights report --session <id-prefix>
session-insights report --all    # one line per recorded session
session-insights report --context # also join context-governor ledger health
```

```
session a1b2c3d4  [2026-06-20T18:00:00+09:00]
  project: playbook
  size: L   category: coding
  turns: 12   tool events: 47   files: 9
  top tools: Edit 18, Bash 12, Read 9, Write 5, Grep 3
```

## `/record` note (Obsidian, opt-in)

Set `record = true` (and point `obsidian_vault` at your vault). On SessionEnd —
and on demand via the `/record` command — session-insights writes an AEGIS-style
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

The durable, cross-project queue is the **standalone [`backlog`](../backlog)
crate** (`~/.backlog/tasks.toml`), which is now the *single canonical* queue and
injects pending tasks at SessionStart via its own hook. session-insights no
longer keeps an independent backlog store — its old `session-insights backlog`
subcommands and the `<vault>/backlog.md` / `backlog.json` store have been
removed.

```sh
backlog add --title "rebuild darwin-x86_64 on an x86 Mac" --project harness
backlog list --project harness --status pending
backlog done <id>          # close a finished item
```

The `/record` command reconciles this backlog automatically: it closes what the
session finished (`backlog done <id>`) and adds genuinely-open follow-ups from
`## 残課題` (`backlog add`).

### One-time migration from the old session-insights backlog

If you used the old `session-insights backlog` and have a `backlog.json` in your
state dir, migrate its open items into the standalone backlog **once**. The
script below is idempotent (the standalone `backlog add` dedups by project+title)
and is a safe no-op when `backlog.json` is empty or absent:

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

Set `obsidian_log = true` and point `obsidian_vault` at your vault. On SessionEnd
the session is written/overwritten to `<vault>/sessions/<date>-<id>.md` with
frontmatter (`type: session`, size, category, turns…) — only if the vault dir
already exists (it never creates the vault).

## Install (plugin)

The bundled `hooks/hooks.json` wires both hooks automatically. Drop a
`session-insights.toml` to tune thresholds or enable Obsidian logging.

## Standalone (cargo)

```sh
cargo install --path .
session-insights install      # merge the PostToolUse + Stop + SessionEnd hooks
session-insights report --all
session-insights status        # resolved config
session-insights uninstall
```

`install`/`uninstall` are idempotent, back up `settings.json`, and preserve
foreign hook groups.

## Config

Drop `./session-insights.toml` (project) or `~/.session-insights/config.toml`
(global); the first that exists wins.

| key | meaning | default |
|---|---|---|
| `size_thresholds` | `[S, M, L, XL]` lower bounds by tool events | `[5, 15, 40, 100]` |
| `ignore_tools` | tools excluded from metrics | `["TodoWrite"]` |
| `obsidian_log` | write the dated `sessions/` note on SessionEnd | `false` |
| `obsidian_vault` | vault root (notes go under `sessions/` and `records/`) | `~/Documents/vault/yukineko` |
| `record` | write/update the `/record` note on SessionEnd | `false` |
| `record_dir` | vault subdir for record notes | `records` |
| `state_dir` | rollup state directory | `~/.session-insights/state` |
| `price_overrides` | per-model cost overrides for the `## コスト` block | (built-in rates) |

Kill switch: `SESSION_INSIGHTS_DISABLE=1`.

## Build

```sh
make bins     # refresh bin/session-insights-darwin-<arch> and -linux-x86_64
cargo test
```

The committed `bin/session-insights-*` binaries are what the plugin ships, so end
users need neither cargo nor an API key.
