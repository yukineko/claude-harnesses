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

**This table lists shapes, not verdicts.** Since 0.2.51 the delete/truncate
rows above (`rm`, `find -delete`, `truncate`, `shred`, `>`, `git clean -f`,
`chmod -R`, `chown -R`) resolve to an `ask` instead of a `deny` when — and only
when — every target can be *proven* to sit inside the project tree or `/tmp`;
see [The location axis](#the-location-axis-blast-radius--0251). Anything that
cannot be proven, leaves those roots, or touches a protected path is denied
exactly as before.

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

## The location axis (blast radius) — 0.2.51

**Through 0.2.50 the rules judged the SHAPE of a command and never its
LOCATION.** Measured on 0.2.50, real PreToolUse payloads piped into the hook
binary:

```text
rm -rf target   -> deny: recursive rm (-r) can delete an entire directory tree
rm -rf /tmp/foo -> deny: recursive rm (-r) can delete an entire directory tree
rm -rf /usr/lib -> deny: recursive rm (-r) can delete an entire directory tree
rm -rf /        -> deny: recursive rm (-r) can delete an entire directory tree
```

One verdict and one reason for four blast radii that differ by orders of
magnitude. That is not a strict gate but an uninformative one, and an operator
who cannot clear `rm -rf target` does not stop deleting `target` — they reach for
a route with LESS analysis behind it (a python `shutil.rmtree`, a generated shell
script, a bypass-permissions session). The false positives were pushing work out
of the gate's sight.

So 0.2.51 adds an **allowlist of safe roots** (not a denylist of dangerous paths:
a denylist resolves the unlisted case to allow, which CLAUDE.md 3 forbids).
Placement is three-valued in `src/scope.rs` — `Inside` / `IsRoot` / `Outside`,
plus `Undetermined` — and **only `Inside` may relax a verdict**.

- **Safe roots**: the session's `cwd` (its worktree), `CLAUDE_PROJECT_DIR`, and
  `/tmp` / `/var/tmp` (plus `$TMPDIR`). `/`, `/usr`, `/mnt/c/Users`, `$HOME` and
  friends can never become one (`NEVER_A_ROOT`, plus a two-component minimum).
- **What relaxes**: when EVERY target resolves to a strict descendant of a safe
  root, the `deny` becomes an **`ask`** — never an `allow`. Covered verbs:
  recursive/wildcard `rm`, `find -delete` / `-exec rm`, `truncate` / `shred`,
  truncating `>` redirects, `git clean -f`, `chmod -R` / `chown -R`.
- **What does not** (each pinned by a test in `tests/scoped_destructive.rs`):
  anything outside every safe root; anything that is not a literal path (`$VAR`,
  `~`, `` `pwd` ``, `*`, `{}`); an unresolvable `cd`; a relative operand a `cd`
  takes out of the tree; **a safe root itself** (`rm -rf .` takes `.git` with
  it); **protected gate paths** (`.git`, `.claude/settings.json`,
  `.githooks/**` are not excused by being nearby); **a symlink that leaves the
  tree** (real paths are resolved first); and an unfiltered whole-tree walk such
  as `find . -delete`.
- **`ask` only where a human can answer.** In headless runs, condukt workers and
  cron, `Decision::hardened` turns it straight back into a `deny`. **0.2.53
  qualified that sentence**: the approval memory (below) is consulted BEFORE
  hardening, so a headless run CAN be allowed by an approval a human gave in an
  interactive session for this exact effect — which is the point of the feature,
  since the autonomous sessions are the ones drowning in unanswerable asks. That
  approval is a record of a human decision about these parameters, these real
  paths and this content, and it lapses the moment any of them moves. **With an
  empty memory (first use) the threat model is identical to 0.2.50.**
- Library use (`detect::detect`) stays location-blind. The real-path resolution
  is an injected `scope::RealPathResolver` that only the hook binary supplies, so
  `detect` is still pure and condukt / specguard / daily's `sh -c` verdicts are
  unchanged.

## Approval memory: an `Ask` a human answered is not asked again — 0.2.53

`Ask` is correct and it is also **repetitive**. A command a human approved five
minutes ago is asked about again on the next run, and the next. That is not a
cosmetic complaint: it is the exact failure mode that got the sibling crate
`taintguard` **retired by user ruling on 2026-08-24**, because a gate that asks
about ordinary work teaches its operator to stop reading the question. 0.2.53
removes the repetition **without removing the question**.

### The key is the EFFECT, never the script

An approval's fingerprint contains three things. Change any of them and it is a
different key:

| component | what changing it does | why it is in the key |
|---|---|---|
| whitespace-normalised command text | approving `chmod -R 755 sub` says **nothing** about `chmod -R 777 sub` | the effect lives in the parameters |
| every token's **resolved real path** | re-pointing a symlink after the approval **moves** the key rather than inheriting it | `exclude.rs` deliberately does not canonicalize, so "approve once, then swap the link" would otherwise become available the moment a memory exists |
| every target's **content hash** | a target that changed under a standing approval is **re-judged** | 「過去に実行されても変更があったときは再度判断すべきである」 |

And it is bounded by WHERE the effect lands: a fingerprint is only computable
when every token resolves **strictly inside** a safe root
(`scope::Placement::Inside` — the one variant that module documents as allowed to
relax a verdict). An effect reaching outside the project is not "approved with
caveats"; **it is not representable in this store at all.**

### One direction only: `Ask` → `Allow`

The downgrade lives in the **single** `Decision::Ask` arm, so `Deny` is
structurally out of reach — not "not currently downgraded" but unreachable from
this function's only mutating arm. It never widens a blast radius and never
manufactures an approval.

### Recording happens on PostToolUse, the only place a human's "yes" is observable

A `PreToolUse` hook **cannot know what the human answered**. So the store has two
tiers:

1. **PreToolUse** — no approval on record: return the `ask`, and leave behind a
   PENDING fingerprint of the world as the human is about to see it.
2. **PostToolUse** (`blastguard record-approval`) — promote the pending entry.
   **The tool having actually RUN is the evidence they said yes**: a `Deny` never
   reaches `PostToolUse`, and a refused `Ask` never runs, so only
   actually-executed commands are ever recorded.

A pending entry is **never** an approval. Treating it as one would approve every
command blastguard had merely asked about, including the ones that were refused.
Promotion **consumes** the pending entry: one ask, one approval.

The same asymmetry is why the fingerprint cannot simply be recomputed at
`PostToolUse` time — by then the command has already changed the very targets it
hashes (`rm -rf x` leaves `x` absent), so a recomputed key would describe a state
no future `PreToolUse` ever observes. The recorder therefore looks the entry up
by command identity alone and promotes the fingerprint `PreToolUse` computed:
**the state the human actually looked at.**

### Every unknown resolves to "no approval", i.e. the ask stands (CLAUDE.md §3)

`Lookup` is three-valued (`Approved` / `NotRecorded` / `Undetermined`), not a
bool. None of the following can produce `Approved`:

- **expansions, substitutions, quoting** (`$VAR`, `` `cmd` ``, `'`, `"`, `\`) —
  a value that only exists at run time means the same TEXT is not the same
  EFFECT; and quoting is something the whitespace tokeniser cannot faithfully
  split, so an **imperfect tokenisation degrades to "ask"** rather than being
  papered over with a second, driftable copy of `detect`'s parser;
- any token that does not resolve strictly inside a safe root (`Outside` and
  `IsRoot` both refuse);
- a target that exists but cannot be read, or exceeds the 64 MiB hashing cap;
- store IO failure, an entry that does not parse, or an entry that **does not
  name the fingerprint it is filed under** (so a truncated or hand-edited file
  cannot earn an approval from its filename alone);
- an empty store → `NotRecorded`, i.e. first use, i.e. ask.

**No path returns `Approved` on the strength of something it failed to read.**

### Anti-vacuity controls (`tests/approval_memory.rs`)

Measuring only "it stopped asking" passes when nothing ever asked, so both
directions are measured. Observed RED before GREEN — four controls failed on
exactly the four "the memory works" assertions:

| # | control | expected |
|---|---|---|
| 0 | baseline with no memory | **asks** (without this, every "does not ask" below is vacuous) |
| i | second run, same command, same parameters, unchanged target | does not ask |
| ii | parameters changed (`755`→`777`, extra operand) | **asks again** |
| iii | target content changed | **asks again** |
| iv | effect reaching outside the project | **asks however often it runs**, and **no entry is created** in `approved/` |
| v-a | same sequence against a **different store** | **asks** (proof that (i) is reading the store) |
| v-b | PostToolUse step **omitted** | **asks** (proof that a pending is not an approval) |
| — | `Deny` after any number of recorded runs | still `Deny` |
| — | a command containing an expansion | never recorded |

Not a `TempDir`, on purpose: **`/tmp` is a blastguard safe root**, so a project
built under it has no "outside" and control (iv) would be unwritable.
`CARGO_TARGET_TMPDIR` sits under `target/`, which is not a root, so the
inside/outside distinction survives.

### Where it lives

`~/.blastguard/approvals/{pending,approved}/`, overridable with
`BLASTGUARD_APPROVALS_DIR` — an override that exists not for convenience but
because control (v-a) is unwritable without it. One file per approval rather than
an index: concurrent sessions share this store, an index would need a lock, and
that lock's failure would have to resolve to `Undetermined` on every read.
Writes go through temp + rename, so a reader never sees a half-written entry.


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
