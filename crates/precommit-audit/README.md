# precommit-audit

A fast, **config-driven pre-commit static audit** that runs the same on
**Linux, macOS, and Windows**. It is a from-scratch Rust port of a PowerShell
pre-commit hook, with all project-specific policy lifted out of the code and into
a TOML file — so the binary itself is generic and reusable across repos.

## What it does

On each invocation it inspects the working set (`git diff HEAD` + untracked
files) and reports findings. Built-in **generic** checks:

| Check | Blocks? | Notes |
|---|---|---|
| Source changed without a test | yes | configurable source / test patterns |
| Hard-coded IP address | yes | RFC 5737 test-nets & loopback are benign |
| Hard-coded secret (`password = "…"`) | yes | env getters / placeholders allowed |
| Swallowed exception / `\|\| true` | yes | bare `except:`, `except … : pass`, … |
| Duplicate function definition | yes (opt-in) | heuristic shared-code reuse via `git grep` |
| `local VAR=$(…)` in a `set -e` script | yes | silent-failure bash footgun |
| Broken Markdown links | yes | repo-relative link targets must resolve |
| CRLF/LF line endings | yes | per-extension policy |
| External linters | yes | py_compile, ruff, bash -n, eslint, tsc, radon, semgrep, gitleaks (each optional; a linter not found on `PATH` is **skipped**, not failed; a linter that times out counts as a failure) |
| File too long | **warn only** | advisory nudge to split |
| Subagent review contract | yes (opt-in, `stop` mode only) | `[review_contract]`: blocks until `<path>` (default `.claude/last-review.json`) holds `"verdict": "pass"` and the current diff hash |

**Project-specific** policy is expressed as data-driven `[[rule]]` entries
(regex over added lines, with glob scoping and an allowlist) — never hard-coded.
See [`examples/web-project.toml`](examples/web-project.toml) for a worked config
(node project roots, `console.log` / `print()` rules with glob scoping).
A `[[rule]]` whose `pattern` is not a valid regex is **skipped** with a stderr
warning (the rule does not run); invalid `unless` regexes and invalid globs are
dropped silently.

**Whose changes.** In either mode the changed set is narrowed by session
attribution (`harness_core::attribution`, keyed on `CLAUDE_CODE_SESSION_ID`):
a file that a *peer* session's transcript positively claims, and this
session's does not, is excluded from the audit. When attribution is
undetermined (no session id, e.g. a plain terminal `git commit`; no
transcript) the **full** set is audited, and a file no transcript claims stays
in.

## Install

```sh
cargo install --path .
# or build a release binary:
cargo build --release   # -> target/release/precommit-audit
```

## Adopting in a new project

The binary is project-agnostic — nothing about any one repo is baked in. To use
it in another project:

1. **Install the binary** once (`cargo install --path .`), so `precommit-audit`
   is on your `PATH`.
2. **(Optional) add a config.** Drop a `.precommit-audit.toml` at the repo root.
   With no config the generic checks still run; add config only to tune them or
   to declare project-specific `[[rule]]`s. Start from the documented template
   [`.precommit-audit.toml`](.precommit-audit.toml), or see
   [`examples/web-project.toml`](examples/web-project.toml) for a worked example.
