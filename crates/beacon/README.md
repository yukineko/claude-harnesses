The hook can only *notify*: `beacon notify` exits 0 with empty stdout (never a block decision) on a valid payload, on malformed or empty stdin, and when a channel fails.

beacon is wired to exactly two Claude Code events, Stop and Notification.

- On **Stop** (turn finished) the title is "✅ \<project\> — 完了" and the default body is the first 160 characters (`snippet_chars`) of Claude's last message, followed by "…" when it is longer. With no transcript the body is "ターンが完了しました。"
- On **Notification** (needs input/permission) the title is "🔔 \<project\> — 確認" and the body is Claude's own notification text.

Enable channels in `beacon.toml`; several can be active at once:

- **desktop** — macOS `osascript` notification (optional `sound`).
- **slack_webhook** — Slack incoming webhook (`{"text": …}`). Prefer the
  `BEACON_SLACK_WEBHOOK` env var so the URL isn't committed; it overrides the file.
- **webhook** — generic endpoint; receives `{event, project, title, body}` as a JSON POST (`BEACON_WEBHOOK` overrides).
- **command** — escape hatch; a shell command run through the shell with `BEACON_EVENT`,
  `BEACON_PROJECT`, `BEACON_TITLE`, `BEACON_BODY` in the environment. A `command`
  coming from a *project-local* `beacon.toml` is ignored until you run
  `beacon trust` in that project (workspace-trust gate, à la git `safe.directory`
  / `HARNESS_TRUST_ALL`); a home/default-config `command` needs no trust.

Network channels shell out to `curl --max-time 8`; no HTTP client crate is a direct dependency.

The bundled `hooks/hooks.json` wires both Stop and Notification to
`bin/beacon notify`. Drop a `beacon.toml` in your project
(or `~/.beacon/config.toml`) to choose channels; without one, desktop
notifications are on by default.

## Install standalone (cargo)

```sh
cargo install --path .
beacon init          # write a starter ./beacon.toml (--force to overwrite)
beacon test          # fire a sample notification through configured channels
beacon install       # merge the Stop + Notification hooks into ~/.claude/settings.json
beacon status        # show the resolved config and each channel's setting
beacon trust         # trust this project so its beacon.toml `command` is honored
beacon uninstall     # remove them again
```

`beacon install`/`uninstall` accept `--dry-run`, are idempotent, back up `settings.json` before any
write, and preserve foreign hook groups.

See [`beacon.example.toml`](beacon.example.toml). Key knobs: `on_stop`,
`on_notification`, `include_snippet`/`snippet_chars`, the channel fields, and
`log`. `BEACON_DISABLE=1` silences the hook (`beacon notify`); `beacon test` still sends.

## Build the per-platform binaries

```sh
make bins     # refresh bin/beacon-darwin-<arch> and bin/beacon-linux-x86_64
make mac      # just the native macOS binary
make linux    # just the Linux x86_64 cross-build (cargo-zigbuild)
cargo test
```

Only the `bin/beacon` launcher is committed; the per-platform `bin/beacon-<os>-<arch>`
builds are git-ignored.
