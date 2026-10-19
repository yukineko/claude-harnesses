[record: 2026-06-22 rev: 49a1cc25] Inspired by Devin's Session Insights, rebuilt as a local, no-API-key hook.

- **PostToolUse** records each tool call (per-tool counts, distinct files touched) — subcommand `record` [test: crates/session-insights/tests/prose_claims.rs::claim_posttooluse_records_tool_and_files].
- **Stop** counts a turn; if `obsidian_log = true` also (re)writes the dated `sessions/` note, and if `record = true` refreshes the `/record` note — subcommand `stop` [test: crates/session-insights/tests/prose_claims.rs::claim_stop_counts_turn] [test: crates/session-insights/tests/prose_claims.rs::claim_stop_writes_session_note_when_obsidian_log] [test: crates/session-insights/tests/prose_claims.rs::claim_stop_refreshes_record_when_record_true].
- **SessionEnd** is opt-in (`record = true`) and writes/updates the `/record` note — subcommand `sessionend` [test: crates/session-insights/tests/prose_claims.rs::claim_sessionend_writes_record_only_when_record_true].
- **size**: XS / S / M / L / XL by total tool events (thresholds configurable) [test: crates/session-insights/tests/prose_claims.rs::claim_size_thresholds_default_boundaries] [test: crates/session-insights/tests/prose_claims.rs::claim_size_thresholds_configurable]
- **category**: `coding` (Edit/Write/MultiEdit/NotebookEdit), `ops` (Bash), `research`
  (Read/Grep/Glob/WebFetch/WebSearch) when that group is at least half of the categorized
  events; otherwise `mixed` (`empty` when there are none) [test: crates/session-insights/tests/prose_claims.rs::claim_category_rule]
- per-session turns, tool events, file count, and top tools [test: crates/session-insights/tests/prose_claims.rs::claim_report_shows_counts]

```sh
session-insights report          # latest session [test: crates/session-insights/tests/prose_claims.rs::claim_report_latest_default]
session-insights report --session <id-prefix>
session-insights report --all    # one line per recorded session [test: crates/session-insights/tests/prose_claims.rs::claim_report_all_one_line_each]
session-insights report --context # also join context-governor ledger health [test: crates/session-insights/tests/integration.rs::report_context_joins_synthetic_ledger]
```

If the session store cannot be read, `report` prints `unknown — session store could
not be read: …` to stderr and exits 1 (it does not print "no sessions") [test: crates/session-insights/tests/prose_claims.rs::claim_report_unreadable_store_exits_1].

```
session a1b2c3d4  [2026-06-20T18:00:00+09:00]
  project: playbook
  size: L   category: coding
  turns: 12   tool events: 47   files: 9
  top tools: Edit 18, Bash 12, Read 9, Write 5, Grep 3
```

Set `record = true` (and point `obsidian_vault` at your vault) [test: crates/session-insights/tests/prose_claims.rs::claim_stop_refreshes_record_when_record_true]. On every Stop and
on SessionEnd — and on demand via `record-now`, which writes even when
`record = false` — session-insights writes a
Markdown note to `<vault>/<record_dir>/<date>-<project>-<id>.md`, only if the
vault directory already exists [test: crates/session-insights/tests/prose_claims.rs::claim_record_note_path_and_vault_required] [test: crates/session-insights/tests/prose_claims.rs::claim_stop_refreshes_record_when_record_true] [test: crates/session-insights/tests/prose_claims.rs::claim_sessionend_writes_record_only_when_record_true]. The deterministic `## 数値サマリ` and `## コスト`
blocks (fenced by machine-owned `<!-- si:numeric:* -->` / `<!-- si:cost:* -->`
markers) are filled by the binary; the prose sections stay as `<!-- fill: … -->`
placeholders [test: crates/session-insights/src/record.rs::skeleton_has_all_sections_and_placeholders] [test: crates/session-insights/tests/prose_claims.rs::claim_skeleton_prose_sections_placeholders].

