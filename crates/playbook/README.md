# playbook

> **Project knowledge retrieval + injection for Claude Code**, written in Rust.

You tell the agent the same things over and over — "use chunked reads here",
"branch before committing", "the staging URL is X". `playbook` is Devin's
**Knowledge** feature as a local hook: curate those facts once as atomic notes,
and a **UserPromptSubmit** hook injects the ones relevant to each prompt — under
a strict character budget, so conventions resurface without bloating context.

Deterministic retrieval (keyword + trigger scoring, no embeddings, no API key).

## How it works

`playbook inject` is wired to the **UserPromptSubmit** hook. On each prompt it
does two things:

**Complexity detection.** A deterministic keyword classifier (no LLM) flags
prompts that look like complex work — production impact, cross-crate Rust
changes, untestable constraints, an explicit "condukt で回して" request — and
prints a `→ /flow <problem>` suggestion. Knowledge injection still runs
alongside it.

**Knowledge injection.**

1. loads the notes visible from the cwd — the project store
   (`<store>/<basename>-<hash>/`) plus the shared `_global/` store;
2. **scores** each note against the prompt: `triggers` (×5) > `tags` (×3) >
   title words (×2) > body overlap (capped). CJK is tokenized per-char so
   Japanese prompts match;
3. selects `always` notes first, then the top scorers above `min_score`, up to
   `top_k`, stopping at the `max_chars` budget;
4. prints them as added context. Nothing relevant ⇒ nothing injected.

## Curate

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
playbook search lightgbm が OOM で落ちる   # see what would be injected (✓ marks it)
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

## Config

See [`playbook.example.toml`](playbook.example.toml). Project `./playbook.toml`
over `~/.playbook/config.toml` over built-in defaults (the first file that exists
wins; no merging).

| key | meaning | default |
|---|---|---|
| `store_dir` | note store root | `~/.playbook/store` |
| `top_k` | max notes injected per prompt | 3 |
| `min_score` | relevance threshold (`always` bypasses) | 5 |
| `max_chars` | hard cap on injected characters | 1500 |
| `include_global` | also search the `_global` store | true |

Kill switch: `PLAYBOOK_DISABLE` set to any non-empty value other than `0`.

## The harness family

- `ctxrot` — keeps context healthy over long sessions.
- `donegate` — won't let the agent declare done until checks pass.
- `stuckguard` — won't let the agent grind in a loop forever.
- **`playbook`** — feeds the agent what your project already taught you.

## License

MIT
