# blastguard

A Claude Code **PreToolUse** hook that **denies project-destroying operations**
before they run. It is a single self-contained Rust binary that reads the pending
tool call from stdin, decides allow/deny/ask with a pure function, and — on
anything but an allow — emits the PreToolUse JSON. **Empty** stdin and an
**unmatched tool** are cases where it determined there is nothing to judge, so
it stays silent (allow, exit 0).

Three cases are *undetermined*, not *safe*, and none of them is a silent allow:

| case | resolves to |
|---|---|
| an internal panic during analysis | **deny** (`catch_unwind`) |
| stdin non-empty but unparseable | **ask** → hardened to deny where no human can answer |
| a matched tool whose operand (`tool_input.command`, `file_path`) is missing or not a string | **ask** → hardened |

Until 2026-08-02 the last two were silent allows, and this paragraph said
"Empty/invalid input … determined there is nothing to judge" — which described
the first case correctly and used it to cover the other two. A tool call *was*
being made and blastguard could not read it; that is the definition of failing
to determine. See `src/main.rs` for the entry boundary and
`detect::unreadable_operand` for the per-tool one.

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

The detector only **denies** *clearly* destructive, hard-to-undo patterns, and
stays out of the way of ordinary work: a single non-recursive `rm file.txt`,
appends (`>>`), and fd redirects (`2>&1`, `>&2`) are all allowed.

**Ambiguity does not fall through to allow.** This paragraph used to say it did,
which was the pre-`Ask` two-valued behaviour and the opposite of what ships:
forcing every construct the analyser cannot read into `Allow` is the fail-open
`model.rs` exists to name and remove (CLAUDE.md 3 cites it as the worked
example). Unanalysable constructs resolve to `Decision::Ask` — not a verdict
about the command, a refusal to guess about one. Prose that describes a safer
system than the code implements is the trap CLAUDE.md 4 forbids, so the
behaviour is stated here instead.

An `ask` is only emitted where a human can answer it; in a headless or
agent-driven session `Decision::hardened` collapses it to a `deny`, never to an
allow.

What an intervention actually prevented is reviewable after the fact with
`blastguard retro` (see [Retrospective](#retrospective)).

## Why it exists

In agentic coding, a single `rm -rf`, `git reset --hard`, `git clean -fdx`, or
`>`-overwrite can wipe out uncommitted work or a huge directory in an instant.
These are irreversible, and because they arrive buried inside a stream of tool
calls, expecting a human to catch each one by eye isn't realistic. blastguard is
a safety net dedicated to intercepting only that small set of destructive-yet-
irreversible patterns before they run — deterministically, via a pure function —
and it favors reliably stopping the clearly dangerous over casting a wide net
that gets in the way of ordinary work. Undetermined outcomes (a panic inside the
analyser) resolve to a deny, not to a silent allow — see "Build" below for how
that is implemented.

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
`${CLAUDE_PLUGIN_ROOT}/bin/blastguard`, a POSIX-sh launcher (`bin/blastguard`)
that execs the matching `blastguard-<os>-<arch>` build. If no build is bundled
for the host platform, the launcher prints a warning to stderr and exits 0 — a
known fail-open gap distinct from the deny-on-panic behaviour above: with no
binary to run, there is no analyser present to resolve the "undetermined" case
to a deny in the first place.

## Retrospective

A gate's own log answers "what did it say", never "did it prevent anything".
`blastguard retro` answers the second by joining each PreToolUse verdict in the
Claude Code transcript to the `tool_result` for the same `toolUseID`:

```sh
blastguard retro                              # this project, inferred from cwd
blastguard retro --project /path/to/repo
blastguard retro --dir ~/.claude/projects/-path-to-repo
```

Each intervention gets a tri-state outcome — `executed-anyway` (a human said
yes, so the gate prevented nothing that time), `not-executed`, or `unknown`. A
missing `tool_result` is never counted as a prevention: an abandoned turn or a
truncated transcript would otherwise inflate the gate's apparent value.

Gates that block by exiting non-zero rather than printing PreToolUse JSON
(`guard-maintree-bash.py` and friends) are parsed too, so the review compares
every gate that stopped a call on the same footing.

Reading no transcripts prints `UNDETERMINED` and exits **2** rather than an
empty table — "I measured nothing" must not render as "nothing was wrong",
which is the failure mode this whole crate is built to remove.

Two things the report deliberately does not claim: an approval does **not**
establish that stopping was wrong (only that nothing was prevented), and a
command the agent rephrased and re-ran is **not** detected — so prevention
counts are an upper bound. Both qualifiers are printed with the numbers.