```sh
session-insights record-now              # (re)generate the note, print its path [test: crates/session-insights/tests/prose_claims.rs::claim_record_now_prints_path]
session-insights record-now --session <id>
```

Re-runs merge in place:
only the machine-owned blocks are refreshed, your prose is preserved. [test: crates/session-insights/src/record.rs::merge_replaces_auto_blocks_and_preserves_prose]

The cross-session queue is the **standalone [`backlog`](../backlog)
crate** (per repo: `<repo root>/.backlog/tasks.toml`) [test: crates/backlog/src/config.rs::project_in_a_repo_resolves_to_the_repo_local_store], whose `backlog session-start`
command injects the pending tasks [test: crates/backlog/tests/backlog_016c2562_mirrors.rs::session_start_pending_listing_omits_wontfix_but_lists_normal_task]. session-insights no
longer keeps an independent backlog store — its old `session-insights backlog`
subcommands and the `<vault>/backlog.md` / `backlog.json` store have been
removed [test: crates/session-insights/tests/prose_claims.rs::claim_no_backlog_subcommand].

```sh
backlog add --title "rebuild darwin-x86_64 on an x86 Mac" --project harness --repro-test "<cmd>"
backlog list --project harness --status pending
backlog done <id> --test "cargo test -p x" --red-rev <rev>   # close with evidence [test: crates/backlog/tests/closure_evidence.rs::allowlisted_bash_script_with_red_rev_closes_and_records_closure]
```

Store writes (`add` / `done` / …) are refused in the primary working tree [test: crates/backlog/tests/store_worktree_boundary.rs::add_from_primary_checkout_is_refused_and_writes_nothing] [test: crates/backlog/tests/store_worktree_boundary.rs::done_from_primary_checkout_is_refused_and_file_unchanged] — run them
from a linked worktree. A bare `backlog done <id>` is refused [test: crates/backlog/tests/closure_evidence.rs::bare_done_is_refused_and_status_unchanged].
Closing needs evidence: `--test … --red-rev …`, `--doc-only <commit>` or `--duplicate-of <id>` [test: crates/backlog/tests/closure_evidence.rs::allowlisted_bash_script_with_red_rev_closes_and_records_closure] [test: crates/backlog/tests/doc_only_closure.rs::plain_docs_paths_are_accepted] [test: crates/backlog/tests/closure_evidence.rs::duplicate_of_pending_target_is_accepted].
An `add` without a reproducing `--repro-test` is refused and writes nothing to the store [test: crates/backlog/tests/no_confirm_no_unproven.rs::add_without_repro_writes_nothing_anywhere] [test: crates/backlog/tests/no_confirm_no_unproven.rs::add_with_non_reproducing_repro_writes_nothing_anywhere].
See the backlog README for the exact rules.

## Enable Obsidian logging (opt-in)

Set `obsidian_log = true` and point `obsidian_vault` at your vault [test: crates/session-insights/tests/prose_claims.rs::claim_stop_writes_session_note_when_obsidian_log]. On every Stop
the session is written/overwritten to `<vault>/sessions/<date>-<id>.md` with
frontmatter (`type: session`, size, category, turns…) — only if the vault dir
already exists (it never creates the vault) [test: crates/session-insights/tests/prose_claims.rs::claim_stop_writes_session_note_when_obsidian_log] [test: crates/session-insights/tests/prose_claims.rs::claim_stop_session_note_never_creates_vault_and_needs_opt_in].

## Install (plugin)

The bundled `hooks/hooks.json` wires all three hooks automatically (PostToolUse
matcher: `Bash|Edit|MultiEdit|Write|Read|Grep|Glob|WebFetch|WebSearch|NotebookEdit`) [test: crates/session-insights/tests/prose_claims.rs::claim_hooks_json_three_hooks]. Drop a
`session-insights.toml` to tune thresholds or enable Obsidian logging [test: crates/session-insights/tests/prose_claims.rs::claim_project_toml_tunes].

