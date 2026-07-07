# hypothesis

> **PDO hypothesis lifecycle management for Claude Code**, written in Rust.
> The **discovery ledger**: it tracks the falsifiable bets you're testing toward a
> [compass](../compass) goal, and enforces that **shipping is not validating**
> (build ≠ validate).

A goal ([compass](../compass)) tells you *what you're aiming at*; a backlog
([backlog](../backlog)) tells you *what's queued*. Neither captures the **open
question** — "we *believe* X; is it true?" `hypothesis` is that missing ledger: each
record is a falsifiable statement with a lifecycle, so discovery work can't quietly
collapse into "we built it, therefore it's true."

```
discovery(hypothesis) ─▶ flow ─▶ condukt ─▶ merge ─▶ awaiting-measurement ─▶ measure ─▶ validate / reject
        ▲                                                                                      │
        └──────────────────────── a rejected bet feeds the next question ──────────────────────┘
```

It is **subscription-native**: no API key. The judging (is this bet validated? what's
the evidence?) is LLM/human labor in your Claude Code session; the binary only keeps
the ledger and renders deterministic SessionStart context. It never calls an LLM.

## Where it sits in the harness

| Question | Owner |
|---|---|
| What is this for · what's the next move? | `compass` |
| What's the confirmed queue? | `backlog` |
| **What do we believe but haven't proven — and what's its measured verdict?** | **`hypothesis`** |
| Decompose / schedule / run a task | `condukt` |
| Bind source → executor in a loop | `flow` |

`hypothesis` is a **source** for [`/flow`](../flow): open hypotheses become experiments
to build, and `awaiting-measurement` ones become the **measure step** that closes the
loop. A hypothesis can be linked to a compass goal (`--goal <keyword>`) so the
SessionStart hook can flag bets that have drifted away from the charter.

## The lifecycle

The living artifact is `~/.hypothesis/hypotheses.toml`. Each hypothesis moves through
four states — and the two transitions out of "shipped" are deliberately separate so a
merge can't masquerade as a validated learning:

| Status | Meaning | Enters via |
|---|---|---|
| `open` | a falsifiable bet, not yet tested | `hypothesis add` |
| `awaiting-measurement` | the experiment **shipped** but the signal isn't measured yet | `hypothesis await-measurement` (condukt sets this on merge) |
| `validated` | measured evidence **supports** the bet | `hypothesis validate --evidence …` |
| `rejected` | measured evidence **refutes** the bet | `hypothesis reject --reason …` |

`validate` **requires** `--evidence` and `reject` takes a `--reason`: you cannot move a
hypothesis to a terminal state without recording what you measured. Shipping alone only
reaches `awaiting-measurement` — **build ≠ validate**.

Each record holds `id` / `text` / `status` / `evidence[]` / `linked_goal` /
`condukt_run` / `success_criterion` / `kill_criterion` / `assumptions[]` /
`confidence` / `created_at` / `updated_at`.

## Subcommand surface

The binary is thin and deterministic:

| Subcommand | Purpose |
|---|---|
| `hypothesis add <text> [--goal <keyword>] [--success "<metric> >= <n>"] [--kill "<metric> <= <n>"] [--confidence <0..1>]` | add a bet; prints the new id. `--goal` links it to a compass charter goal. `--success`/`--kill` pre-register a falsifiable bar *before* shipping (operators: `>= <= > < ==`). `--confidence` sets the discovery confidence (default 0.5) |
| `hypothesis list [--status <s>]` | list bets in **discovery order** (confidence descending, then created_at), optionally filtered (`open` / `awaiting-measurement` / `validated` / `rejected`); pre-registered criteria are shown inline |
| `hypothesis confidence <id> <value>` | set a bet's discovery confidence (verification priority; higher = surfaces for validation sooner). Reorders the `list` discovery queue |
| `hypothesis await-measurement <id> [--run <run>]` | mark a shipped-but-unmeasured bet (condukt calls this on merge) |
| `hypothesis validate <id> --evidence <text>… [--measurement "<metric>=<value>"…] [--run <run>]` | terminal: measured evidence supports the bet (evidence required). If a `--success` criterion was pre-registered, a matching `--measurement` must clear it — otherwise validation is refused (no post-hoc goalpost-shifting) |
| `hypothesis reject <id> [--reason <text>] [--run <run>]` | terminal: measured evidence refutes the bet |
| `hypothesis assume <id> --text <t> --risk <low\|medium\|high> --evidence <strong\|weak\|none>` | record a belief the bet rests on (for RAT de-risking) |
| `hypothesis rat <id>` | print the riskiest untested assumption (the leap of faith to de-risk first via a minimal experiment); prints nothing when the bet is already de-risked. flow runs this before a full build |
| `hypothesis tested <id> <index>` | mark the assumption at `<index>` as tested (e.g. after a RAT), so it no longer registers as a leap of faith |
| `hypothesis install [--dry-run]` / `hypothesis uninstall` | add/remove the SessionStart hook in settings |
| `hypothesis session-start` | SessionStart hook entry point (internal) |

## The hook

Deterministic, non-blocking, exits 0 on any error (a discovery hook must never break a turn):

