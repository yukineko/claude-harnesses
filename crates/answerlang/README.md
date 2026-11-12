# answerlang

Keeps answers and conclusions in the user's language — by mechanism, not by
asking nicely.

## Three layers

| Layer | What | Where | Switch |
|---|---|---|---|
| 1 | Claude Code's built-in `"language"` key in `~/.claude/settings.json`, e.g. `"language": "japanese"` | Claude Code itself — **not part of this crate** (set separately) | settings.json |
| 2 | **inject**: SessionStart / UserPromptSubmit add the target language to `additionalContext` | `answerlang session-start`, `answerlang prompt` | `inject` |
| 3 | **enforce**: the Stop hook blocks a final answer that is not in the target language's script | `answerlang stop` | `enforce` |

Everything is switchable; `enabled = false` silences all hooks (empty stdout, exit 0). Exception: when the hook's stdin itself cannot be read, that is handled as undetermined before the config is consulted (Stop blocks; the inject hooks print that injection did NOT run).

## Configuration — `~/.answerlang/config.toml`

```toml
enabled   = true      # master switch
inject    = true      # layer 2
enforce   = true      # layer 3
language  = "auto"    # or "japanese", "ja", "ja-JP", ...
min_share = 0.3       # clamped to [0.05, 0.95]
```

- `answerlang set [--language <name|tag|auto>] [--inject on|off] [--enforce on|off] [--enabled on|off]`
  creates or merges into the file (keys not given are kept; a hand-written
  `min_share` is preserved) and writes it atomically (temp file in the same
  directory + rename). No option → exit 2 with usage. An invalid `on|off` value
  → exit 2 and the file is not touched. An existing file that cannot be read or
  parsed is not overwritten (exit 1). The language is stored lowercased.
- `answerlang show` prints `config_path`, `config_present`, `enabled`, `inject`,
  `enforce`, `language`, `min_share` (effective, clamped), `effective_language`
  and `language_source` (`config` | `macos` | `env` | `none`; `show` has no
  prompt, so the prompt step is skipped). For an explicit language that is not
  in the table, `effective_language` is that name verbatim.
- An **unreadable or unparseable** config file: a message on stderr, all
  defaults are used (enabled), and it **counts as present** (enforcement
  applies; no first-run directive). A non-finite `min_share` falls back to 0.3.

## Languages

Canonical names (tags map by their primary subtag: `ja-JP` → japanese,
`en_US.UTF-8` → english): japanese (ja), korean (ko), chinese (zh), russian
(ru), ukrainian (uk), greek (el), arabic (ar), hebrew (he), thai (th), hindi
(hi), english (en), french (fr), german (de), spanish (es), portuguese (pt),
italian (it).

Target script: japanese = hiragana + katakana (incl. halfwidth) + CJK
ideographs; chinese = CJK ideographs; korean = hangul (+ CJK ideographs);
russian/ukrainian = cyrillic; greek; arabic; hebrew; thai; hindi = devanagari;
english/french/german/spanish/portuguese/italian = latin.

**Limitation:** only scripts are measured, so the latin-script languages cannot
be told apart (a French answer passes for `english`), and neither can russian
and ukrainian.

**Unknown explicit language** (e.g. `language = "klingon"`): layer 2 injects the
name verbatim; layer 3 has no script to measure, so Stop does not block and
emits a `systemMessage` saying enforcement is unavailable for that language.

## Detection order (`language = "auto"`, first that determines wins)

1. **The latest user prompt** (UserPromptSubmit). Any kana → japanese; else any
   hangul → korean; else the non-latin script with the most letters (Han →
   chinese, cyrillic → russian, greek, arabic, hebrew, thai, devanagari →
   hindi). Latin-only or no letters → does not determine. The result
   (including "none") is recorded in `~/.answerlang/state/<session>.prompt`;
   Stop uses that record and never re-derives it from the transcript (Stop-hook
   feedback appears in the transcript as user text).
2. **macOS UI language** (first `AppleLanguages` entry of
   `/usr/bin/defaults read -g AppleLanguages`; macOS only). Run at most once per
   session — the result, including "not determined", is cached in
   `state/<session>.macos` — and not at all when step 1 determined the
   language. Spawn failure, non-zero exit, a 3-second timeout or unparseable
   output = not determined. Without a usable session id the query runs
   uncached. `ANSWERLANG_DEFAULTS_CMD` overrides the program (test seam). This
   is the only subprocess on UserPromptSubmit.
3. **`LC_ALL`, then `LC_MESSAGES`, then `LANG`** — the first non-empty one is
   the effective locale; `C`, `POSIX`, `C.UTF-8` or a locale whose primary
   subtag is not in the table do not determine (and do not fall through to the
   next variable).

Nothing determined: layer 2 injects no language line (stderr note); layer 3
does not block and emits a `systemMessage` saying the target language is
undetermined.

The session id is the payload's `session_id`, else `CLAUDE_CODE_SESSION_ID`;
only `[A-Za-z0-9_-]` ids are used as state keys.

## First run (config absent) — ask once

While `~/.answerlang/config.toml` does not exist:

