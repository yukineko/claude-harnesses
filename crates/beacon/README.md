The hook can only *notify*: `beacon notify` exits 0 with empty stdout (never a block decision) on a valid payload, on malformed or empty stdin, and when a channel fails [test: crates/beacon/tests/integration.rs::notify_with_malformed_stdin_exits_zero] [test: crates/beacon/tests/prose_claims.rs::claim_exit_zero_never_blocks].

beacon is wired to exactly two Claude Code events, Stop and Notification [test: crates/beacon/tests/prose_claims.rs::claim_two_events_only].

- On **Stop** (turn finished) the title is "✅ \<project\> — 完了" and the default body is the first 160 characters (`snippet_chars`) of Claude's last message, followed by "…" when it is longer [test: crates/beacon/tests/prose_claims.rs::claim_stop_title_body]. With no transcript the body is [test: crates/beacon/tests/prose_claims.rs::claim_stop_title_body] "ターンが完了しました。"
- On **Notification** (needs input/permission) the title is "🔔 \<project\> — 確認" and the body is Claude's own notification text [test: crates/beacon/tests/prose_claims.rs::claim_notification_title_body].

Enable channels in `beacon.toml`; several can be active at once: [test: crates/beacon/tests/prose_claims.rs::claim_any_combination]

- **desktop** — macOS `osascript` notification (optional `sound`) [test: crates/beacon/tests/prose_claims.rs::claim_desktop_channel].
- **slack_webhook** — Slack incoming webhook (`{"text": …}`) [test: crates/beacon/tests/prose_claims.rs::claim_slack_payload_env_override]. Prefer the
  `BEACON_SLACK_WEBHOOK` env var so the URL isn't committed; it overrides the file [test: crates/beacon/tests/prose_claims.rs::claim_slack_payload_env_override].
- **webhook** — generic endpoint; receives `{event, project, title, body}` as a JSON POST (`BEACON_WEBHOOK` overrides) [test: crates/beacon/tests/prose_claims.rs::claim_webhook_payload_env_override].
- **command** — escape hatch; a shell command run through the shell with `BEACON_EVENT`,
  `BEACON_PROJECT`, `BEACON_TITLE`, `BEACON_BODY` in the environment [test: crates/beacon/tests/prose_claims.rs::claim_command_env_vars]. A `command`
  coming from a *project-local* `beacon.toml` is ignored until you run
  `beacon trust` in that project (workspace-trust gate, à la git `safe.directory`
  / `HARNESS_TRUST_ALL`); a home/default-config `command` needs no trust [test: crates/beacon/src/config.rs::project_command_is_gated_behind_workspace_trust] [test: crates/beacon/tests/prose_claims.rs::claim_trust_cli].

Network channels shell out to `curl --max-time 8` [test: crates/beacon/tests/prose_claims.rs::claim_curl_max_time_no_http_stack]; no HTTP client crate is a direct dependency [test: crates/beacon/tests/prose_claims.rs::claim_curl_max_time_no_http_stack].

The bundled `hooks/hooks.json` wires both Stop and Notification to
`bin/beacon notify` [test: crates/beacon/tests/prose_claims.rs::claim_two_events_only]. Drop a `beacon.toml` in your project
(or `~/.beacon/config.toml`) to choose channels; without one, desktop
notifications are on by default [test: crates/beacon/tests/prose_claims.rs::claim_config_locations_default_desktop].

## Install standalone (cargo)

```sh
cargo install --path .
beacon init          # write a starter ./beacon.toml (--force to overwrite) [test: crates/beacon/tests/prose_claims.rs::claim_init_force]
beacon test          # fire a sample notification through configured channels [test: crates/beacon/tests/prose_claims.rs::claim_test_fires_sample]
beacon install       # merge the Stop + Notification hooks into ~/.claude/settings.json [test: crates/beacon/tests/prose_claims.rs::claim_install_merges_hooks]
beacon status        # show the resolved config and each channel's setting [test: crates/beacon/tests/prose_claims.rs::claim_status_active_channels]
beacon trust         # trust this project so its beacon.toml `command` is honored [test: crates/beacon/tests/prose_claims.rs::claim_trust_cli]
beacon uninstall     # remove them again [test: crates/beacon/tests/prose_claims.rs::claim_uninstall_removes]
```

`beacon install`/`uninstall` accept `--dry-run`, are idempotent, back up `settings.json` before any
write, and preserve foreign hook groups [test: crates/beacon/tests/prose_claims.rs::claim_install_dryrun_idempotent_backup_foreign].

See [`beacon.example.toml`](beacon.example.toml) [test: crates/beacon/tests/prose_claims.rs::claim_example_toml_knobs]. Key knobs: `on_stop`,
`on_notification`, `include_snippet`/`snippet_chars`, the channel fields, and
`log` [test: crates/beacon/tests/prose_claims.rs::claim_example_toml_knobs]. `BEACON_DISABLE=1` silences the hook (`beacon notify`); `beacon test` still sends [test: crates/beacon/tests/prose_claims.rs::claim_disable_env].

## Build the per-platform binaries

```sh
make bins     # refresh bin/beacon-darwin-<arch> and bin/beacon-linux-x86_64 [test: crates/beacon/tests/prose_claims.rs::claim_make_targets]
make mac      # just the native macOS binary [test: crates/beacon/tests/prose_claims.rs::claim_make_targets]
make linux    # just the Linux x86_64 cross-build (cargo-zigbuild) [test: crates/beacon/tests/prose_claims.rs::claim_make_targets]
cargo test
```

Only the `bin/beacon` launcher is committed; the per-platform `bin/beacon-<os>-<arch>`
builds are git-ignored [test: crates/beacon/tests/prose_claims.rs::claim_platform_bins_untracked].
