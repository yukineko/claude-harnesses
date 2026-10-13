# beacon

Desktop & webhook notifications for Claude Code — a pair of **Stop** and
**Notification** hooks that ping you when a turn finishes or Claude needs your
input, so you can walk away from a long session and get pulled back at the right
moment. Inspired by Devin's Slack notifications, rebuilt as a tiny local hook.

Subscription-native: one bundled Rust binary, **no API key**, no daemon. The
hook can only *notify* — it never blocks a turn and always exits 0, so a missing
`curl`, a denied notification, or empty stdin costs nothing.

## What it does

| Claude Code event | beacon notifies | default body |
|---|---|---|
| **Stop** (turn finished) | "✅ \<project\> — 完了" | tail of Claude's last message |
| **Notification** (needs input/permission) | "🔔 \<project\> — 確認" | Claude's own notification text |

## Channels

Enable any combination in `beacon.toml`:

- **desktop** — macOS `osascript` notification (optional `sound`), Linux `notify-send`.
- **slack_webhook** — Slack incoming webhook (`{"text": …}`). Prefer the
  `BEACON_SLACK_WEBHOOK` env var so the URL isn't committed; it overrides the file.
- **webhook** — generic endpoint; receives `{event, project, title, body}` as a JSON POST (`BEACON_WEBHOOK` overrides).
- **command** — escape hatch; a shell command run via `sh -c` with `BEACON_EVENT`,
  `BEACON_PROJECT`, `BEACON_TITLE`, `BEACON_BODY` in the environment. A `command`
  coming from a *project-local* `beacon.toml` is ignored until you run
  `beacon trust` in that project (workspace-trust gate, à la git `safe.directory`
  / `HARNESS_TRUST_ALL`); a home/default-config `command` needs no trust.

Network channels shell out to `curl --max-time 8`; no HTTP stack is linked in.

## Why it exists

On a long session you want to step away while Claude keeps working — but then you
miss the moment a turn finishes, or the moment it stalls waiting for a permission
or an input, and time is wasted either way (as is sitting there watching it).
beacon solves that "when do I look back?" problem by notifying on just the Stop
and Notification events. Inspired by Devin's Slack notifications, it's rebuilt as
a tiny local hook with no external-service dependency, and — because the hook can
only notify and never blocks a turn — a failed notification never affects the
session: it's strictly fail-safe, "nice to have, never breaks anything."

## Install (plugin)

Installed via the plugin marketplace, the bundled `hooks/hooks.json` wires both
hooks automatically — nothing else to do. Drop a `beacon.toml` in your project
(or `~/.beacon/config.toml`) to choose channels; without one, desktop
notifications are on by default.

## Standalone (cargo)

```sh
cargo install --path .
beacon init          # write a starter ./beacon.toml (--force to overwrite)
beacon test          # fire a sample notification through configured channels
beacon install       # merge the Stop + Notification hooks into ~/.claude/settings.json
beacon status        # show resolved config + active channels
beacon trust         # trust this project so its beacon.toml `command` is honored
beacon uninstall     # remove them again
```

`beacon install`/`uninstall` accept `--dry-run`, are idempotent, back up `settings.json` before any
write, and preserve foreign hook groups.

## Config

See [`beacon.example.toml`](beacon.example.toml). Key knobs: `on_stop`,
`on_notification`, `include_snippet`/`snippet_chars`, the channel fields, and
`log`. `BEACON_DISABLE=1` silences everything.

## Build

```sh
make bins     # refresh bin/beacon-darwin-<arch> and bin/beacon-linux-x86_64
make mac      # just the native macOS binary
make linux    # just the Linux x86_64 cross-build (cargo-zigbuild)
cargo test
```

Only the `bin/beacon` launcher is committed; the per-platform `bin/beacon-<os>-<arch>`
builds are git-ignored. In this repo the installed plugin gets its binary from
`scripts/rollout-plugins.sh` (build from source + copy into the plugin cache), not
from git. No API key is needed at runtime.