```sh
cargo install --path .
session-insights install      # merge the PostToolUse + Stop + SessionEnd hooks [test: crates/session-insights/tests/prose_claims.rs::claim_install_merges_three_hooks]
session-insights report --all
session-insights status        # resolved config [test: crates/session-insights/tests/prose_claims.rs::claim_status_resolved_config]
session-insights init          # create the state dir [test: crates/session-insights/tests/prose_claims.rs::claim_init_creates_state_dir]
session-insights uninstall
```

`install`/`uninstall` are idempotent, back up `settings.json`, and preserve
foreign hook groups [test: crates/session-insights/tests/prose_claims.rs::claim_install_idempotent_backup_foreign].

Drop `./session-insights.toml` (project) or `~/.session-insights/config.toml`
(global); the first that exists wins [test: crates/session-insights/tests/prose_claims.rs::claim_config_lookup_order].

- `enabled` — master switch for the hooks; default `true` [test: crates/session-insights/tests/prose_claims.rs::claim_config_defaults_values] [test: crates/session-insights/tests/prose_claims.rs::claim_config_defaults_enabled_false_is_noop]
- `size_thresholds` — `[S, M, L, XL]` lower bounds by tool events; default `[5, 15, 40, 100]` [test: crates/session-insights/tests/prose_claims.rs::claim_size_thresholds_default_boundaries] [test: crates/session-insights/tests/prose_claims.rs::claim_size_thresholds_configurable]
- `ignore_tools` — tools excluded from metrics; default `["TodoWrite"]` [test: crates/session-insights/tests/prose_claims.rs::claim_config_defaults_values] [test: crates/session-insights/tests/prose_claims.rs::claim_config_defaults_todowrite_ignored]
- `obsidian_log` — write the dated `sessions/` note on every Stop; default `false` [test: crates/session-insights/tests/prose_claims.rs::claim_config_defaults_values] [test: crates/session-insights/tests/prose_claims.rs::claim_stop_session_note_never_creates_vault_and_needs_opt_in]
- `obsidian_vault` — vault root (notes go under `sessions/` and `records/`); default `~/Documents/vault/yukineko` [test: crates/session-insights/tests/prose_claims.rs::claim_config_defaults_values]
- `record` — write/update the `/record` note on Stop and SessionEnd; default `false` [test: crates/session-insights/tests/prose_claims.rs::claim_config_defaults_values] [test: crates/session-insights/tests/prose_claims.rs::claim_sessionend_writes_record_only_when_record_true]
- `record_dir` — vault subdir for record notes; default `records` [test: crates/session-insights/tests/prose_claims.rs::claim_config_defaults_values] [test: crates/session-insights/tests/prose_claims.rs::claim_record_note_path_and_vault_required]
- `state_dir` — rollup state directory; default `~/.session-insights/state` [test: crates/session-insights/tests/prose_claims.rs::claim_config_defaults_values] [test: crates/session-insights/tests/prose_claims.rs::claim_init_creates_state_dir]
- `price_overrides` — per-model cost overrides for the `## コスト` block; default (built-in rates) [test: crates/session-insights/tests/prose_claims.rs::claim_config_defaults_price_overrides_change_cost]

Kill switch: `SESSION_INSIGHTS_DISABLE` set to any non-empty value other than `0` [test: crates/session-insights/tests/prose_claims.rs::claim_kill_switch_env].

```sh
make bins
cargo test
```

Only the `bin/session-insights` launcher is committed; the per-platform
`bin/session-insights-<os>-<arch>` builds are git-ignored [test: crates/session-insights/tests/prose_claims.rs::claim_bins_gitignored]. When no build exists
for the host, the launcher does not pretend to have run: `record` writes a stderr
line, `stop` emits a `systemMessage`, `sessionend` exits 1, and any CLI verb exits
127 [test: crates/session-insights/tests/prose_claims.rs::claim_launcher_missing_binary].
