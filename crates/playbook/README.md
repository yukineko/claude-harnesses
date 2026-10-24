`playbook inject` is wired to the **UserPromptSubmit** hook.

It does not suggest `/flow`: a keyword classifier that did was removed because it misfired on status questions and sub-agent reports.

1. loads the notes visible from the cwd — the project store
   (`<store>/<basename>-<hash>/`) plus the shared `_global/` store;
2. **scores** each note against the prompt: `triggers` (×5) > `tags` (×3) >
   title words (×2) > body overlap (capped at 4). CJK is
   tokenized per-char, so a Japanese title matches Japanese prompts; a
   multi-character Japanese trigger scores 0 (only a single-character one
   matches).
3. selects `always` notes first, then the top scorers at or above `min_score`, up
   to `top_k`, dropping a scored note that would overflow the `max_chars` budget;
4. prints them on stdout. With no `always` note, nothing relevant ⇒ nothing injected.

```sh
cargo install --path .
playbook install                 # wire the UserPromptSubmit hook (backs up settings.json)

playbook add --title "メモリ: 一括読込み禁止" \
  --trigger "pandas,read_csv,lightgbm,memory" --tags "data" \
  --body "read_csv 等で全件ロード禁止。chunksize / ParquetFile.read_row_group で部分読み。"

playbook add --title "commit は branch を切ってから" --always \
  --body "main で直接コミットしない。必ず作業ブランチを切る。"
# --global stores the note in the shared _global store; without --body the body is read from stdin

playbook list
playbook search lightgbm が OOM で落ちる   # scores per note; ✓ marks notes at or above min_score and always notes
playbook rm <slug>
playbook status                  # resolved config + store paths + visible note count
playbook init                    # create the store dirs and a sample note
playbook uninstall               # remove the hook (install/uninstall accept --dry-run)
```

A note is a markdown file with TOML frontmatter:

```
+++
title = "メモリ: 一括読込み禁止"
tags = ["data"]
triggers = ["pandas", "read_csv", "lightgbm", "memory"]
always = false
+++

read_csv 等で全件ロード禁止。chunksize / ParquetFile.read_row_group で部分読み。
```

See [`playbook.example.toml`](playbook.example.toml). Project `./playbook.toml`
over `~/.playbook/config.toml` over built-in defaults (the first file that exists
wins; no merging).

| key | meaning | default |
|---|---|---|
| `store_dir` | note store root | `~/.playbook/store` |
| `top_k` | max scored notes injected per prompt (`always` notes do not count) | 3 |
| `min_score` | relevance threshold (`always` bypasses) | 5 |
| `max_chars` | budget for scored notes: `always` notes are injected in full past it; a scored note that would overflow it is dropped, unless it is the first note admitted | 1500 |
| `include_global` | also search the `_global` store | true |

Kill switch: `PLAYBOOK_DISABLE` set to any non-empty value other than `0`.