- SessionStart and UserPromptSubmit inject the directive
  `answerlang: no language configured. Ask the user once with AskUserQuestion which language answers should use; offer the auto-detected language as the recommended option, then run `answerlang set --language <choice>`.`
  plus the normal language line for the auto-detected language when one is
  determined.
- Stop **does not block**; it only emits a `systemMessage` noting that the
  language is unconfigured.

Once the file exists (`answerlang set` or by hand) the directive disappears and
enforcement applies.

## Layer 2 output

```json
{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"..."}}
```

The language line is exactly:
`Answers and conclusions: write them in <lang>. Keep code, identifiers, commit messages and verbatim quotes (gate messages, test output) in their original form.`

Nothing to inject (inject off, disabled, or undetermined language with the
config present) → empty stdout, exit 0. A stdin read error or an internal
error prints a `systemMessage` + `additionalContext` saying injection did NOT
run (never silence). An unparseable payload skips prompt detection (stderr
note) and injects what can still be determined.

## Layer 3 (Stop) — main session only

`hooks.json` registers `Stop`, not `SubagentStop`. Order:

1. `enabled = false` or `enforce = false` → empty stdout.
2. Config absent → `systemMessage` only.
3. Payload unparseable → block (see "bounded re-entry").
4. Explicit unknown language / target undetermined → `systemMessage`, no block.
5. Read the transcript. The checked text is the last entry with
   `"type":"assistant"` (skipping `"isSidechain": true`) whose
   `message.content` has at least one non-empty `text` block; all its text
   blocks are joined with `\n`. A line that cannot be parsed AFTER that entry
   makes the check undetermined (it could be a later answer).

### What is stripped before counting

- fenced code blocks (``` or ~~~, the whole block; an **unclosed** fence is not
  treated as a fence, so prose after it still counts),
- inline code spans (`` `...` ``; an unmatched backtick stays literal),
- blockquote lines (leading `>`),
- quote lines: trimmed line starting with `$ `, `test result:`, `error:`,
  `error[`, `warning:`, or wholly wrapped in `"..."` / `「...」`,
- URLs (`http://` / `https://` up to whitespace),
- file paths: whitespace-delimited tokens containing `/` or starting with `~`,
- identifier tokens (delimited by whitespace and punctuation other than
  `. : - _`): hex ids (7–40 chars of `[0-9a-f]` with at least one digit),
  run ids (`run-` + alnum/dash containing at least one digit — so a word like
  `run-time` still counts), versions (`v?\d+(\.\d+)+`), and `name:line`
  tokens (anything ending in `:<digits>`, which includes `:<digits>:<digits>`).

Markdown bullets and heading markers are punctuation and never count.

### Measurement

Letters = `char::is_alphabetic`. **Fewer than 40 letters left → clean by
rule.** share = target-script letters / letters; `share < min_share` (or an
incomparable value) → violation.

- Clean → empty stdout, exit 0; clears this session's block marker.
- Violation, first time → `{"decision":"block","reason":"..."}` naming the
  target language and asking for the answer/conclusion to be restated in it.

### Bounded re-entry

A per-session marker `~/.answerlang/state/<session>.block` records "the
previous stop of this session was blocked by answerlang".

- First stop of a chain (`stop_hook_active: false`) with a violation or an
  undetermined check (no `transcript_path`, unreadable transcript, no assistant
  text, unreadable prompt-language record) → record the marker and block. The
  reason names the failure (and contains `transcript` for transcript failures).
- Re-entry (`stop_hook_active: true`) and the marker is present → clear it and
  **allow** with a `systemMessage` containing `answerlang` and `failed twice`.
- Re-entry without the marker (another gate's block set `stop_hook_active`) →
  record it and block.
- Re-entry where the marker cannot be read, cannot be recorded, or there is no
  usable session id → allow with an explicit `systemMessage` (a block could
  never be bounded).
- Unparseable payload: `stop_hook_active` is unknown, so it always blocks (and
  records the marker so a following parseable re-entry can take the bounded
  allow). Restrictive, and unbounded only while every payload stays
  unparseable.
- Panics go through `harness_core::gate::run::run_guarded` (fail closed); a
  stdin read error goes through `read_stop_payload_guarded`.

## Launcher without a bundled binary

`bin/answerlang` execs `answerlang-<os>-<arch>`. When it is missing:

- `stop` → the check did not run, i.e. cannot determine → **block** on the
  first stop (`{"decision":"block"}`), and on a re-entry
  (`"stop_hook_active": true` in the payload) allow with a `systemMessage`
  saying the check did NOT run. The launcher has no per-gate marker, so here
  `stop_hook_active` alone bounds it.
- `session-start` / `prompt` → exit 0 with a `systemMessage` +
  `additionalContext` saying language injection did NOT run.
- anything else → exit 1.

## Enabling

1. `scripts/rollout-plugins.sh --plugin answerlang`
2. Add `"answerlang@yukineko": true` to `enabledPlugins` in
   `~/.claude/settings.json` (the user does this).
3. Optionally set layer 1: `"language": "japanese"` in the same settings file.
