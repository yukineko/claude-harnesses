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
| a panic anywhere else on the verdict path (stdin read, parse, approval memory, printing the decision) | **block**: exit 2 with the panic on stderr (`run_verdict_guarded`; until 0.2.75 this was `run_hook`'s exit 0, i.e. an allow) |
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
| Truncating redirect (single `>`) | `echo x > existing` (judged by recoverability — see [Truncating redirects](#truncating-redirects-are-judged-by-recoverability)) |
| Bash write into a system directory | `echo x > /etc/paths.d/zz`, `… \| tee /etc/sudoers.d/x`, `cp evil /etc/paths.d/zz` |
| File truncation / wipe | `truncate -s0 f`, `shred f` |
| Filesystem / device writes | `mkfs.ext4 …`, `dd of=/dev/sda` |
| Recursive permission/owner change | `chmod -R 777 .`, `chown -R root .` |
| Mass delete via find | `find . -delete`, `find . -exec rm …` |
| Fork bomb | `:(){ :\|:& };:` |
| Raw network socket path | `… > /dev/tcp/host/port` |
| Interpreter program that destroys / names a gate path | `python3 -c "import shutil; shutil.rmtree('/')"` (deny), `perl -e "unlink q{.githooks/pre-commit}"` (ask) |
| Interpreter program from stdin | `cat x.py \| python3 -`, `python3 /dev/stdin`, `python3 <<EOF` (the program is not on the command line, so it is refused) |

Inline-eval interpreters (`python3`/`perl`/`ruby`/`node`/`php`/`lua` with
`-c`/`-e`/…) are no longer denied for their **shape**: the program text is
audited (`analyze_code_interpreter` / `interpreter_code_verdict` in
`src/detect.rs`). A program that demonstrably deletes something, or whose
embedded shell line is destructive, is a deny; one that names a protected
gate/config path is an ask; a benign program such as `python3 -c "print(1)"`
is allowed (0.2.59). Running a script **file** (`python3 app.py`,
`python3 -m …`) is not judged. A `>` in program text whose "target" cannot be
determined (`print(1 > 0)`, a backticked `` `<k(r)>/x.json` `` in a comment) is
an `ask` marked "Recommended: do not run this." — never a deny, never an allow
(user ruling R2, 2026-10-14). Since 0.2.140 this also holds for the line-level
redirect scan over the body of a quoted-delimiter here-document fed to a
non-shell interpreter (`python3 - <<'PY'`), when that body is closed by its
delimiter line (backlog 4a1cf1ad).

### File operations

- **Write** that replaces a file with **empty content** (wipes it), or that
  overwrites **git internals** (`.git/**`) → denied.
- **Write / Edit / MultiEdit / NotebookEdit** of a **protected gate/config
  path** (see below) → `ask` (hardened to deny where no human can answer):
  blastguard can see that the path controls which gates run, but not whether
  the new bytes strengthen or disarm it.
- Other **Edit / MultiEdit / NotebookEdit** calls are partial edits → allowed.
- A **Write** tool call into a system directory (e.g. `/etc/paths.d/x`) is
  **not** denied by the Write arm: `detect_write` does not consult
  `system_path_block` (only the Bash redirect / `tee` / `cp`-family arms do).
  Observed on the 0.2.108 build: a Write payload for `/etc/paths.d/evil`
  produced no output (allow). Note that `blastguard --protects` currently
  claims otherwise.

**This table lists shapes, not verdicts.** Since 0.2.51 the delete/truncate
rows above (`rm`, `find -delete`, `truncate`, `shred`, `>`, `git clean -f`,
`chmod -R`, `chown -R`) resolve to an `ask` instead of a `deny` when — and only
when — every target can be *proven* to sit inside the project tree or `/tmp`;
see [The location axis](#the-location-axis-blast-radius--0251). Anything that
cannot be proven, leaves those roots, or touches a protected path is denied
exactly as before. A truncating `>` is further relaxed by recoverability
(see [Truncating redirects](#truncating-redirects-are-judged-by-recoverability)).

## What it excludes (never blocks)

Routine edits/deletes of repo **config files** are always allowed, even when the
shape looks destructive:

- `.claude/**` and any nested `.claude/`
- `**/settings.local.json`, `**/.claude/settings.json`
- `**/package.json`
- `**/*.toml`, `**/*.yaml`, `**/*.yml`, `**/*.lock`
- `.config/**` and any nested `.config/`

Truncating redirects to `/dev/null`, `/dev/stdout`, `/dev/stderr` are also fine.

Two carve-outs from the `.claude/**` exemption (`src/exclude.rs`
`is_config_file`): `.claude/plugins/**` and `.claude/projects/**` are state
(deployed binaries, transcripts), not config, and are never exempt; and a
repository checked out under `.claude/worktrees/<name>/` is source — the path
*inside* the checkout is matched against the list (so its `Cargo.toml` is still
exempt, its `src/main.rs` is not).

Protected gate paths outrank this exemption and are never exempt: Claude Code settings
and hook wiring (`.claude/settings.json`, `settings.local.json`,
`.claude/hooks.json`, `.claude/hooks/**`), `.githooks/**`, `.git/hooks/**`,
shell rc files (`.zshrc`, `.bashrc`, `.profile`, …), the gate config tomls
(`deny.toml`, `donegate.toml`, `blastguard.toml`, …, `.precommit-audit.toml`),
and the deployed plugin cache's executables and hook wiring
(`.claude/plugins/cache/**/bin/**`, `.claude/plugins/cache/**/hooks/**`) — the
full list is `PROTECTED_GLOBS` in `src/exclude.rs`. Writing to one (Write, Edit,
`>`, `cp` onto, `sed -i`, …) is an `ask`; disarming one without writing to it
(`rm`, `mv` away, `chmod -x`) is a `deny`. A
deployed launcher without its exec bit dies with `Permission denied` and
emits no finding, so the gate goes dark rather than red (backlog 1b82a049).
For the plugin cache paths, removing read/exec (`chmod -x`, `chmod 644`), `rm` and
`mv` are denied; a write (a truncating redirect, `cp` onto, Write/Edit) resolves
to `ask`, which hardens to deny where no human can answer. Reads and `chmod +x`
stay allowed.

## Design bias

The detector only **denies** *clearly* destructive, hard-to-undo patterns, and
stays out of the way of ordinary work: a single non-recursive `rm file.txt`,
appends (`>>`), and fd redirects (`2>&1`, `>&2`) are all allowed. (Exception:
a non-recursive `rm` whose operand is a `.git` entry is denied — in a linked
worktree that is the `gitdir:` file, and removing it severs the checkout;
backlog eba15868.)

**Ambiguity does not fall through to allow.** This paragraph used to say it did,
which was the pre-`Ask` two-valued behaviour and the opposite of what ships:
forcing every construct the analyser cannot read into `Allow` is the fail-open
`model.rs` exists to name and remove (CLAUDE.md 3 cites it as the worked
example). Unanalysable constructs resolve to `Decision::Ask` — not a verdict
about the command, a refusal to guess about one. Prose that describes a safer
system than the code implements is the trap CLAUDE.md 4 forbids, so the
behaviour is stated here instead.

An `ask` produced by the analyser is only emitted where a human can answer it;
in a headless or agent-driven session `Decision::hardened` collapses it to a
`deny`, never to an allow. "Can answer" is `interactive::ask_available`:
`CLAUDECODE=1` **and** `CLAUDE_CODE_ENTRYPOINT=cli` (the only value measured to
be an interactive terminal), overridable with `BLASTGUARD_ASK=never|always|auto`
(unset = `auto`; an unrecognised value behaves as `never`).

**Exception — the second identical deny becomes an `ask`, after hardening**
(operator ruling 2026-09-18,「ユーザの指示を2度やぶるgateはいらない。2度目はaskせよ」;
`downgrade_on_repeat` in `src/main.rs`). When blastguard has already denied the
same effect (tool + command or `file_path` + deny reason) in the same session,
the repeat is emitted as `ask` — deliberately placed after `hardened()`, so it
is emitted as `ask` in headless sessions too. Only `Deny` is touched; it never
produces an `Allow`. The ledger is `harness_core::repeat`
(`~/.claude/harness-state/repeat/<CLAUDE_CODE_SESSION_ID>/`); an unset session
id or a ledger IO failure keeps the deny.

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
  root, the `deny` becomes an **`ask`** — not an `allow`. Covered verbs:
  recursive/wildcard `rm`, `find -delete` / `-exec rm`, `truncate` / `shred`,
  truncating `>` redirects, `git clean -f`, `chmod -R` / `chown -R`.
  **Exception — recursive `rm` of literal operands** (user rulings 873651b9 and
  3aa215e1: "work products and anything recoverable may be deleted"): it is
  judged by WHAT it destroys. Strictly inside a worktree storage root, `/tmp`,
  `/private/tmp`, `$TMPDIR`, `$HOME/.cache`, `$HOME/Library/Caches`, ignored
  build output in a git work tree, or a git subtree with nothing untracked /
  modified / ignored → `allow`; a git subtree where git reports such content →
  `deny` naming it; the root of any of those areas, `$HOME`, anything directly
  in `$HOME`, or a system directory → `deny` even inside a safe root (0.2.94).
  A git work-tree root is `deny` too, EXCEPT a clone root that lies strictly
  inside `/tmp`, `$TMPDIR`, a cache root or worktree storage: that is an
  `allow` by the earlier class. Anything it cannot determine keeps the
  verdicts above. A build-output-named directory git does not report ignored
  (a tracked `target`) is judged by recoverability like any other git
  subtree. `/var/tmp` is a departure from the spec's "`/var` outside
  `$TMPDIR` is a system dir": paths STRICTLY under `/var/tmp` /
  `/private/var/tmp` are judged by the git classes (and the home / root
  refusals) like any other path, but `/var/tmp` is NOT a temp root — nothing
  there is an `allow` merely for being there. `$TMPDIR` is only a temp root
  when HOME is known and it is not a system directory (e.g. `TMPDIR=/etc`),
  not `$HOME` or an ancestor, and not below `$HOME` (0.2.95).
  Full table: `src/deletion.rs` and the README.ja.md section
  「再帰 `rm` は「何が消えるか」で判定する」.
- **What does not** (each pinned by a test in `tests/scoped_destructive.rs`):
  anything outside every safe root; anything that is not a literal path (`$VAR`,
  `~`, `` `pwd` ``, `*`, `{}`); an unresolvable `cd`; a relative operand a `cd`
  takes out of the tree; **a safe root itself** (`rm -rf .` takes `.git` with
  it); **protected gate paths** (`.git`, `.claude/settings.json`,
  `.githooks/**` are not excused by being nearby — the one exception is a
  copy strictly inside a linked git worktree checkout under a worktree storage
  root, named by an absolute path in a single plain command or a
  Write/Edit, which is that checkout's own work, per the 2026-10-03 user
  ruling); **a symlink that leaves the
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

## Truncating redirects are judged by recoverability

A single `>` (and `N>`, `&>`) to a target that is not `/dev/null` & co. or an
exempt config file is no longer a flat deny. In order (`src/detect.rs`, the
truncating-redirect arm; `src/reversible.rs` `probe`):

1. A protected gate/config path → `ask`; a target inside a system directory →
   `deny`. The system-directory check is made on the text **and** on where the
   kernel really lands it (deepest existing ancestor canonicalised, so
   `<proj>/esc/etc/new.conf` with `esc -> /` is `/etc/new.conf`; backlog
   2287ebae). The `>>` append loop makes the same two checks.
2. The target **does not exist** → allowed (no prior bytes to destroy) —
   unless its real landing could not be resolved (e.g. an unsearchable
   ancestor), which is an `ask`.
3. The target is inside a git work tree whose `git status` answered **and** it
   lies inside this session's safe roots (or a linked worktree checkout under
   a worktree storage root) → allowed, unattended. By operator ruling
   2026-09-18 this covers tracked-clean, tracked-modified **and untracked**
   files alike — `RecoverableFromGit` means "inside a work tree", not "git
   holds these bytes" (see the doc on `reversible::Recovery`).
4. Otherwise the location axis below applies: confined to a safe root → `ask`
   (deny headless); anything else, including a probe that did not answer, →
   `deny`.

Measured on the 0.2.108 debug build (2026-10-14): `echo hi > existing.txt` on
an untracked file in the session's git repo → allow; the same to an existing
file under `/tmp` outside any repo → `ask` (interactive) / `deny` (headless).

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
analyser) resolve to a deny, not to a silent allow — see the table at the top
of this README for how each undetermined case is resolved.

## Also a library

`src/lib.rs` exposes the same detection to other crates in this repo (pure, no
I/O): specguard's forge runs an LLM-generated `test_cmd` through `detect::detect`
before ever handing it to `sh -c`, and condukt's scheduler uses the graded
`classify::classify` risk/reversibility assessor to force outward, irreversible
actions (a deploy, `git push`, a release) through its GATED gate even when an
upstream LLM mislabelled the task.
A deploy word that appears only inside a file name in the task text (for example
`rollout` in `scripts/check-plugin-rollout.py`) does not force the gate [test: crates/blastguard/src/classify.rs::deploy_needle_only_inside_a_touched_file_name_does_not_gate].
A file-path token is a word containing `/`, or one ending in a known extension
such as `.py`/`.rs`/`.sh`/`.md`/`.toml`, with no `:` or `@` [test: crates/blastguard/src/classify.rs::file_path_token_rule_is_narrow].
A path still counts when it comes first, follows a runner word (`run`, `bash`,
...) or shell separator, or starts with `./` [test: crates/blastguard/src/classify.rs::real_commands_containing_paths_still_gate].
A path quoted in backticks still counts [test: crates/blastguard/src/classify.rs::backtick_spans_are_still_never_masked_for_deploy_needles].
Task prose is never parsed as a command: `classify` reads prose with the deploy
and history needles only, so prose that merely describes a command (for example
quotes `git config core.hooksPath` in backticks) is not force-gated [test: crates/blastguard/src/classify.rs::prose_quoting_a_command_is_not_parsed_as_that_command].
The commands a task will actually run are passed separately and go through the
full `classify_command` analysis, so the same command passed as a command still
gates [test: crates/blastguard/src/classify.rs::the_same_command_passed_as_a_command_still_gates].
`classify_change` with no declared command list (`None`) is undetermined, never
Low [test: crates/blastguard/src/classify.rs::undeclared_commands_are_undetermined_even_for_benign_prose].

Two diff-level modules are exported as well (pure, no I/O): `diffrisk`
(`classify_diff` / `changes_public_symbol`) and `callgraph`
(`changed_symbol_names` / `enumerate_callers`, now a thin re-export of
`harness_core::callgraph`). condukt consumes both
(`crates/condukt/src/diffrisk_record.rs`).

## Build

The CLI surface is minimal: `--version` / `-V`, `--help` / `-h` and `--protects`
(the gate's protection statement from `src/protection.rs`: PROTECTS / AGAINST /
GROUNDS lines) short-circuit before stdin is touched; `retro` runs the
[retrospective](#retrospective); `record-approval` reads a PostToolUse payload
(the approval-memory recorder); otherwise it reads a PreToolUse payload from
stdin.

| env var | effect |
|---|---|
| `BLASTGUARD_ASK` | `never` / `always` / `auto` (default): whether an `ask` may be emitted or is hardened to `deny` (see [Design bias](#design-bias)) |
| `BLASTGUARD_APPROVALS_DIR` | approval-memory store location (default `~/.blastguard/approvals`) |
| `CLAUDE_PROJECT_DIR`, `TMPDIR`, `HOME` | read by the hook to build the safe roots |
| `CLAUDE_CODE_SESSION_ID` | keys the repeat-deny ledger |

```sh
cargo build --release -p blastguard   # -> target/release/blastguard
make bins                             # refresh bundled per-platform binaries
cargo test -p blastguard              # unit + integration tests
```

The hooks (`hooks/hooks.json`): **PreToolUse** with matcher
`Bash|Edit|Write|MultiEdit|NotebookEdit` (timeout 10) calls
`${CLAUDE_PLUGIN_ROOT}/bin/blastguard`; **PostToolUse** with matcher `Bash`
(timeout 10) calls `${CLAUDE_PLUGIN_ROOT}/bin/blastguard record-approval`.
`bin/blastguard` is a POSIX-sh launcher that execs the matching
`blastguard-<os>-<arch>` build. If no build is bundled for the host platform,
the launcher prints a warning to stderr **and emits a PreToolUse decision**
saying the call was not evaluated: `ask` when `BLASTGUARD_ASK=always`, or when
it is `auto`/unset and `CLAUDECODE=1` with `CLAUDE_CODE_ENTRYPOINT=cli`;
`deny` otherwise (the same policy as `interactive::ask_available`). It does not
exit silently, because a silent exit 0 is an allow. (Until this was fixed the
launcher exited 0 with no output — a fail-open — and this README said so.)

## Retrospective

A gate's own log answers "what did it say", never "did it prevent anything".
`blastguard retro` answers the second by joining each PreToolUse verdict in the
Claude Code transcript to the `tool_result` for the same `toolUseID`:

```sh
blastguard retro                              # this project, inferred from cwd
blastguard retro --project /path/to/repo
blastguard retro --dir ~/.claude/projects/-path-to-repo
blastguard retro --list                       # one row per intervention
blastguard retro --rule-id rm-recursive       # listing narrowed to one rule (implies --list)
```

Without `--list` it prints a per-rule count table. An unknown argument, or a
flag missing its value, exits 2.

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