3. **Wire it into a hook** — either the pre-commit framework or a raw git hook:

   ```sh
   # .git/hooks/pre-commit   (chmod +x)
   #!/bin/sh
   exec precommit-audit --mode precommit
   ```

   or the pre-commit-framework / Claude Code Stop forms shown under
   [Usage](#usage) below.
4. **Add project rules as you go.** Each new policy is a `[[rule]]` block (regex
   over added lines, with glob scoping and an allowlist) — you never touch the
   binary. Disable any built-in check you don't want under `[checks]`.

That's it: the same binary serves every repo; each repo's `.precommit-audit.toml`
carries its own policy.

## Usage

```sh
precommit-audit [--mode stop|precommit] [--config <file>] [--root <dir>]
precommit-audit trust   # trust <root> so its .precommit-audit.toml is honored
precommit-audit skip --reason "<why>"   # one-shot bypass, THIS session only
```

- `trust` — add the resolved `--root` to the shared workspace-trust list
  (`harness_core::trust`, the same list `donegate`/`reviewgate`/`tdd` use) so an
  auto-discovered `.precommit-audit.toml` is honored. Until trusted, a
  repo-shipped config is ignored (built-in checks still run on defaults).
- `--mode precommit` — for a pre-commit-framework / git hook on a human commit.
  Exits **1** on failure. Skips the (Claude Code) review contract.
- `--mode stop` (default) — for a Claude Code Stop hook. Honors the subagent
  review contract and exits **2** to feed findings back to the agent. If the
  hook payload has `stop_hook_active: true` (a re-fire within the same stop
  cycle) it exits **0** without auditing. When invoked from a **SessionEnd**
  hook (the bundled `hooks/hooks.json` registers **Stop only**; SessionEnd is
  only reached if you wire it yourself) it runs in **advisory** mode: blocking
  findings are still surfaced (printed prominently *and* recorded as a `block`
  in the audit log) but the exit code for findings stays **0**. Advisory mode
  does *not* mean it can never exit non-zero: a git error computing the
  changed set exits **3**, and a panic exits **2** (both fail closed).
- The mode can also come from the `AUDIT_MODE` env var (`--mode` wins).
- `--config` — defaults to `<root>/.precommit-audit.toml`. A config that fails
  to load exits **64**.
- `--root` — defaults to `$CLAUDE_PROJECT_DIR`, else the git top-level.

Exit codes: `0` clean · `1` blocked (precommit) · `2` blocked (stop) ·
`3` git error under SessionEnd · `64` usage / config error.

Fail-closed paths: a git error while computing the changed set, a git error
mid-scan (`AUDIT INCOMPLETE`), and a panic all block with the mode's blocking
code — never exit 0. Two states are positively detected as "nothing to audit"
and exit **0**: a directory outside any git repository, and a repository with
no commit yet (right after `git init`).

State files live under `<root>/<audit_dir>/` (default `.claude/`):
`.audit-blocked` (block marker) and `audit-log.jsonl` (one JSON line per run).

### As a pre-commit-framework hook

```yaml
# .pre-commit-config.yaml
- repo: local
  hooks:
    - id: precommit-audit
      name: precommit-audit
      entry: precommit-audit --mode precommit
      language: system
      pass_filenames: false
```

### As a Claude Code Stop hook

```json
{ "hooks": { "Stop": [ { "hooks": [
  { "command": "precommit-audit --mode stop", "timeout": 30 }
] } ] } }
```

## Configuration

Every knob lives in `.precommit-audit.toml`; all have built-in defaults, so the
file is optional. Start from the documented template
[`.precommit-audit.toml`](.precommit-audit.toml) in this repo.

### Suppression

- Per line: append `# audit-ignore: <reason>` (use `//` for JS/TS). A reason is
  **required** — a bare marker does not suppress.
- Per file: put `audit-ignore-file: <reason>` in the first 20 lines.
- One-shot bypass: `precommit-audit skip --reason "<why>"`. A reason is
  required, the skip applies only to the session that issued it (keyed on
  `CLAUDE_CODE_SESSION_ID`; without it `skip` refuses with exit 64), it is
  stored under `~/.precommit-audit/state/`, it is consumed by that session's
  next audit, and both the issue and the consumption are recorded. (The old `<audit_dir>/.audit-skip` file was
  removed: it sat in the shared working tree with no attribution, so
  whichever invocation ran next spent it — CLAUDE.md section 5.)

## Relation to the other Stop gates (donegate / reviewgate / tdd)

precommit-audit is **deliberately not** one of the JSON Stop-gates built on
`harness_core::gate`. Those three (donegate, reviewgate, tdd) are *Claude-only*
Stop hooks: they block by printing `{"decision":"block","reason":…}`. (Like
them, precommit-audit also gates project-local config behind
`harness_core::trust` — see **Trust gate** below — but it is not a JSON
Stop-gate.)

precommit-audit is a **dual-mode** hook and so keeps a different contract on
purpose:

- It runs both as a **git / pre-commit-framework hook on a human commit**
  (`precommit` mode, exit **1** on failure per pre-commit convention) and as a
  **Claude Code Stop hook** (`stop` mode, exit **2** to feed findings back), plus
  an advisory mode when invoked from **SessionEnd** (findings exit **0** but are
  still surfaced and logged; git errors and panics still exit non-zero). A git hook cannot speak
  Claude's JSON `decision:block` protocol, so the **exit-code + block-marker**
  contract is required, not an oversight. Its own `hookio` exists for the same
  dual-mode reason.
- It is therefore **excluded from the shared JSON-gate layer** — treat it as a
  sibling, not a fourth member of the trio.

**Trust gate.** Like the trio, precommit-audit now loads an auto-discovered
project `.precommit-audit.toml` only when the root is trusted
(`harness_core::trust`). The blast radius was always narrower than donegate's
(config only *toggles* hard-coded built-in linters and adds **regex** rules — it
can't name an arbitrary command), but `linters.node_projects` can resolve
repo-local `eslint`/`tsc` binaries, so a cloned untrusted repo was the one
execution vector. Now an untrusted repo's config is ignored (built-in checks
still run on defaults) with a one-shot stderr notice; trust the root with
`precommit-audit trust` (the shared list, so `donegate trust` / `tdd trust`
work too) to honor it. An explicit `--config <file>` is the operator's
deliberate choice and is always honored, trusted or not.

## Position in this repo (self-apply)

precommit-audit is a project-agnostic product this repo ships, but it is **not**
integrated into this repo's own git-commit-time gates (`.githooks/pre-commit`,
which runs its own set of independent stdlib-only python3 scanners — see the
`run …` lines in `.githooks/pre-commit`). Where precommit-audit is installed at
user scope it does fire here as a Stop hook (`hooks/hooks.json`). The recorded
gap — no generic secret scan among the git-commit-time gates — was closed by
`scripts/check-hardcoded-secret.py` (secret-guard), which mirrors
`check_hardcoded_secret` (`src/checks/mod.rs:175`) on ADDED lines without
integrating precommit-audit wholesale.

## Why a port

The original hook was PowerShell-only (Windows). This rewrite:

- runs natively on Linux/macOS/Windows as a single static binary,
- is UTF-8 throughout (no CP932 mojibake workarounds),
- separates generic checks (in the binary) from project policy (in TOML),
- ships with an integration test suite (`cargo test`).

## License

MIT
