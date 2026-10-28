# compass

> **Goal re-grounding + next-move derivation for Claude Code**, written in Rust.
> Sits **upstream of [condukt](../condukt)**: it decides *what to work on* so condukt can decide *how*.

There's a recurring moment in a project where you lose the thread — "what is this
*for* again, and what should I do next?" `compass` treats that as one pain with two
faces: the **goal goes blurry** (you can't say what "done" is), and that produces the
**blank after a checkpoint** (you finished a piece and nothing obvious is next).

Most tools answer the blank by *listing candidates* (symptom). compass instead keeps
the **goal sharp** and derives the next move as the **gradient** down from it:

```
next move = (sharp goal / definition-of-done) − (reality: git · deepwiki) , the largest right-sized slice
```

It is **subscription-native**: no API key. The judging (carving the goal, reading the
gap, choosing the move) is LLM labor that runs in your Claude Code session; the binary
only keeps state and renders deterministic context. It never calls an LLM or
`AskUserQuestion` itself.

## Where it sits in the harness

| Question | Owner |
|---|---|
| What's the structure? | `deepwiki` |
| Has impl drifted from spec? | `specguard` |
| **What is this for · what is "done" · what's the next move?** | **`compass`** |
| Decompose / schedule / run / done-gate a given task | `condukt` |

`condukt` structures *a task you hand it* — but nothing decides *which* task to pick.
compass closes that **direction → execution** chain: its output (a sharpened goal + an
agreed single move) becomes condukt's input.

## The charter

The living artifact is `.compass/charter.md` (committed alongside the repo, like
condukt). It holds the project's north star and definition-of-done at a higher
altitude than a specguard canon:

| Section | Holds | Freshness |
|---|---|---|
| `north_star` | 1–2 lines: what this project is ultimately for | re-carved when blurry |
| `definition_of_done` | observable done conditions (same vocabulary as condukt's `done_criteria`) | re-carved when blurry |
| `measuring_stick` | how to measure the next move (default: *defensibility × closeness-to-goal ÷ cost*) | project-fixed |
| `current_gap` | goal − reality summary (regenerated each round) | recomputed every round |
| `next_action` | the first physical step on resume (written by the Stop breadcrumb) | updated each Stop (not in the primary working tree, see below) |
| `parked` | pointers to deferred work, written via `compass charter --write` (route itself stores nothing) | written by the skill |

## The C1–C5 freshness gates

"The goal is blurry" is defined as an **unmet-question set** over five graded gates —
a deterministic floor first, then LLM audit. **Zero unmet = the charter is sharp.** One
or more left → drop into the carve loop.

| Gate | Checks | Who | Unmet → carve about |
|---|---|---|---|
| **C1 existence** | charter.md exists with non-empty north_star/DoD | deterministic (binary) | absent → carve from scratch |
| **C2 freshness** | charter hasn't drifted from reality | deterministic (binary) | drift → "is the goal still valid?" |
| **C3 observable** | each DoD item is an observable pass/fail | LLM (skill), + `compass c3-screen` lexical pre-flag | vague → demand a measurable criterion |
| **C4 consistent** | north_star/DoD don't contradict recent work | LLM (skill) | contradiction → a human adjudicates ("did the thread move?") |
| **C5 gradient-able** | the DoD is concrete enough to compute a gap | LLM (skill) | too abstract → demand concretion until one move can be drawn |

`compass c3-screen` is an advisory deterministic lexical screen: it flags DoD items whose
wording looks vague/non-observable as `{"flagged":[{"index","item","reason"}...]}`. It
SUPPLEMENTS the skill's C3 judgment (never blocks, never replaces it).

C2's deterministic floor (cheap, no LLM) flags "drift suspect" when any of: commits
since the charter was last touched `> stale_commits`; wall-clock `> stale_days`;
DoD-referenced paths/symbols no longer exist; or a recorded `next_action` diverges from
what was actually committed.

## The `/compass` flow

`/compass` runs one re-orientation cycle. The skill drives the loop (it owns
`AskUserQuestion`); the binary supplies stateless, deterministic operations:

```
evaluate ─► carve loop ─► charter ─► gap ─► (condukt decompose) ─► route
   │            │            │         │                            │
 C1/C2       ask 1 Q       save the  goal − reality            size triage:
 floor       at a time,   sharpened  largest slice          one right-sized move → condukt
 (binary)    re-check,    charter                           the rest → parked (printed)
             persist
```

1. **evaluate** — the binary prints the C1/C2 floor as `{open_questions, status, round}`
   and initializes-or-loads the persisted carve state. The skill adds its own C3–C5
   questions on top.
2. **carve loop** — while questions remain and a round budget is left, the skill asks
   **one** `AskUserQuestion` at a time (concrete scenarios + an opt-out; the
   highest-authority default goes first as the "recommended" option; no motivation
   either/or; contradictions are adjudicated by the human, never auto-resolved), then
   `compass apply --answer <JSON>` folds it in, persists, and re-checks C1/C2.
3. **charter** — `compass charter --write <JSON>` saves the sharpened charter with an
   **observable** DoD.
4. **gap** — `compass gap` assembles the inputs (DoD / recent activity)
   deterministically; the skill reasons the goal − reality delta and writes it back with
   `compass gap --write <text>`.
5. **condukt decompose** — the agreed single move becomes a task, decomposed by condukt
   into a `Decomposition` JSON with a `size` (xs|s|m|l|xl) on each task.
6. **route** — `compass route` triages by size under **B-plan focus-protection**:
   exactly one right-sized move (default `s`/`m`) goes to condukt; everything else is
   parked. condukt can run things in parallel, but compass commits to **one** move so the
   thread never multiplies. Two edges fall back into the loop:
   - **`GoalTooBig`** (everything `l`/`xl`, no right-sized slice) → re-carve the goal
     *smaller* (often a validate-shaped minimal slice).
   - **`OnlyNoise`** (everything `xs`, nothing hits the main thread) → re-question the
     north_star itself (the direction is exhausted).

Parked work is only printed in the routing JSON; it is not stored anywhere and
does not come back as gap input on the next `/compass` (user ruling 2026-10-27).

### Closing the loop — `outcome`

Routing a move to condukt **ships** it, but shipping is not validated learning
(**build ≠ validate**). When a move completes, `compass outcome --verdict
<forward|unchanged|backward> --evidence <measured result>` judges it against the
`measuring_stick` and appends the verdict to `.compass/outcomes.json` under the
project's **main worktree root** (one history shared by every linked worktree; an
undeterminable root is refused, never answered from a per-worktree store) — **evidence is
required**, an outcome with no measured result is refused, so a move can't be marked
"progress" just for having shipped. The next `compass gap` reads the latest record back
as `last_outcome`, so each round reflects *measured* progress rather than what was merely
built. When driven by [`/flow`](../flow), the sink records this automatically.

## The two hooks

Both are deterministic and non-blocking (neither ever emits a block decision):

| Hook | Event | What it does |
|---|---|---|
| **`compass nudge`** | `SessionStart` (startup/resume/clear) | runs the C1/C2 deterministic floor only (no LLM) and prints a one-line nudge if the charter is absent, blurry, or drift-suspect — "run `/compass` to re-ground." |
| **`compass breadcrumb`** | `Stop` | reads the assistant's final message, extracts an explicit ```` ```compass-next ```` block, and writes it into `charter.next_action`. No LLM, never guesses; if there's no explicit block it does nothing. |

Know what their silence does and does not mean:

- **`nudge` silence is not a guaranteed-fresh charter.** An absent charter always nudges,
  but a charter file that exists and **fails to parse is treated as fresh** (no nudge;
  `nudge --json` reports `{"fresh":true,"reason":null}`). Within C2, a signal git cannot
  compute (not a git repo, `git` failing, no determinable last-touch time) contributes
  nothing rather than tripping. Only a successfully observed signal over threshold trips.
- **`breadcrumb`** stays silent on an empty/invalid Stop payload, an unreadable transcript,
  or no `compass-next` block. A failed charter write is swallowed (not reported); a write
  refused because the cwd is the primary working tree (see below) *is* reported on stderr.
- If the host has no bundled binary, the launcher does not fall silent: `nudge` and
  `breadcrumb` emit a `systemMessage` saying they did NOT run (for `nudge`, also as
  `additionalContext`, stating freshness is UNKNOWN); every other verb, including
  `nudge --json`, exits 127.

**Store writes need a linked worktree.** `charter --write`, `gap --write`, `outcome`,
`opportunity add`, `apply`, `evaluate`, `carve-reset` and `route` are refused (non-zero exit,
nothing written) when the cwd is inside the *primary* working tree of a git repo, because
main's tree only receives merges (CLAUDE.md §8); run them from a linked worktree
(`git worktree add …`). If git cannot say which kind of checkout it is, they are refused too.
Reads still work anywhere, and a cwd outside any git repo behaves as before. In the primary
tree `compass breadcrumb` does not write and says so on stderr.

## Subcommand surface

The binary is thin and deterministic:

| Subcommand | Purpose |
|---|---|
| `compass nudge [--json]` | SessionStart freshness nudge (C1/C2 floor); `--json` emits `{fresh, reason}` so a downstream driver (e.g. flow) can gate on the same floor |
| `compass breadcrumb` | Stop hook: write the next physical step into the charter |
| `compass evaluate` | print the C1/C2 open questions as JSON; init/load carve state |
| `compass apply --answer <JSON>` | fold one human answer in, re-check C1/C2, persist |
| `compass carve-reset` | clear the persisted carve state (start fresh) |
| `compass gap` / `compass gap --write <text>` | assemble gap inputs / persist the gap text. A missing `.compass/opportunities.json` lists no opportunities; an unreadable or corrupt one exits non-zero with `undetermined` on stderr (never `"opportunities": []`) |
| `compass route [--file <path>]` | size-triage a condukt decomposition; park the rest. An unreadable or corrupt `.compass/opportunities.json` exits non-zero with `undetermined` on stderr and prints nothing (no handoff with silently empty opportunity refs) |
| `compass charter` / `compass charter --write <JSON>` | show the parsed charter + config / persist a sharpened charter |
| `compass outcome --verdict <forward\|unchanged\|backward> --evidence <text>…` | record a completed move's verdict vs the `measuring_stick` (evidence required); the next `compass gap` surfaces it as `last_outcome` |
| `compass pivot-check` | print the pivot-or-persevere signal from the trailing outcome streak as `{recommendation, streak, threshold, reason}` from the shared (main-worktree-root) store, for flow to gate on. A never-written store is an empty history (exit 0, persevere); an undeterminable root or a corrupt store exits non-zero with no recommendation |
| `compass opportunity add --title <T> [--outcome <ref>] [--weight <f>]` | record a named bet (PDO OST) under the active outcome (charter `north_star` unless `--outcome` overrides) |
| `compass opportunity list [--json] [--outcome <ref>]` | list the named bets under the active outcome; `--json` prints a JSON array |
| `compass discovery record --title <T> [--session-id <ID>]` | append a discovery row for `<T>` under the given (or env-derived) session, for cross-session dedup; exits non-zero with `undetermined` on stderr when the row was not stored (unresolvable main worktree root, unreadable store, lock/write failure) |
| `compass discovery select [--fingerprint <FP>] [--title <T>]` | mark a discovered task `Selected` (fingerprint given verbatim, or derived from `--title`); with neither flag it prints a usage note and exits 0 (no-op); exits non-zero with `undetermined` on stderr when the store could not be updated |
| `compass discovery list [--json]` | list discovery rows (the shared store plus this checkout's legacy worktree-keyed file); `--json` prints a JSON array, `[]` only when no store file was ever written. When the store cannot be determined — unresolvable root, unreadable file, a corrupt line, or the legacy file cannot be located (git unavailable inside a git checkout) — it exits non-zero with `undetermined` on stderr and prints nothing on stdout (no `[]`, no partial list) |
| `compass discovery migrate-legacy` | copy the legacy worktree-keyed rows of every linked worktree into the shared store; all-or-nothing and idempotent, legacy files untouched; prints one JSON summary (`migrated` / `already_present` / `sources`); exits non-zero with `undetermined` on stderr and writes nothing when the root, `git worktree list` (e.g. git unavailable), a worktree path, a legacy file or the shared store cannot be determined |
| `compass c3-screen` | advisory deterministic screen for the C3 "observable DoD" gate: flags vague/non-observable `definition_of_done` items as `{"flagged":[{"index","item","reason"}...]}`; SUPPLEMENTS the skill's C3 judgment, never blocks |
| `compass suggest-verdict --tests-delta <N> [--regressions <N>] [--gap-closed]` | advisory deterministic DEFAULT verdict for `compass outcome`, derived from observed facts; prints `{"suggested":"forward\|unchanged\|backward"}` (the skill can still override via `outcome --verdict`) |
| `compass score --severity <high\|medium\|low> --effort <xs\|s\|m\|l\|xl> --lens <l1..l5> --goal-proximity <f64>` | deterministic priority score exposing the harness-core scorer — `(severity × goal_proximity) ÷ effort`, with L2/security and L5/safety lenses up-weighted 1.5× — so the scout skill can rank candidates without LLM hand-arithmetic; prints `{"score": f64}` |

## Configuration

`.compass/config.toml` under the project root (all sections/keys optional; a missing
file, section, or key falls back to the defaults below — and so does a file that fails
to parse, **silently**: a typo in this file reverts every key to its default with no
warning):

```toml
[freshness]
stale_commits  = 20          # commits since the charter was last touched (primary drift signal)
stale_days     = 14          # wall-clock days since last touch (secondary signal)
check_dod_refs = true        # check that DoD-referenced paths/symbols still exist

[carve]
max_rounds     = 4           # interrogate sync-round cap. 0 = emit everything as a sentinel (no sync)

[routing]
right_size     = ["s", "m"]  # B-plan: these sizes go to condukt; the rest is parked
```

## Install

### As a Claude Code plugin (recommended)

The plugin bundles the two hooks (`hooks/hooks.json`), the `/compass` skill, and a
launcher for a per-platform binary (built locally — see *Build from source*) — so it runs
entirely on your Claude **subscription**, no API key.

```text
# in Claude Code:
/plugin marketplace add <git-url-of-this-repo>
/plugin install compass@yukineko
```

Hooks call `${CLAUDE_PLUGIN_ROOT}/bin/compass <sub>`. `bin/compass` is a small POSIX
launcher that picks the right per-platform binary (`bin/compass-<os>-<arch>`) for the
host, so the same repo works on Linux and macOS. If a host has no matching binary the
launcher prints a build hint to stderr and makes the gap visible rather than silent:
the `nudge`/`breadcrumb` hooks emit a "did NOT run" `systemMessage` (exit 0, never a
block), and any other invocation exits 127 so no caller reads a missing result as
success.

> **Per-user step:** each user must `/plugin marketplace add <git-url>` once — Claude
> Code does not auto-register a marketplace from a checked-in repo.

### Build from source

Per-platform binaries are **not committed** (`crates/*/bin/*-<os>-<arch>` is
gitignored; only the `bin/compass` launcher is tracked). Build on the target machine
from the workspace root:

```sh
# host platform
scripts/build-plugin-bin.sh compass

# cross-target on a Mac to also produce the Intel build:
rustup target add x86_64-apple-darwin
scripts/build-plugin-bin.sh compass x86_64-apple-darwin
```

The script normalizes the Rust host triple to `compass-<os>-<arch>` in `bin/`. For the
live plugin, use the repo's `scripts/rollout-plugins.sh` (it builds and places the
binary in the plugin cache; see `docs/repo-operations.md`) rather than copying by hand.
The `bin/compass` launcher must be mode 100755 in the git index (`core.fileMode=false`
here), which `check-launcher-exec-bit.py` enforces at pre-commit.

## Platform support

The `bin/compass` launcher selects `bin/compass-<os>-<arch>` at runtime:

| Host | File |
|---|---|
| Linux x86_64 / arm64 | `bin/compass-linux-x86_64` / `bin/compass-linux-arm64` |
| macOS Apple Silicon | `bin/compass-darwin-arm64` |
| macOS Intel | `bin/compass-darwin-x86_64` |

None of these is committed; each must be built on (or for) its platform as above.
macOS binaries can't be cross-built from Linux (they need the macOS SDK).

## Plugin layout

```
.claude-plugin/plugin.json     # plugin manifest
hooks/hooks.json               # SessionStart=nudge / Stop=breadcrumb → ${CLAUDE_PLUGIN_ROOT}/bin/compass
skills/compass/SKILL.md        # the /compass skill (drives the carve loop)
bin/compass                    # POSIX launcher → compass-<os>-<arch>
bin/compass-<os>-<arch>        # per-platform binaries (gitignored, built locally)
src/ … Cargo.toml              # the Rust crate
```

## Development

```sh
cargo test -p compass     # unit tests
cargo build -p compass
```

## License

MIT

---

## 日本語

`compass` は **ゴール再接地と「次の一手」導出** を行う、Rust 製の Claude Code プラグイン。
[condukt](../condukt) の **上流**に座り、「何をやるか」を決める（condukt は「どうやるか」を決める）。

プロジェクトのある時点で訪れる「これは何のためだっけ、次に何をすればいい？」という瞬間を、
一つの痛みの二つの顔として扱う — **ゴールが霞む**（完成の定義が言えない）と、その結果生じる
**一区切り後の空白**（終わったが次が無い）。多くのツールは候補を列挙して空白に答える（対症療法）が、
compass は**ゴールを鋭く保ち、次の一手をそこからの勾配（gap）として導く**。

```
次の一手 = (鋭いゴール / 完成定義) − (現状: git・deepwiki) の 最大かつ右サイズな差分
```

**subscription で完結**（API キー不要）。判定（ゴールを彫る・gap を読む・一手を選ぶ）は
Claude Code セッション内の LLM 労働、バイナリは状態維持と決定論的な context 生成のみ
（LLM も AskUserQuestion も呼ばない）。

### charter（`.compass/charter.md`）

`north_star` / `definition_of_done` / `measuring_stick` / `current_gap` / `next_action` /
`parked` を持つ「生きた一枚」。リポ同居。

### C1–C5 鮮度ゲート

「ゴールが霞んでいる」を未達集合として定義。決定的 floor → LLM 監査の二段。未達 0 件 = 鮮明。
**C1 存在 / C2 鮮度**（バイナリ・決定的）、**C3 観測可能 / C4 整合 / C5 勾配可能**（LLM・skill）。

### `/compass` フロー

`evaluate`（C1/C2 floor）→ carve ループ（1問ずつ問い、再評価・永続化）→ `charter`（彫れた charter を保存）
→ `gap`（ゴール − 現状）→ condukt 分解（size 付与）→ `route`（size triage）。
**焦点保護（B案）**: 右サイズ（既定 `s`/`m`）の一手を **1件だけ** condukt へ、残りは保留へ。
保留は route の JSON に印字されるだけで保存されず、次回 /compass の gap 入力にも再浮上しない（2026-10-27 ユーザー裁定）。
エッジ: **`GoalTooBig`**（全部 l/xl）→ ゴールを小さく彫り直す、**`OnlyNoise`**（全部 xs）→ north_star を問い直す。

**計測ループ（outcome）**: 一手の完了後 `compass outcome --verdict <forward|unchanged|backward> --evidence <計測値>`
で measuring_stick 判定を main worktree root の `.compass/outcomes.json`（全 linked worktree で共有）に記録する（**証拠必須**＝出荷だけでは記録できない、build ≠ validate）。
次の `compass gap` が `last_outcome` として読み戻すので、各ラウンドが「計測された進捗」を反映する
（[`/flow`](../flow) 経由なら sink が自動記録）。

### config（`.compass/config.toml`、既定値）

```toml
[freshness]
stale_commits  = 20
stale_days     = 14
check_dod_refs = true
[carve]
max_rounds     = 4      # 0 = 全部 sentinel
[routing]
right_size     = ["s", "m"]
```

### 2 つの hook

- **SessionStart = `compass nudge`** — C1/C2 の決定的 floor のみ（LLM 不使用）。charter が無い/霞む/drift 疑いなら一行 nudge。
  ただし charter が存在してパースできない場合は fresh 扱い（nudge なし、`--json` は `fresh:true`）、git で計算できない C2 シグナルは trip しない — 無言は「鮮明」の保証ではない。
- **Stop = `compass breadcrumb`** — 本体応答から ```` ```compass-next ```` ブロックを抽出し `charter.next_action` へ書き戻す（推測しない）。

### 導入

プラグイン（推奨）: `/plugin marketplace add <git-url>` → `/plugin install compass@yukineko`。
ソースビルド: リポジトリ root で `scripts/build-plugin-bin.sh compass`（プラットフォーム別バイナリはコミットしない＝gitignore。macOS バイナリは Mac 上でビルド）。稼働プラグインへの反映は `scripts/rollout-plugins.sh`。
