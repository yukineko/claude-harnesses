# blastguard

A Claude Code **PreToolUse** hook that **denies project-destroying operations**
before they run. It is a single self-contained Rust binary that reads the pending
tool call from stdin, decides allow/deny with a pure function, and — only on a
deny — emits the PreToolUse `deny` JSON. It never breaks a turn: on empty/invalid
input, an unmatched tool, or any internal panic it stays silent and exits 0.

**Subscription-native:** one hook + one bundled binary, no API key.

## What it blocks

It matches `Bash`, `Edit`, `Write`, `MultiEdit`, and `NotebookEdit`.

### Bash commands

| Pattern | Example |
|---|---|
| Recursive `rm` | `rm -rf dir`, `rm -fr dir`, `rm -r -f dir` |
| Wildcard `rm` | `rm *`, `rm -f *`, `rm path/*` |
| `git clean` force + dir/ignored | `git clean -fdx`, `git clean -fd` |
| `git reset --hard` | `git reset --hard HEAD~3` |
| Working-tree discard | `git checkout -- .`, `git checkout --force` |
| Truncating redirect (single `>`) | `echo x > existing` |
| File truncation / wipe | `truncate -s0 f`, `shred f` |
| Filesystem / device writes | `mkfs.ext4 …`, `dd of=/dev/sda` |
| Recursive permission/owner change | `chmod -R 777 .`, `chown -R root .` |
| Mass delete via find | `find . -delete`, `find . -exec rm …` |
| Fork bomb | `:(){ :\|:& };:` |

### File operations

- **Write** that replaces a file with **empty content** (wipes it), or that
  overwrites **git internals** (`.git/**`) → denied.
- **Edit / MultiEdit / NotebookEdit** are partial edits → always allowed.

## What it excludes (never blocks)

Routine edits/deletes of repo **config files** are always allowed, even when the
shape looks destructive:

- `.claude/**` and any nested `.claude/`
- `**/settings.local.json`, `**/.claude/settings.json`
- `**/package.json`
- `**/*.toml`, `**/*.yaml`, `**/*.yml`, `**/*.lock`
- `.config/**` and any nested `.config/`

Truncating redirects to `/dev/null`, `/dev/stdout`, `/dev/stderr` are also fine.

## Design bias

The detector is deliberately **conservative**: it only denies *clearly*
destructive, hard-to-undo patterns. Anything ambiguous falls through to allow, so
blastguard stays out of the way of ordinary work. A single non-recursive
`rm file.txt`, appends (`>>`), and fd redirects (`2>&1`, `>&2`) are all allowed.

## Why it exists

In agentic coding, a single `rm -rf`, `git reset --hard`, `git clean -fdx`, or
`>`-overwrite can wipe out uncommitted work or a huge directory in an instant.
These are irreversible, and because they arrive buried inside a stream of tool
calls, expecting a human to catch each one by eye isn't realistic. blastguard is
a safety net dedicated to intercepting only that small set of destructive-yet-
irreversible patterns before they run — deterministically, via a pure function,
holding the "never breaks a turn" invariant — and it favors reliably stopping the
clearly dangerous over casting a wide net that gets in the way of ordinary work.

## Also a library

`src/lib.rs` exposes the same detection to other crates in this repo (pure, no
I/O): specguard's forge runs an LLM-generated `test_cmd` through `detect::detect`
before ever handing it to `sh -c`, and condukt's scheduler uses the graded
`classify::classify` risk/reversibility assessor to force outward, irreversible
actions (a deploy, `git push`, a release) through its GATED gate even when an
upstream LLM mislabelled the task.

## Build

The CLI surface is minimal: `--version` / `-V` and `--help` / `-h` short-circuit
before stdin is touched; otherwise it reads a hook payload from stdin.

```sh
cargo build --release -p blastguard   # -> target/release/blastguard
make bins                             # refresh bundled per-platform binaries
cargo test -p blastguard              # unit + integration tests
```

The hook (`hooks/hooks.json`) registers on **PreToolUse** with matcher
`Bash|Edit|Write|MultiEdit|NotebookEdit` (timeout 10) and calls
`${CLAUDE_PLUGIN_ROOT}/bin/blastguard`, a POSIX-sh launcher that execs the
matching `blastguard-<os>-<arch>` build, or exits 0 silently if none is bundled
for the host.
