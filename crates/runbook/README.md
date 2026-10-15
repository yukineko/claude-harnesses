Reusable **procedure includes** [test: crates/runbook/tests/integration.rs::help_describes_procedure_includes]. A `UserPromptSubmit` hook
expands `!name` macros you type into the matching repo-committed procedure
(`.runbook/<name>.md`) [test: crates/runbook/tests/prose_claims.rs::claim_inject_expands_macro] [test: crates/runbook/tests/prose_claims.rs::claim_hooks_json_wires_userpromptsubmit].

```
> follow !release-plugin and cut a new version
```
→ the hook prints the `release-plugin` procedure (Overview / Procedure /
Specifications / Forbidden Actions) on stdout [test: crates/runbook/tests/prose_claims.rs::claim_inject_expands_macro].

The hook injects only the procedures a prompt asked for [test: crates/runbook/tests/prose_claims.rs::claim_inject_only_requested_under_budget]. Once the next procedure would exceed `max_chars`, it and the rest are omitted with a `残り N 件` note; the char budget is not hard: the first procedure is always included (up to `per_runbook_chars` plus heading and ellipsis), so the output can exceed `max_chars` [test: crates/runbook/tests/prose_claims.rs::claim_inject_only_requested_under_budget].

If an expected procedure did not arrive, check `runbook list` / `runbook status`, which report an unreadable store
(`unknown — …`; `list`/`show` exit 1) [test: crates/runbook/tests/prose_claims.rs::claim_unreadable_store_reported]. A real injection is also recorded as a line in the shared harness inject-metrics ledger
(`~/.harness/state/inject-metrics.jsonl`) [test: crates/runbook/tests/prose_claims.rs::claim_inject_metrics_ledger].

`!name` can be dropped mid-sentence and stacked (`!build !test`) [test: crates/runbook/src/inject.rs::scans_boundaried_tokens_only] [test: crates/runbook/src/inject.rs::expand_matches_and_dedupes].
A macro only fires when it resolves to an existing
runbook, so stray `!` in prose or code (`x != y`, `!!`, `foo!`) never injects
anything [test: crates/runbook/src/inject.rs::ignores_non_boundary_and_operators] [test: crates/runbook/src/inject.rs::unknown_token_injects_nothing].

One markdown file per procedure under `.runbook/` (project, committed) or
`~/.runbook/runbooks/` (global) [test: crates/runbook/tests/prose_claims.rs::claim_store_dirs_project_and_global].
The file stem is the macro name
(`deploy.md` → `!deploy`) [test: crates/runbook/tests/prose_claims.rs::claim_file_stem_is_macro].
Optional TOML frontmatter adds a description and
aliases: [test: crates/runbook/src/store.rs::parse_frontmatter] [test: crates/runbook/src/store.rs::parse_body_only]

```markdown
+++
description = "本番デプロイ手順"
aliases = ["ship"]
+++

# deploy
## Overview …
## Procedure …
## Forbidden Actions …
```

Type `!runbooks` in a prompt to inject the list of what's available [test: crates/runbook/src/inject.rs::index_token_requests_index].

`hooks/hooks.json` wires the UserPromptSubmit
hook (`bin/runbook inject`) [test: crates/runbook/tests/prose_claims.rs::claim_hooks_json_wires_userpromptsubmit].
Add procedures under `.runbook/` in your repo and invoke
them with `!name` [test: crates/runbook/tests/prose_claims.rs::claim_inject_expands_macro].

```sh
cargo install --path .
runbook init                 # create .runbook/ + a sample procedure [test: crates/runbook/tests/prose_claims.rs::claim_init_creates_sample]
runbook new deploy --description "本番デプロイ手順"
runbook list                 # show available macros [test: crates/runbook/tests/prose_claims.rs::claim_list_shows_macros]
runbook show deploy          # print one procedure [test: crates/runbook/tests/prose_claims.rs::claim_show_prints_one]
runbook install              # merge the UserPromptSubmit hook into ~/.claude/settings.json [test: crates/runbook/tests/prose_claims.rs::claim_install_merges_hook] [test: crates/runbook/tests/prose_claims.rs::claim_install_dryrun_idempotent_backup_foreign]
runbook status               # resolved config + dirs + count [test: crates/runbook/tests/prose_claims.rs::claim_status_shows_config_dirs_count]
runbook uninstall
```

`runbook new` also takes `--global` (write into the global dir instead of the
project) and `--force` (overwrite an existing file); the name is normalized to
lowercase `[a-z0-9_-]` (max 48 chars) [test: crates/runbook/tests/prose_claims.rs::claim_new_global_force_normalize]. `runbook install`/`uninstall` take
`--dry-run` and preserve foreign hook groups; installing twice leaves one runbook hook
group, and install backs up `settings.json` [test: crates/runbook/tests/prose_claims.rs::claim_install_dryrun_idempotent_backup_foreign].

Project procedures shadow global ones with the same name [test: crates/runbook/tests/prose_claims.rs::claim_project_shadows_global]. Macros typed in the prompt (names and aliases)
match case-insensitively [test: crates/runbook/tests/prose_claims.rs::claim_case_insensitive_names_aliases]. If a real runbook is named after `index_token`, `!runbooks`
injects that runbook instead of the index [test: crates/runbook/tests/prose_claims.rs::claim_real_index_token_runbook_wins].

See [`runbook.example.toml`](runbook.example.toml): `enabled`, `project_dir`,
`global_dir`, `include_global`, `prefix`, `index_token`, `max_chars`,
`per_runbook_chars` [test: crates/runbook/tests/prose_claims.rs::claim_example_toml_keys]. `RUNBOOK_DISABLE` set to any non-empty value other than `0`
turns the hook off [test: crates/runbook/tests/prose_claims.rs::claim_runbook_disable_env].

The config is read from `./runbook.toml` (project) **or** `~/.runbook/config.toml`
(global) — the first file that exists wins and the two are **not** merged [test: crates/runbook/tests/prose_claims.rs::claim_config_first_file_wins].
`max_chars` is floored at 200 and `per_runbook_chars` is clamped to
`[200, max_chars]` [test: crates/runbook/tests/prose_claims.rs::claim_budget_floor_clamp].

```sh
cargo build --release -p runbook
cargo test -p runbook
```

The platform-tagged `bin/runbook-<os>-<arch>` binaries are **not committed**
(they are git-ignored); only the `bin/runbook` launcher
is tracked [test: crates/runbook/tests/prose_claims.rs::claim_platform_bins_not_committed]. The
crate's `Makefile` (`make bins`) copies from a crate-local `target/release/`, which a
workspace build does not produce, so it is not a working build path here [test: crates/runbook/tests/prose_claims.rs::claim_makefile_crate_local_target].
