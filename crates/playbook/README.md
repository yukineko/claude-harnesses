`playbook inject` is wired to the **UserPromptSubmit** hook [test: crates/playbook/tests/prose_claims.rs::claim_hooks_json_wiring].

A keyword classifier flags a
prompt that contains an explicit flow request (such as "condukt で回して"), or at
least two of production impact, Rust changes and untestable constraints, and prints a `→ /flow <problem>` suggestion [test: crates/playbook/tests/prose_claims.rs::claim_complex_prompt_flow_line].
Knowledge injection still runs alongside it [test: crates/playbook/tests/prose_claims.rs::claim_flow_and_notes_together].

1. loads the notes visible from the cwd — the project store
   (`<store>/<basename>-<hash>/`) plus the shared `_global/` store; [test: crates/playbook/tests/prose_claims.rs::claim_visible_stores]
2. **scores** each note against the prompt: `triggers` (×5) > `tags` (×3) >
   title words (×2) > body overlap (capped at 4) [test: crates/playbook/tests/prose_claims.rs::claim_score_weights]. CJK is
   tokenized per-char, so a Japanese title matches Japanese prompts; a
   multi-character Japanese trigger scores 0 (only a single-character one
   matches) [test: crates/playbook/tests/prose_claims.rs::claim_score_weights] [test: crates/playbook/tests/prose_claims.rs::claim_score_weights_japanese_trigger_observed].
3. selects `always` notes first, then the top scorers at or above `min_score`, up
   to `top_k`, dropping a scored note that would overflow the `max_chars` budget; [test: crates/playbook/tests/prose_claims.rs::claim_selection_order_budget]
4. prints them on stdout [test: crates/playbook/tests/prose_claims.rs::claim_nothing_relevant_nothing_injected]. With no `always` note, nothing relevant ⇒ nothing injected [test: crates/playbook/tests/prose_claims.rs::claim_nothing_relevant_nothing_injected].

```sh
cargo install --path .
playbook install                 # wire the UserPromptSubmit hook (backs up settings.json) [test: crates/playbook/tests/prose_claims.rs::claim_install_backs_up]

playbook add --title "メモリ: 一括読込み禁止" \
  --trigger "pandas,read_csv,lightgbm,memory" --tags "data" \
  --body "read_csv 等で全件ロード禁止。chunksize / ParquetFile.read_row_group で部分読み。"

playbook add --title "commit は branch を切ってから" --always \
  --body "main で直接コミットしない。必ず作業ブランチを切る。"
# --global stores the note in the shared _global store; without --body the body is read from stdin [test: crates/playbook/tests/prose_claims.rs::claim_add_global_and_stdin]

playbook list
playbook search lightgbm が OOM で落ちる   # scores per note; ✓ marks notes at or above min_score and always notes [test: crates/playbook/tests/prose_claims.rs::claim_search_check_mark]
playbook rm <slug>
playbook status                  # resolved config + store paths + visible note count [test: crates/playbook/tests/prose_claims.rs::claim_status_output]
playbook init                    # create the store dirs and a sample note [test: crates/playbook/tests/prose_claims.rs::claim_init_creates]
playbook uninstall               # remove the hook (install/uninstall accept --dry-run) [test: crates/playbook/tests/prose_claims.rs::claim_uninstall_and_dry_run]
```

A note is a markdown file with TOML frontmatter: [test: crates/playbook/tests/prose_claims.rs::claim_note_format]

```
+++
title = "メモリ: 一括読込み禁止"
tags = ["data"]
triggers = ["pandas", "read_csv", "lightgbm", "memory"]
always = false
+++

read_csv 等で全件ロード禁止。chunksize / ParquetFile.read_row_group で部分読み。
```

See [`playbook.example.toml`](playbook.example.toml) [test: crates/playbook/tests/prose_claims.rs::claim_example_toml]. Project `./playbook.toml`
over `~/.playbook/config.toml` over built-in defaults (the first file that exists
wins; no merging) [test: crates/playbook/tests/prose_claims.rs::claim_config_precedence].

| key | meaning | default [test: crates/playbook/tests/prose_claims.rs::claim_config_defaults] |
|---|---|---|
| `store_dir` | note store root | `~/.playbook/store` [test: crates/playbook/tests/prose_claims.rs::claim_config_defaults] |
| `top_k` | max scored notes injected per prompt (`always` notes do not count) | 3 [test: crates/playbook/tests/prose_claims.rs::claim_config_defaults] [test: crates/playbook/tests/prose_claims.rs::claim_top_k_excludes_always] |
| `min_score` | relevance threshold (`always` bypasses) | 5 [test: crates/playbook/tests/prose_claims.rs::claim_config_defaults] |
| `max_chars` | budget for scored notes: `always` notes are injected in full past it; a scored note that would overflow it is dropped, unless it is the first note admitted | 1500 [test: crates/playbook/tests/prose_claims.rs::claim_config_defaults] [test: crates/playbook/tests/prose_claims.rs::claim_selection_order_budget_max_chars_not_hard_cap_observed] |
| `include_global` | also search the `_global` store | true [test: crates/playbook/tests/prose_claims.rs::claim_config_defaults] |

Kill switch: `PLAYBOOK_DISABLE` set to any non-empty value other than `0` [test: crates/playbook/tests/prose_claims.rs::claim_kill_switch].