| Hook | Event | What it does |
|---|---|---|
| **`hypothesis session-start`** | `SessionStart` (startup/resume/clear) | injects the project's **open** and **awaiting-measurement** hypotheses as context, with a `validate`/`reject` reminder. Bets whose `linked_goal` no longer matches the compass charter are flagged `[unlinked]` so discovery doesn't silently drift from the goal. Prints nothing when there are no open or awaiting bets. |

## Store & configuration

The ledger lives at `~/.hypothesis/hypotheses.toml`. Optional `~/.hypothesis/config.toml`
(a missing file/key falls back to the defaults; a parse error silently yields defaults so
a discovery tool never crashes a turn):

```toml
enabled      = true     # set false to disable the SessionStart injection
store_dir    = "~/.hypothesis"   # where hypotheses.toml lives
inject_limit = 2000     # max chars of context the SessionStart hook injects
```

Set `HYPOTHESIS_DISABLE=1` in the environment to silence the hook without editing config.

## Install

### As a Claude Code plugin (recommended)

The plugin bundles the SessionStart hook (`hooks/hooks.json`), the `/hypothesis` and
`/add` skills, and a prebuilt binary — so it runs entirely on your Claude
**subscription**, no API key.

```text
# in Claude Code:
/plugin marketplace add yukineko/claude-harnesses
/plugin install hypothesis@yukineko
```

The hook calls `${CLAUDE_PLUGIN_ROOT}/bin/hypothesis session-start`. `bin/hypothesis` is
a small POSIX launcher that selects the right per-platform binary
(`bin/hypothesis-<os>-<arch>`); if a host has no matching binary it exits 0 silently and
prints a one-line build hint to stderr.

> `hypothesis` pairs with [compass](../compass) (the goal it tests toward) and feeds
> [flow](../flow) (which builds and measures the bets). It works standalone, but the loop
> closes only when those are present.

### Build from source

```sh
scripts/build-plugin-bin.sh hypothesis                       # host platform
scripts/build-plugin-bin.sh hypothesis x86_64-apple-darwin   # cross-target the Intel Mac build
git add bin/ && git update-index --chmod=+x bin/hypothesis bin/hypothesis-*
```

## Platform support

| Host | File | Status |
|---|---|---|
| Linux x86_64 | `bin/hypothesis-linux-x86_64` | bundled |
| macOS Apple Silicon | `bin/hypothesis-darwin-arm64` | bundled |
| macOS Intel | `bin/hypothesis-darwin-x86_64` | built in CI on a macOS runner |

## Plugin layout

```
.claude-plugin/plugin.json     # plugin manifest (version 0.1.2)
hooks/hooks.json               # SessionStart=session-start → ${CLAUDE_PLUGIN_ROOT}/bin/hypothesis
skills/hypothesis/SKILL.md     # the /hypothesis skill (manage the lifecycle)
skills/add/SKILL.md            # the /add skill (add a bet, link to a compass goal)
bin/hypothesis                 # POSIX launcher → hypothesis-<os>-<arch>
bin/hypothesis-<os>-<arch>     # prebuilt binaries
src/ … Cargo.toml              # the Rust crate
```

## Development

```sh
cargo test -p hypothesis     # unit tests
cargo build -p hypothesis
```

## License

MIT

---

## 日本語

`hypothesis` は **PDO（プロダクト発見）の仮説ライフサイクル管理** を行う、Rust 製の
Claude Code プラグイン。検証したい「思い込み（falsifiable な賭け）」を台帳で追跡し、
**出荷 ≠ 検証（build ≠ validate）** を構造的に強制する。

[compass](../compass) が「何を目指すか」を、[backlog](../backlog) が「何が確定キューか」を
持つのに対し、`hypothesis` は **未解決の問い**（「X だと信じている。本当か？」）を持つ。
各レコードは状態を持つ反証可能な文で、発見作業が「作った→ゆえに正しい」へ静かに崩れるのを防ぐ。

```
discovery(hypothesis) → flow → condukt → merge → awaiting-measurement → measure → validate / reject
```

### ライフサイクル（`~/.hypothesis/hypotheses.toml`）

`open`（未検証の賭け）→ `awaiting-measurement`（**出荷したが未計測**。condukt が merge 時に遷移）
→ `validated`（計測した証拠が支持）/ `rejected`（計測した証拠が反証）。
`validate` は `--evidence` 必須、`reject` は `--reason` を取る — **計測した内容を記録せずに終端に動かせない**。
出荷だけでは `awaiting-measurement` 止まり（build ≠ validate）。

### SessionStart hook

`hypothesis session-start` — その project の **open** と **awaiting-measurement** の仮説を
context 注入し、`validate`/`reject` を促す。`linked_goal` が compass charter と乖離した賭けは
`[unlinked]` として警告（発見がゴールから静かに逸れるのを防ぐ）。open/awaiting が無ければ何も出さない。
`HYPOTHESIS_DISABLE=1` で無効化。

### 導入

プラグイン（推奨）: `/plugin marketplace add yukineko/claude-harnesses` → `/plugin install hypothesis@yukineko`。
[compass](../compass)（検証先のゴール）と [flow](../flow)（賭けを build・計測する driver）と組み合わせて
ループが閉じる。
