#!/usr/bin/env python3
"""PreToolUse hook: refuse a Bash command that MUTATES this project's main tree.

The Edit/Write tools are not the only way to change a file — `sed -i`, `rm`,
`mv`, `cp`, `tee`, a `>` redirection, an interpreter one-liner all mutate the
working tree from a Bash call, and guard-maintree-edit.py never sees them. This
hook closes those routes at edit time, for the main checkout of
$CLAUDE_PROJECT_DIR: a mutation whose resolved target lands under the main tree
(and is not git-ignored) is refused, with the same instruction — do it in a
worktree. `git` subcommands (`git apply`, `git checkout -- <path>`, `git
merge`, …) also mutate the tree but are deliberately NOT judged here: merges
and conflict resolution on main must stay possible, and the commit-time gate
check-worktree-isolation.py is what stops a git-made change from landing (see
the NOTE in _Walk.judge and the KNOWN HOLES list).

HONESTY ABOUT WHAT THIS IS (CLAUDE.md 4, and modelled on deny-no-verify.py). The
shell is Turing-complete; a sound "does this command mutate the main tree"
decision is not achievable from the command string. This hook is therefore a
HEURISTIC REDUCTION of the ways the main tree can be dirtied, NOT a proof that it
cannot be. Its backstop is the route-independent one: check-worktree-isolation.py
refuses any COMMIT taken from the main tree, so a mutation this hook fails to
catch still cannot become a durable, shared change on main. Known, unclosed
holes are listed at the bottom of this file rather than left for the next reader
to rediscover.

Resolution rule per candidate target:
  * relative paths — and `$PWD` — resolve against the session's cwd: the hook
    payload's `cwd` field, or this process's own cwd when the field is absent.
    A `cwd` field that is present but unusable (not an absolute path to an
    existing directory) makes the cwd UNKNOWN, and every relative write is
    then refused; absolute targets are still judged normally;
  * on macOS (darwin) the comparison with the main root folds case, because
    the default APFS volume is case-insensitive (`…/Harness` IS main);
  * a target that resolves OUTSIDE the main tree (a worktree, /tmp scratchpad,
    ~/.claude memory) is allowed;
  * a target under the main tree that is git-ignored is allowed (local scratch);
  * a target inside THIS process's OWN `.git/worktrees/<name>/` administrative
    directory (resolved from the hook's own cwd via `git rev-parse
    --absolute-git-dir`, never from the command string) is allowed — it is
    linked-worktree-private transient state (e.g. a stale `index.lock`), not
    main's tracked content, and is not shared with any other worktree/session;
  * a target under the main tree that is not ignored, and not inside that own
    admin dir, is REFUSED — this still covers `.git/config`, `.git/hooks/*`,
    `.git/refs/**`, and every OTHER worktree's `.git/worktrees/<other>/`.

JUDGED BY EFFECT, NOT BY argv[0] (ae4543d5). The same write used to pass by
changing its spelling; each of these is now followed to the path it writes:
  * shell wrappers: `sh -c`, `bash -lc` (any bundle containing `c`), `eval`,
    `env …`, `nohup`, `time`, `nice`, `sudo`, `timeout`, `xargs`, `command`,
    `exec`, `find -exec`, a `$( … )` or backquote (quoted or not; a backquote
    pair is rewritten to `$( … )` before tokenizing, and a backslash-escaped
    backquote pair inside one is unescaped and rewritten as a nested
    substitution, recursively) — the payload is tokenized and judged
    by this same procedure, recursively (depth-capped). `sh -c 'TEXT' NAME A
    B` binds literal A B to `$1 $2` / `"$@"` (NAME to `$0`) inside TEXT;
  * a variable used as the command: `CMD="rm <path>"; $CMD` word-splits the
    known value and judges the resulting words as the command; `eval "$CMD"`
    and `sh -c "$CMD"` judge the known value as a command text;
  * interpreters whose payload is NOT shell — `python -c` / `python - <<EOF`,
    `node -e`, `perl -e`, `ruby -e`, an awk program.
      - A process spawn whose command is a LITERAL — subprocess.run/call/
        check_call/check_output/Popen/getoutput, os.system/popen (python,
        read with the ast module; no cwd= / env= / executable= / **kwargs),
        exec/execSync/execFile/spawn/spawnSync (node; no cwd/env/shell
        option), backquotes / qx / %x / system / exec (perl, ruby; no
        interpolation) — is judged with these same shell rules, so
        `subprocess.run(['cat', '<main>/f'])` is a read and `['rm', …]` is not.
      - Every other spawn (an alias, os.exec*/spawn*, Popen with cwd=, an
        f-string, an awk `| "cmd"` / system()), and every literal spawn in a
        payload that also changes its own cwd or environment (chdir,
        os.environ, %ENV, ENV[…], process.env), is UNPARSED.
      - If the payload contains a FILE-WRITE primitive (open(…) with a w / a /
        x / + mode or a non-literal mode, io.FileIO, os.open with O_WRONLY /
        O_RDWR / O_CREAT / O_APPEND / O_TRUNC, write_text, shutil.copy,
        writeFileSync, File.write, a ruby File.open with a write mode, an awk
        print/printf `>` redirection, …), an UNAMBIGUOUS mutation word
        (remove, unlink, rmtree, rmdir, rename, renames, symlink, chmod,
        chown, truncate, mkdir, makedirs, copyfile, copytree, copy2,
        copymode, move, write_text, write_bytes, writeFile(Sync),
        appendFile(Sync), unlinkSync, rmSync, renameSync, mkdirSync,
        copyFileSync, symlinkSync, … anywhere, no module prefix needed), an
        AMBIGUOUS mutation word (replace, write, copy, link, rm, cp) reached
        through os / shutil / pathlib / fs — an alias bound by import /
        require / assignment / a for-loop over one, `Path(…)`, a name
        imported from one, a destructuring of fs, `fs.copy…` — or a python
        call whose arity only a path method has (`x.replace(target)` with ONE
        argument, `x.copy/move/link/rm/cp(arg)`), a dynamic lookup that could
        hand one out by a name this scan cannot read (getattr, __import__,
        __dict__, importlib, exec/eval/compile, `from os import *`, a node
        computed member call `x[…](…)`, eval / Function / a non-literal
        require), or an UNPARSED spawn, then every literal path in it — and
        every operand handed to it — is judged, and one that lands on main is
        refused. `.read().replace(…)`, `sys.stdout.write(…)`,
        `re.findall('copy', …)`, `s.replace(/a/, 'b')` are not mutations.
      - Only a FILE WRITE that names no literal path at all is refused for that
        reason alone (cannot determine); an unparsed spawn without a literal
        path runs an unknown program (residual). A payload with none of these
        is a read and is allowed, even of main.
      - perl regex / substitution / transliteration literals are blanked before
        the word scan (`print if /copy/` is a read) — only where perl itself
        expects a term, and never one that can run code (an `e` flag,
        `(?{…})`, `@{[…]}` / `${…}`). perl literals are read with backslash
        slash and backslash escapes removed (backslash-slash reads as a slash)
        when extracting paths, and a regex-flag-shaped `/e` / `/gi` is not
        taken for a path. A character-code escape (`\\x2f`, `\\057`,
        `\\x{…}`, `\\o{…}`, `\\N{…}`, `\\cX`) in a payload that mutates is
        undetermined and refused.
      - ruby: a write method (`write`, `delete`, `rename`, `mkpath`, …) on a
        variable bound to `Pathname(…)` / `Pathname.new(…)` anywhere earlier
        in the payload is a write, as is File/IO.open|new with a mode that is
        not a read-only literal (`"r"`, `"rb"`, `File::RDONLY`) — a variable
        mode included, as in python.
      - awk is parsed, not grepped: string literals are blanked first and only
        a `>`/`|` at parenthesis depth 0 inside a print/printf counts
        (`print ($1 > 3)` and `"|"` are not writes).
      - A literal that begins with a trailing run of the main root's
        components (`'/src/harness/x'`, the shape of `$HOME +
        '/src/harness/x'`) counts as main;
  * in-place flags in any spelling: `-i`, `-i.bak`, `-pi`, `-Ei`,
    `--in-place[=sfx]`, BSD `-i ''`, gawk `-i inplace`;
  * downloaders and extractors: `curl -o/--output/-O/--output-dir`,
    `wget -O/-P` (and plain `wget`, which writes into the cwd), `tar -x`
    (`-C`/`--directory` or the cwd), `tar -c` (the archive), `unzip` (`-d` or
    the cwd), `find -delete`, `find -exec/-execdir/-ok` (`{}` replaced by the
    start path wherever it appears in a word), `chmod/chown/chgrp`,
    `rsync/scp/ditto` (the last non-option operand; for rsync and scp,
    options and option values may also follow the operands), and
    `cp/mv/install/ln --target-directory=DIR` / `-tDIR` / `-t DIR` — and the
    GNU coreutils `g`-prefixed names Homebrew installs (grm, gcp, gmv,
    ginstall, gln, gtouch, gmkdir, grmdir) like the plain ones;
  * text PRODUCED by a substitution and then run (`eval "$(…)"`, `sh -c
    "$(…)"`, `$(…)` or a backquote as the program): what it prints cannot be
    known, so the substitution's own command text is judged — it runs as a
    command in its own right, and any literal path in it that lands on main
    is refused (`eval "$(echo rm <main>/f)"`, a printf format naming main);
  * an UNKNOWN PROGRAM (an unknown `$CMD …`, `$(echo rm) …`, the text `eval
    "$(brew shellenv)"` runs) could be any tool, so each of its non-option
    operands that lands on main is refused; with none it is the same class as
    make/cargo/an arbitrary binary and is not refused, but the command that
    PRODUCES it (`brew shellenv` inside the `$( … )`) and every redirection
    around it are judged. This holds through a variable too: a variable
    filled by `$( … )` / backquotes, `printf -v V FMT ARGS`, or `read` /
    `mapfile` from a here-string carries its producer's text, and when that
    variable is run (as the program, `eval "$V"`, `sh -c "$V"`) every literal
    path in the producer text that lands on main is refused (`CMD=$(echo rm
    <main>/f); $CMD`). `read V <<< 'one line'` into a single variable is the
    literal line itself; `set -- A B` sets `$1 $2` / `"$@"` to the literal
    words, so `set -- rm <main>/f; "$@"` is judged as `rm <main>/f`;
  * mktemp CREATES a file or directory: that is a write, judged at the
    directory it creates in (see the mktemp rule under variables below);
    `-u` / `--dry-run` creates nothing.

CWD AND VARIABLES ARE TRACKED THROUGH THE COMMAND, not used as a blanket
excuse. `cd <dir>` changes what a RELATIVE path resolves against for the
commands after it; an ABSOLUTE path into main is a hit no matter what was
`cd`-ed to before it, and `git -C <dir>` re-anchors only that one git command.
Relative paths start out resolving against the session's cwd. A `cd` inside a
subshell `( … )` / `$( … )`, a pipeline stage or a backgrounded job does not
change the cwd seen afterwards. A `cd` whose success is uncertain — one that
ran conditionally (after `&&`/`||`, or before `||`), one inside
if/while/for/case, or one to a directory that does not exist when it is not
followed by `&&` — holds only where it certainly ran (the rest of its `&&`
chain / its branch); after that the cwd is the MERGE of both possibilities,
which is UNKNOWN wherever they differ, and a relative write against an unknown
cwd is refused.

Variables are handled the same way. `S=<dir>`, `export S=<dir>`, `declare`,
`local`, `readonly` and `unset` earlier in the SAME command line are tracked
and expanded (`$S`, `${S}`, `"$S"`), with the same uncertainty rules as `cd`.
A value only known at runtime (`S=$(…)`, `read S`, two branches that
disagree) becomes unresolvable — except `$(mktemp …)`, which is a known
directory plus one freshly invented name (never main or one of main's
ancestors), so a write under it is judged by whether that directory is under
main (`S=$(mktemp -d)`, `cd "$(mktemp -d)"` -> allowed). The directory is:
a template's own directory part, taken relative to `-p DIR` /
`--tmpdir[=DIR]` when given and to the cwd otherwise (an absolute template
stands alone) — a template with any `..` component makes it unknown; for BSD
`-t PREFIX` on darwin, the per-user confstr temp dir (`getconf
DARWIN_USER_TEMP_DIR`), not TMPDIR, and a PREFIX holding `/` or `..` makes it
unknown (GNU `-t`: under TMPDIR); otherwise `-p DIR` / `--tmpdir=DIR`; with
neither, on darwin the confstr dir (observed: BSD mktemp ignores TMPDIR
there), else TMPDIR, else /tmp. The mktemp COMMAND itself is judged as a write
under both the darwin and the GNU/TMPDIR reading when they differ (`TMPDIR=<main>
mktemp`, `mktemp -p <main>`, `mktemp <main>/x.XXXX` -> refused); an unknown
directory is refused as undetermined. A `S=x cmd` prefix assignment reaches
mktemp's TMPDIR. A `for f in A B …` variable stands for the
longest literal prefix its listed values share plus a glob (`for f in
<wt>/*.txt` -> under the worktree; a list mixing <wt> and <main> -> their
common parent, refused); a list holding `$(…)` or an unknown variable, or no
`in` list at all, makes it unknown. A `S=x cmd` PREFIX assignment
only reaches cmd's environment and does not expand `$S` in its own arguments.
What is chosen for a variable that is NOT assigned in the command and is
therefore inherited from the session's shell: only `$HOME` (and `~`) and
`$TMPDIR` are taken from this hook's environment, because they are the values
the hook reliably shares with the session (both are launched by the same Claude
Code process); TMPDIR only when it is an absolute path that is not under main
(a TMPDIR that is set but fails that is unknown; mktemp falls back to /tmp only
when TMPDIR is unset). `$PWD` is the TRACKED cwd (initially the session's cwd,
as above). Every other inherited variable is treated as unknown — this hook's
environment is not the Bash tool's shell, so reading it would judge a value
that may not be the one used. An unknown variable is refused only if it could
point into main: a path that BEGINS with an unknown value (`$X/f`,
`~user/f`, `$(…)/f`) may be absolute and is refused; a path with a `..`
component ANYWHERE after its first unknown or glob component (`/tmp/$X/../f`,
`<scratch>/*/../../main/f`, a for-loop variable followed by `/../`) is refused,
because the unknown part may stand for any depth and the `..` can then climb
anywhere; otherwise the path is judged on its longest LITERAL prefix, so
`<parent-of-main>/$X` is refused, while `/tmp/$X/f` and `<worktree>/$X` (no
later `..`) cannot reach main and are allowed.

UNDECIDABLE INPUT RESOLVES TO DENY (CLAUDE.md 3), as it always has in the twin
guard-maintree-edit.py. Three sites used to answer "I could not tell" with ALLOW:
the main root could not be established, the command would not tokenize, and the
target contained a shell construct this process cannot expand. The middle one was
not theoretical — a here-document whose body contains an apostrophe does not
tokenize, so `cat > <main>/f <<'EOF' … it's … EOF` was waved straight through.

Rather than pay for that in false positives, the guard first makes MORE input
decidable, and only then denies what is left:

  * here-document BODIES are stripped before tokenizing (they are data, not
    shell syntax) — but only when the terminator is actually found, so a `<<`
    inside a quoted string cannot swallow later lines;
  * `~`, `$HOME`, `$PWD` and same-command assignments are expanded;
  * for a target still holding `$`, a glob or a brace, and no `..` after the
    first such component (that is refused), the longest LITERAL path prefix is
    resolved: if that prefix and the main root are on the same ancestor chain
    the expansion could land on main, so it is refused; if they are on disjoint
    branches it cannot, so it is allowed.

The remaining refusals are honest "cannot determine" answers, and each names what
could not be resolved so the caller can rewrite it with a literal path.

HOOK MACHINERY (e033c406, second gate). Rewiring the repo's hook directory
disarms every local gate at once, so it is refused REGARDLESS of the session's
cwd and of which tree it is in — including when CLAUDE_PROJECT_DIR is itself a
linked worktree (no main tree to guard: the walk then runs in a hooks-only
mode that judges nothing else):
  * `git … config` that sets core.hooksPath to anything but `.githooks`,
    unsets it (`--unset`, `unset`), removes / renames the `core` section, or
    opens `--edit`; `git -c core.hooksPath=X …` (separate or glued `-cK=V`) /
    `--config-env` on any git command (X = `.githooks` allowed). git's words
    are expanded with the variables tracked through the command first, so
    `k=core.hooksPath; git config "$k" X` is the same write; a key position
    (config key, `-c` key, `--config-env` key) holding a value this walk cannot
    know is undetermined and refused, except under an explicit read flag. Read
    forms (`git config core.hooksPath`, `--get`, `get`) are allowed;
  * GIT_CONFIG_* environment assignments, judged on the PARSED, quote-removed
    and expanded words — as a command prefix, a bare assignment, `export` /
    `declare` / `local` / `readonly`, or `env NAME=V`: a GIT_CONFIG_KEY_n whose
    value is core.hooksPath (any case) or unknown, and a GIT_CONFIG_PARAMETERS
    that is unknown or names core.hooksPath (single quotes removed, as git
    dequotes it), are refused. Separately, any GIT_CONFIG_* name in the raw
    text of a command that also names core.hooksPath is refused;
  * git subcommands that write working-tree paths named by a pathspec — `rm`,
    `mv`, `checkout`, `restore`, `clean`, `stash push` / `stash -- …`, and
    `apply` / `am` through `--directory` / `--include` — when a pathspec
    (expanded, `:(top)` / `:/` magic stripped, resolved against the tracked
    cwd or `git -C DIR`) names, lies under, or as a git glob (whose `*`
    crosses `/` and leading dots) can match `.githooks` or `.git/hooks`.
    `:!` / `:(exclude)` pathspecs write nothing and are skipped. Broad forms
    with NO pathspec stay allowed (see KNOWN HOLES);
  * every write target — the same targets judged for main above, `chmod -x
    FILE` included (a mode starting with `-` is the mode, not an option) —
    that, as written or resolved, is inside `.githooks`, inside `.git/hooks`,
    is a `.git/config`, or is a `config.worktree` under `.git`, or that has a
    glob component that can match one of those (`.githook*`, `.git/hoo*`; a
    glob not starting with `.` does not match a dot-name, as in the shell). A
    `$( … )` / backquote that prints a git directory (`git rev-parse
    --git-dir` / `--git-common-dir` / `--absolute-git-dir` / `--git-path X`)
    stands for `.git` (`.git/X`), so `$(git rev-parse --git-dir)/config` is
    `.git/config`. Reading them is allowed; `cp` FROM them is judged as a
    write like every `cp` operand. In the hooks-only mode a command this walk
    cannot tokenize or place is refused only if its text names `.githooks`,
    `.git/hooks`, `.git/config`, `config.worktree` or core.hooksPath.

DENY LEDGER (e033c406, scripts/deny_ledger.py). Every refusal above is appended
to the per-session ledger `~/.claude/state/maintree-deny/<session_id>.jsonl` — or, when that cannot be
written, the temp-dir fallback `<tmp>/maintree-deny-<uid>/<session_id>.jsonl`
(both are always read) — target realpath, `raw` (the
command's first line) truncated to 120 characters, the reason's first line
truncated to 300, and a snapshot of the target's state. A refusal that can be
written to neither location keeps its deny and says so. A call this
guard's own rules ALLOW is then checked against that ledger: if it names a main
target refused earlier in the same session (within 25 guarded calls / 20
minutes), it is refused quoting the earlier refusal. That refusal is an ASK
(`permissionDecision: "ask"` JSON on stdout, exit 0) only in an interactive
terminal session (CLAUDECODE=1 and CLAUDE_CODE_ENTRYPOINT=cli); anywhere else
it is hardened to a deny (exit 2). An unreadable or corrupt ledger, or an
unusable session_id, refuses the same way — a bad ledger file only for 20
minutes from its mtime, after which it is renamed aside to
`<name>.corrupt-<ts>` with a stderr notice and the call proceeds.
scripts/stop-verify-worktree.py
re-checks the refused targets at Stop, and scripts/deny-ledger-clear.py
(UserPromptSubmit) empties the session's ledger. See deny_ledger.py for the
exact rules and residuals.

A crash of this script (an uncaught exception) is a deny, not the exit-1
"non-blocking error" Claude Code would otherwise let through; so is a
deny_ledger.py that fails to import.

    exit 0   allow (or, with the ask JSON on stdout, ask)
    exit 2   deny; stderr shown to the model
"""

from __future__ import annotations

import ast
import fnmatch
import json
import os
import re
import shlex
import subprocess
import sys

try:
    # Launched by path, so sys.path[0] is scripts/ already; made explicit for
    # callers that exec this file from elsewhere. A failed import must not
    # crash the hook (a crash exits 1, which Claude Code treats as allow).
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    import deny_ledger  # noqa: E402
    _LEDGER_IMPORT_ERROR = ""
except Exception as _e:  # noqa: BLE001 — any import failure resolves to deny
    deny_ledger = None
    _LEDGER_IMPORT_ERROR = f"{type(_e).__name__}: {_e}"

def _git(cwd: str, *args: str) -> str | None:
    try:
        out = subprocess.run(
            ("git", *args), cwd=cwd, capture_output=True, text=True, timeout=10
        )
    except (OSError, subprocess.SubprocessError):
        return None
    if out.returncode != 0:
        return None
    return out.stdout.strip()


# Probe outcomes. Three answers, not two (crates/blastguard/src/model.rs,
# harness_core::verdict). Kept literal here rather than imported: these hook
# scripts are launched standalone by path, and an ImportError would take the
# gate out entirely.
OK = "ok"
NO_REPO = "no-repo"
UNDET = "undetermined"

_NO_REPO_MARKERS = ("not a git repository", "not a git repo")


def _probe(cwd: str, *args: str) -> tuple[str, str | None]:
    """Run a git query and classify the outcome as OK / NO_REPO / UNDET."""
    try:
        r = subprocess.run(
            ("git", *args), cwd=cwd, capture_output=True, text=True, timeout=10
        )
    except (OSError, subprocess.SubprocessError):
        return UNDET, None  # git not runnable here, or it timed out
    if r.returncode == 0:
        value = r.stdout.strip()
        return (OK, value) if value else (UNDET, None)
    if any(m in (r.stderr or "").lower() for m in _NO_REPO_MARKERS):
        return NO_REPO, None
    return UNDET, None


def _main_root() -> tuple[str, str | None]:
    """(state, main-tree root).

    OK       -> guard mutations landing under the returned root.
    NO_REPO  -> git gave a determinate "nothing to guard here" (not a
                repository at all, or this anchor IS a linked worktree, which is
                the sanctioned place to work). Allow.
    UNDET    -> could not tell. Resolves to deny at the callsite (3.).
    """
    proj = os.environ.get("CLAUDE_PROJECT_DIR")
    if not proj:
        # No project anchor. Fall back to the intrinsic test from this process's
        # own cwd rather than allowing everything — the twin
        # guard-maintree-edit.py makes the same fallback for the same reason.
        proj = os.getcwd()
    proj = os.path.realpath(proj)
    # Confirm it is a main checkout (git-dir == common-dir); if it is itself a
    # worktree or not a repo, we have no main tree to guard from here.
    st_gd, gd = _probe(proj, "rev-parse", "--absolute-git-dir")
    st_cm, cm = _probe(proj, "rev-parse", "--path-format=absolute", "--git-common-dir")
    # Undetermined first: a NO_REPO reading of one probe proves nothing while the
    # other one could not answer at all.
    if st_gd == UNDET or st_cm == UNDET:
        return UNDET, None
    if st_gd == NO_REPO or st_cm == NO_REPO:
        return NO_REPO, None
    assert gd is not None and cm is not None  # OK implies a value
    if os.path.realpath(gd) != os.path.realpath(cm):
        return NO_REPO, None  # a linked worktree — mutating here is the point
    return OK, proj


def _under(child: str, parent: str) -> bool:
    parent = parent.rstrip("/")
    return child == parent or child.startswith(parent + "/")


# macOS's default APFS volume is case-INsensitive: `/Users/u/src/Harness` and
# `/USERS/u/src/harness` name the main tree too, and realpath does not
# canonicalise the case of a path. So on darwin, comparisons against the main
# root fold case on both sides. (On a case-sensitive APFS volume this can only
# over-refuse a sibling whose name differs from main's by case alone.)
_FOLD_CASE = sys.platform == "darwin"


def _fold(p: str) -> str:
    return p.casefold() if _FOLD_CASE else p


def _under_main(child: str, root: str) -> bool:
    return _under(_fold(child), _fold(root))


def _resolve(root: str, path: str) -> str:
    if not os.path.isabs(path):
        path = os.path.join(root, path)
    return os.path.realpath(path)


# Constructs this process cannot expand from the command string alone. `~` is
# NOT here: it expands deterministically, so it is expanded and then judged.
_UNRESOLVABLE = set("$`*?{}")

# Stand-in values. Each contains `$`, so a path built from one is unresolvable
# and is judged on its literal prefix (see _hit) — never silently resolved.
UNKNOWN_VAL = "$__undetermined__"  # a variable whose value could not be tracked
SUBST_WORD = "$__substitution__"   # the output of `$( … )` / `<( … )`
MKTEMP_MARK = "$__mktemp__"        # the fresh name `mktemp` invents, one component
TILDE_USER = "$__tilde_user__"     # `~user/…`, which this process does not resolve

_VAR_REF = re.compile(
    r"\$\{([A-Za-z_][A-Za-z0-9_]*|[0-9]+|[@*])\}|\$([A-Za-z_][A-Za-z0-9_]*|[0-9@*])"
)
_ASSIGN = re.compile(r"^([A-Za-z_][A-Za-z0-9_]*)\+?=(.*)$", re.S)


class _State:
    """What the shell's state is known to be at a point in the command.

    rel   directory a RELATIVE path resolves against; None = unknown.
    pwd   value of $PWD; None = unknown.
    vars  shell variables assigned earlier in this same command, already
          expanded. A key whose value is UNKNOWN_VAL was assigned something this
          process cannot know (`$(…)`, `read`, a loop variable, two branches
          that disagree). A key that is ABSENT was never assigned here: its value
          is whatever the session's shell inherited, which this process cannot
          see — the hook's own environment is NOT the Bash tool's shell.
    """

    __slots__ = ("rel", "pwd", "vars")

    def __init__(self, rel: str | None, pwd: str | None, vars_: dict[str, str]):
        self.rel = rel
        self.pwd = pwd
        self.vars = vars_

    def copy(self) -> "_State":
        return _State(self.rel, self.pwd, dict(self.vars))


def _merge(a: _State, b: _State) -> _State:
    """The state when either `a` or `b` may hold: whatever they disagree on
    becomes unknown (never the more permissive of the two)."""
    vs: dict[str, str] = {}
    for k in set(a.vars) | set(b.vars):
        va, vb = a.vars.get(k), b.vars.get(k)
        vs[k] = va if (va is not None and va == vb) else UNKNOWN_VAL
    return _State(
        a.rel if a.rel == b.rel else None,
        a.pwd if a.pwd == b.pwd else None,
        vs,
    )


def _expand(path: str, st: _State) -> str:
    """Expand what this process can know: a leading `~`, variables assigned
    earlier in the same command, `$HOME`, and `$PWD` (the tracked cwd).
    Anything else is left in place, and so stays unresolvable."""
    if path.startswith("~"):
        if path == "~" or path.startswith("~/"):
            home = st.vars.get("HOME", os.environ.get("HOME"))
            path = (home + path[1:]) if home else TILDE_USER + path[1:]
        else:
            path = TILDE_USER + path[1:]

    def sub(m: re.Match) -> str:
        name = m.group(1) or m.group(2)
        if name in st.vars:
            return st.vars[name]
        if name == "HOME":
            return os.environ.get("HOME") or m.group(0)
        if name == "PWD":
            return st.pwd or m.group(0)
        return m.group(0)

    # One pass: a substituted value is never itself re-expanded.
    return _VAR_REF.sub(sub, path)


def _literal_prefix(path: str) -> str:
    """The longest leading path segment sequence free of unresolvable syntax.

    `/a/b/*.rs` -> `/a/b/`; `*.rs` -> `` (which resolves to the cwd).
    """
    hits = [path.find(c) for c in _UNRESOLVABLE if c in path]
    if not hits:
        return path
    cut = path.rfind("/", 0, min(hits))
    return path[: cut + 1] if cut >= 0 else ""


def _abs_target(target: str, st: _State, root: str) -> str | None:
    """The realpath a write target resolves to, or None when it holds
    something this process cannot expand or is relative to an unknown cwd."""
    p = _expand(target, st)
    if not p or any(c in p for c in _UNRESOLVABLE):
        return None
    if not os.path.isabs(p) and st.rel is None:
        return None
    return _resolve(st.rel or root, p)


def _own_worktree_gitdir(root: str) -> str | None:
    """The absolute git-dir of the CALLING process's own worktree, i.e. the
    directory this hook is actually running from (cwd), resolved via git
    itself — NOT trusted from any string in the command being inspected.

    Returns None (undetermined) if this cannot be established, so the caller
    resolves undetermined to "not exempt" (CLAUDE.md 3: block side)."""
    gd = _git(os.getcwd(), "rev-parse", "--absolute-git-dir")
    if gd is None:
        return None
    gd = os.path.realpath(gd)
    # Only a genuine `.git/worktrees/<name>` administrative dir under THIS
    # main root qualifies — a bare repo's own .git, or a git-dir that is not
    # actually nested under root/.git/worktrees/, is not the carve-out this
    # exists for.
    worktrees_root = os.path.realpath(os.path.join(root, ".git", "worktrees"))
    if not _under(gd, worktrees_root) or gd == worktrees_root:
        return None
    return gd


def _hits_main(root: str, path: str, own_gitdir: str | None, st: _State) -> bool:
    """True if `path` (as written at a point where the shell state is `st`)
    could land on a non-ignored location under the main tree."""
    path = _expand(path, st)
    if any(c in path for c in _UNRESOLVABLE):
        # A shell variable this process does not know ($WT), a glob, or a brace
        # expansion. It cannot be resolved to a literal path — but "unresolvable"
        # is not "harmless" (3.), so decide on the longest LITERAL prefix: the
        # expansion can only land under the main tree if that prefix and the root
        # lie on the same ancestor chain. `<wt>/*.rs` provably cannot (disjoint
        # branches) and is allowed; `<parent-of-root>/*` and a bare `*.rs` both
        # can, and are refused.
        comps = path.split("/")
        first = next(i for i, c in enumerate(comps) if any(ch in c for ch in _UNRESOLVABLE))
        if ".." in comps[first + 1:]:
            # `<scratch>/*/../../main/f`: the unknown component may stand for
            # any depth (a glob, a variable holding `a/b/c`), so a later `..`
            # can climb anywhere — main included. The literal prefix proves
            # nothing once that happens.
            return True
        if comps[first] == MKTEMP_MARK and first > 0 and not any(
                ch in c for c in comps[first + 1:] for ch in _UNRESOLVABLE):
            # `$(mktemp …)`: a name that did not exist until mktemp invented it,
            # directly under a known directory. It is never main or one of
            # main's ancestors, so only "is that directory under main" decides.
            base = "/".join(comps[:first]) or "/"
            if os.path.isabs(base):
                return _under_main(os.path.realpath(base), root)
        prefix = _literal_prefix(path)
        if not prefix and path[0] in "$`":
            # The path BEGINS with a value this process does not know, which
            # may itself be absolute (`$X/f`, `~user/f`, `$(…)`): it could be
            # anywhere, main included. Only a leading glob/brace is relative.
            return True
        if not os.path.isabs(prefix):
            if st.rel is None:
                return True  # relative to a cwd we could not determine
            anchor = _resolve(st.rel, prefix)
        else:
            anchor = os.path.realpath(prefix)
        return _under_main(anchor, root) or _under_main(root, anchor)
    if not os.path.isabs(path) and st.rel is None:
        return True  # relative to a cwd we could not determine (3.)
    resolved = _resolve(st.rel or root, path)
    if not _under_main(resolved, root):
        return False
    # Narrow carve-out: the calling worktree's OWN `.git/worktrees/<name>/`
    # administrative directory (e.g. a stale index.lock) is not main's tracked
    # content and is not shared with any other worktree/session, so it is safe
    # to mutate. This is resolved from the hook's own cwd via git — never from
    # a string inside the candidate path — and is scoped to exactly one
    # worktree's admin dir, not a blanket `.git/` or `.git/worktrees/` allow
    # (main's own .git/config, .git/hooks/*, other worktrees' admin dirs, etc.
    # all remain protected below).
    if own_gitdir is not None and _under(resolved, own_gitdir):
        return False
    # exit 0 = ignored (local scratch, allowed). Anything else — including git
    # being unrunnable — means we did not establish that it is ignored, so it
    # counts as hitting main (3.).
    try:
        ignored = subprocess.run(
            ("git", "check-ignore", "-q", resolved),
            cwd=root, capture_output=True, timeout=10,
        )
    except (OSError, subprocess.SubprocessError):
        return True
    return ignored.returncode != 0


def _fragment_of_root(root: str, path: str) -> bool:
    """True if `path` BEGINS with a trailing run of the main root's components,
    e.g. root `/Users/u/src/harness` and path `/src/harness/x` or
    `harness/x`. That is the shape of a main-tree path rebuilt at runtime from
    a value this process cannot see (`os.environ['HOME'] + '/src/harness/x'`).
    Applied to interpreter payload literals only."""
    rc = [c for c in _fold(root).split("/") if c]
    pc = [c for c in _fold(os.path.normpath(path)).split("/") if c and c != "."]
    for k in range(1, len(rc) + 1):
        if pc[:k] == rc[-k:]:
            return True
    return False


# `<<WORD`, `<<'WORD'`, `<<"WORD"`, `<<-WORD`. Not `<<<` (a here-STRING has no
# body): the look-arounds keep the regex off every `<<` inside a `<<<`.
_HEREDOC_START = re.compile(
    r"(?<!<)<<(?!<)-?\s*(?:'([^']*)'|\"([^\"]*)\"|([A-Za-z_][A-Za-z0-9_]*))"
)


def _strip_heredoc_bodies(command: str) -> tuple[str, list[str | None]]:
    """Drop here-document BODIES so an apostrophe in prose cannot make the whole
    command untokenizable. The redirection targets that matter all appear on the
    line that OPENS the heredoc, which is kept.

    Returns the stripped text and the bodies in order of appearance (None for a
    heredoc whose terminator was not found). The bodies are kept because a
    heredoc fed to an interpreter (`python3 - <<EOF`, `bash <<EOF`) IS code, and
    is judged as such.

    Fail-safe on ambiguity: a `<<` that appears inside a quoted string has no
    real terminator, and swallowing lines until EOF could hide a later mutation.
    So lines are dropped only when the terminator is actually found; otherwise
    the text is left exactly as-is and the tokenizer's verdict (deny) stands.
    """
    lines = command.split("\n")
    out: list[str] = []
    bodies: list[str | None] = []
    i = 0
    while i < len(lines):
        line = lines[i]
        out.append(line)
        i += 1
        for m in _HEREDOC_START.finditer(line):
            delim = m.group(1) or m.group(2) or m.group(3)
            end = i
            while end < len(lines) and lines[end].strip() != delim:
                end += 1
            if end < len(lines):
                bodies.append("\n".join(lines[i:end]))
                i = end + 1  # body + terminator consumed
            else:
                bodies.append(None)
                # no terminator in sight — keep the lines, decide on the text.
    return "\n".join(out), bodies


_WORD_BREAK = set(" \t\n;&|()<>")


def _newlines_to_separators(command: str) -> str:
    """Rewrite every UNQUOTED newline as an explicit ` ; ` command separator, and
    drop comments, so each line becomes its own segment.

    A newline ends a simple command exactly like `;` does, but the tokenizer
    used to turn it into an empty token that no separator check recognised: the
    lines of `echo hi<NL>rm <main>/f` merged into ONE segment whose program was
    `echo`, so the second-line `rm` was never judged (9fdb49d8). The reverse
    also held — a second line's operands became extra operands of the first
    line's `cp`, refusing a harmless command.

    Only newlines that the shell itself treats as separators are rewritten:
      * inside '…' or "…" a newline is data and is kept;
      * backslash-newline (outside '…') is a line CONTINUATION and is removed,
        joining the two lines, as the shell does;
      * a `#` at the start of a word begins a comment that runs to the newline;
        the comment is dropped here (and the tokenizer's own, looser comment
        rule is switched off), because a comment swallowing a rewritten `;`
        would hide every later line.
    Here-document bodies are removed before this runs (_strip_heredoc_bodies).
    An unbalanced quote leaves the rest of the text untouched, so the tokenizer
    still fails on it and the command is refused (3.).
    """
    out: list[str] = []
    i, n = 0, len(command)
    quote: str | None = None
    while i < n:
        c = command[i]
        if quote == "'":
            out.append(c)
            if c == "'":
                quote = None
            i += 1
            continue
        if c == "\\" and i + 1 < n:
            if command[i + 1] == "\n":
                i += 2  # line continuation: not a separator, join the lines
                continue
            out.append(command[i : i + 2])
            i += 2
            continue
        if quote == '"':
            out.append(c)
            if c == '"':
                quote = None
            i += 1
            continue
        if c in ("'", '"'):
            quote = c
            out.append(c)
        elif c == "#" and (not out or out[-1][-1:] in _WORD_BREAK):
            while i < n and command[i] != "\n":
                i += 1
            continue  # the newline (if any) is handled on the next pass
        elif c == "\n":
            out.append(" ; ")
        else:
            out.append(c)
        i += 1
    return "".join(out)


_PUNCT = set("();<>|&")
# Longest first. shlex returns a RUN of punctuation as one token (`);`, `>&`),
# so runs are re-split into the operators the shell would see.
_OPERATORS = sorted(
    ["&>>", "<<<", ";;&", "&&", "||", ";;", ";&", "|&", ">>", ">|", ">&", "&>",
     "<<", "<&", "<>", "<(", ">(", ";", "&", "|", "(", ")", "<", ">"],
    key=len, reverse=True,
)


def _split_punct(tok: str) -> list[str]:
    if not tok or any(c not in _PUNCT for c in tok):
        return [tok]
    out: list[str] = []
    i = 0
    while i < len(tok):
        for op in _OPERATORS:
            if tok.startswith(op, i):
                out.append(op)
                i += len(op)
                break
        else:  # unreachable: every char of _PUNCT is a 1-char operator
            out.append(tok[i])
            i += 1
    return out


def _backquotes_to_subst(text: str) -> str:
    """Rewrite each shell backquote substitution `` `…` `` (unquoted or inside
    double quotes — not inside single quotes, not escaped) as `$( … )`, so it
    is walked like one: its own command is judged, and its output is an
    unknown word. An unterminated backquote is left as it is."""
    out: list[str] = []
    i, sq, dq = 0, False, False
    while i < len(text):
        ch = text[i]
        if sq:
            out.append(ch)
            sq = ch != "'"
        elif ch == "\\" and i + 1 < len(text):
            out.append(text[i : i + 2])
            i += 1
        elif ch == "'" and not dq:
            out.append(ch)
            sq = True
        elif ch == '"':
            out.append(ch)
            dq = not dq
        elif ch == "`":
            j = i + 1
            while j < len(text) and text[j] != "`":
                j += 2 if text[j] == "\\" else 1
            if j >= len(text):
                out.append(text[i:])
                break
            # Inside backquotes `\``, `\$` and `\\` lose their backslash, so an
            # escaped backquote there opens a NESTED substitution.
            inner = re.sub(r"\\([$`\\])", r"\1", text[i + 1 : j])
            out.append("$(" + _backquotes_to_subst(inner) + ")")
            i = j
        else:
            out.append(ch)
        i += 1
    return "".join(out)


def _tokenize(command: str) -> list[str] | None:
    lexer = shlex.shlex(
        _newlines_to_separators(command), posix=True, punctuation_chars="();<>|&"
    )
    lexer.whitespace_split = True
    # Comments were already removed with shell word-start semantics; shlex's own
    # rule also fires mid-word (`a#b`) and would swallow the rest of the input.
    lexer.commenters = ""
    try:
        raw = list(lexer)
    except ValueError:
        return None
    return [t for tok in raw for t in _split_punct(tok)]


# ---------------------------------------------------------------------------
# Effect analysis
# ---------------------------------------------------------------------------

class _Hit(Exception):
    """A write target that lands on the main tree."""


class _Undet(Exception):
    """Could not determine what a (nested) command writes to — resolves to deny."""


class _Unparseable(Exception):
    """The top-level command does not tokenize."""


class _HookHit(Exception):
    """A write into the repo's hook machinery, or a rewiring of core.hooksPath
    (e033c406). args[0] is the target as written, args[1] says why."""


# ---------------------------------------------------------------------------
# Hook-machinery protection (e033c406, second gate)
# ---------------------------------------------------------------------------
# Disarming the hooks disarms every local gate at once (CLAUDE.md 7: the gates
# live in .githooks). So these are refused REGARDLESS of which tree they are in
# and regardless of the session's cwd — a linked worktree's `git config` writes
# the SHARED <common-dir>/config, and a worktree's `.githooks` is the same
# tracked gate code. Reading them stays allowed: only write targets and
# rewiring commands reach these checks.
HOOKS_KEY = "core.hookspath"
SANCTIONED_HOOKS_PATHS = {".githooks", ".githooks/"}
# Sentinel main root for the hooks-only pass (no main tree to guard): never a
# prefix of a real path, and realpath-safe (no NUL).
_NO_MAIN = "/nonexistent/.maintree-guard-no-main-tree"


_GLOB = set("*?[")


def _comp_may_be(comp: str, name: str, dotglob: bool = False) -> bool:
    """`comp` names `name`, or is a glob that can match it. Like the shell, a
    glob only matches a leading-dot name when the pattern itself starts with
    `.` — unless `dotglob` (git pathspecs, whose `*` matches dots)."""
    if comp == name:
        return True
    if not any(ch in comp for ch in _GLOB):
        return False
    if name.startswith(".") and not comp.startswith(".") and not dotglob:
        return False
    return fnmatch.fnmatchcase(name, comp)


def _hook_protected(path: str, dotglob: bool = False) -> bool:
    """True if `path` (any form; folded for case on darwin) is — or, through a
    glob component (`.githook*`, `.git/hoo*`), may be — inside `.githooks`,
    inside `.git/hooks`, a `.git/config`, or a `config.worktree` under a
    `.git` directory."""
    comps = [c for c in _fold(path).split("/") if c]
    for i, c in enumerate(comps):
        if _comp_may_be(c, ".githooks", dotglob):
            return True
        if _comp_may_be(c, ".git", dotglob) and i + 1 < len(comps):
            if _comp_may_be(comps[i + 1], "hooks", dotglob):
                return True
            if _comp_may_be(comps[i + 1], "config", dotglob) and i + 2 == len(comps):
                return True
        if _comp_may_be(c, "config.worktree", dotglob) and any(
                _comp_may_be(x, ".git", dotglob) for x in comps[:i]):
            return True
    return False


# The text of a `$( … )` / backquote that prints a git directory: its output
# stands for a `.git` directory (`$(git rev-parse --git-common-dir)/hooks`).
_GITDIR_PRODUCER = re.compile(
    r"^\s*git\b.*\brev-parse\b.*--(?:absolute-git-dir|git-dir|git-common-dir)\b")
_GITPATH_PRODUCER = re.compile(r"^\s*git\b.*\brev-parse\b.*--git-path\s+(\S+)")
_QUOTED_SUBST = re.compile(r"\$\(([^()]*)\)|`([^`]*)`")


_GITDIR_SUBST = re.compile(r"\$\(([^()]*)\)")


def _gitdir_substs_to_literal(text: str) -> str:
    """Rewrite each `$( … )` that prints a git directory (`git rev-parse
    --git-dir` / `--git-common-dir` / `--absolute-git-dir` / `--git-path X`)
    as the literal `.git` / `.git/X`, BEFORE tokenizing. The tokenizer splits
    `$(…)/config` into two words, so the path suffix would otherwise be judged
    on its own; as `.git/config` the hook-machinery shape rule sees it. What
    the substitution runs is a read (rev-parse), so nothing is lost."""
    def sub(m: re.Match) -> str:
        g = _gitdir_text(m.group(1))
        return g if g is not None else m.group(0)

    return _GITDIR_SUBST.sub(sub, text)


def _gitdir_text(text: str) -> str | None:
    """`.git` (or `.git/<path>`) when `text` is a command printing a git
    directory, else None."""
    m = _GITPATH_PRODUCER.match(text)
    if m:
        return ".git/" + m.group(1).strip("'\"")
    if _GITDIR_PRODUCER.match(text):
        return ".git"
    return None


def _git_env_rewires(name: str, value: str) -> str | None:
    """A GIT_CONFIG_* environment assignment (parsed and quote-removed, value
    already expanded): the reason it can rewire core.hooksPath, or None."""
    unknown = any(ch in value for ch in "$`")
    # git dequotes GIT_CONFIG_PARAMETERS itself ('core.hooks''Path'): match
    # with its single quotes removed.
    value = value.replace("'", "")
    if re.fullmatch(r"GIT_CONFIG_KEY_\d+", name):
        if unknown:
            return f"{name} is set to a value this gate cannot determine"
        if value.strip().casefold() == HOOKS_KEY:
            return f"{name}={value} injects core.hooksPath through the environment"
    if name == "GIT_CONFIG_PARAMETERS":
        if unknown or HOOKS_KEY in value.casefold():
            return "GIT_CONFIG_PARAMETERS can carry core.hooksPath"
    return None


def _hookspath_assignment(kv: str) -> str | None:
    """`core.hooksPath=VALUE` (a `git -c` operand, already expanded): the
    reason it rewires the hooks, or None. A key this gate cannot read (an
    unknown variable) is undetermined and refused."""
    key, eq, val = kv.partition("=")
    if any(ch in key for ch in "$`"):
        return f"`-c {kv}` sets a config key this gate cannot determine"
    if key.strip().casefold() != HOOKS_KEY:
        return None
    if eq and val.strip() in SANCTIONED_HOOKS_PATHS:
        return None
    return f"`-c {kv}` points core.hooksPath away from .githooks for that command"


_GIT_GLOBAL_VALUE_OPTS = {"-C", "--git-dir", "--work-tree", "--namespace",
                          "--super-prefix", "--list-cmds", "--attr-source"}
_CONFIG_VALUE_OPTS = {"--type", "--default", "--file", "-f", "--blob",
                      "--comment", "--value"}
_CONFIG_READ_OPTS = {"--get", "--get-all", "--get-regexp", "--get-urlmatch",
                     "--list", "-l", "--get-color", "--get-colorbool"}
_CONFIG_VERBS = {"get", "list", "set", "unset", "rename-section",
                 "remove-section", "edit"}
# git subcommands that write working-tree paths named by a pathspec.
_GIT_TREE_WRITERS = {"rm", "mv", "checkout", "restore", "clean", "stash",
                     "apply", "am"}
# Their options that take a separate value (not a pathspec).
_GIT_SUB_VALUE_OPTS = {"-m", "--message", "-b", "-B", "--orphan", "-s",
                       "--source", "-e", "--exclude", "-p", "-C",
                       "--pathspec-from-file", "--conflict"}


def _git_rewires_hooks(args: list[str]) -> str | None:
    """For `git ARGS...`: the reason it rewires the repo's hook directory, or
    None. Covers `git -c core.hooksPath=X <any>`, `--config-env`, and `git
    config` setting / unsetting core.hooksPath (any scope, old or new verb
    syntax), removing / renaming the `core` section, or `--edit`. Setting it
    to `.githooks` (the repo's own opt-in) and every read form are allowed."""
    i = 0
    while i < len(args):
        a = args[i]
        if a == "-c" or (a.startswith("-c") and len(a) > 2 and "=" in a):
            glued = a != "-c"
            why = _hookspath_assignment(a[2:] if glued else (
                args[i + 1] if i + 1 < len(args) else ""))
            if why:
                return why
            i += 1 if glued else 2
            continue
        if a.startswith("--config-env"):
            spec = a.split("=", 1)[1] if "=" in a else (
                args[i + 1] if i + 1 < len(args) else "")
            key = spec.split("=", 1)[0]
            if any(ch in key for ch in "$`"):
                return "`--config-env` sets a config key this gate cannot determine"
            if key.strip().casefold() == HOOKS_KEY:
                return "`--config-env` sets core.hooksPath from the environment"
            i += 1 if "=" in a else 2
            continue
        if a in _GIT_GLOBAL_VALUE_OPTS:
            i += 2
            continue
        if a.startswith("-"):
            i += 1
            continue
        break
    if i >= len(args) or args[i] != "config":
        return None
    ops: list[str] = []
    flags: set[str] = set()
    rest = args[i + 1:]
    j = 0
    while j < len(rest):
        a = rest[j]
        if a in _CONFIG_VALUE_OPTS:
            j += 2
            continue
        if a.startswith("-"):
            flags.add(a.split("=", 1)[0])
        else:
            ops.append(a)
        j += 1
    if ops and any(ch in ops[0] for ch in "$`"):
        # The verb or key position holds a value this gate cannot read.
        if not (flags & _CONFIG_READ_OPTS):
            return "`git config` writes a key this gate cannot determine"
    verb = ops.pop(0) if ops and ops[0] in _CONFIG_VERBS else None
    if verb == "edit" or flags & {"--edit", "-e"}:
        return "`git config --edit` opens the repository config for arbitrary edits"
    if verb in ("rename-section", "remove-section") or flags & {
            "--rename-section", "--remove-section"}:
        if ops and ops[0].strip().casefold() == "core":
            return "it removes or renames the [core] section that holds core.hooksPath"
        return None
    reading = verb in ("get", "list") or bool(flags & _CONFIG_READ_OPTS)
    if ops and any(ch in ops[0] for ch in "$`") and not reading:
        return "`git config` writes a key this gate cannot determine"
    if not ops or ops[0].strip().casefold() != HOOKS_KEY:
        return None
    if reading:
        return None
    if verb == "unset" or flags & {"--unset", "--unset-all"}:
        return "it unsets core.hooksPath, so git falls back to .git/hooks"
    if len(ops) < 2:
        return None  # `git config core.hooksPath` reads the value
    if ops[1] in SANCTIONED_HOOKS_PATHS:
        return None
    return f"it sets core.hooksPath to `{ops[1]}` instead of .githooks"


_GIT_CONFIG_ENV = re.compile(
    r"\bGIT_CONFIG(?:_(?:PARAMETERS|COUNT|KEY_\d+|VALUE_\d+|GLOBAL|SYSTEM))?\b")


MAX_DEPTH = 6

REDIR_OUT = {">", ">>", ">|", "&>", "&>>", "<>"}
SEPARATORS = frozenset(
    {";", "&&", "||", "|", "&", ";;", ";&", ";;&", "|&", "(", ")", "<(", ">("}
)
_CHAIN_END = {";", ";;", ";&", ";;&", "&", None}

# Commands where every non-flag operand is a filesystem target that gets created,
# overwritten, or removed. Over-inclusive on purpose (a refusal is cheap).
TARGET_ALL = {
    "rm", "unlink", "rmdir", "mv", "cp", "tee", "touch", "mkdir",
    "grm", "gcp", "gmv", "ginstall", "gln", "gtouch", "gmkdir", "grmdir",
    "ln", "install", "truncate", "shred",
}
# The first operand is a mode/owner, the rest are targets.
TARGET_AFTER_FIRST = {"chmod", "chown", "chgrp"}
# The LAST operand is the destination.
TARGET_LAST = {"rsync", "scp", "ditto"}
# Programs whose targets `xargs` would feed from stdin.
STDIN_MUTATORS = TARGET_ALL | TARGET_AFTER_FIRST | {"sed", "gsed", "perl", "ruby"}

SHELLS = {"sh", "bash", "zsh", "dash", "ksh", "mksh", "fish", "busybox"}
AWKS = {"awk", "gawk", "nawk", "mawk"}
_PYTHON = re.compile(r"^(?:python[0-9.]*|pypy[0-9.]*)$")

# Words that precede a command without being it.
LEAD_KEYWORDS = {"!", "{", "}", "if", "then", "else", "elif", "while", "until",
                 "do", "fi", "done", "esac", "coproc"}
BLOCK_OPEN = {"if", "while", "until", "for", "select", "case"}
BLOCK_CLOSE = {"fi", "done", "esac"}

# Wrappers that run their operand as a command: {name: options taking an arg}.
WRAPPERS: dict[str, set[str]] = {
    "nohup": set(), "command": {"-p"}, "builtin": set(), "exec": {"-a"},
    "nice": {"-n"}, "sudo": {"-u", "-g", "-h", "-p", "-C", "-r", "-t", "-U", "-T", "-D"},
    "doas": {"-u", "-C"}, "timeout": {"-s", "-k"}, "gtimeout": {"-s", "-k"},
    "stdbuf": {"-i", "-o", "-e"}, "caffeinate": {"-w", "-t"}, "chronic": set(),
    "unbuffer": set(), "ionice": {"-c", "-n", "-p"}, "time": {"-f", "-o"},
    "xargs": {"-I", "-n", "-P", "-L", "-d", "-E", "-s", "-a"},
}
# Wrappers that run the command IN the current shell (state changes persist).
SAME_SHELL_WRAPPERS = {"command", "builtin", "exec", "time"}

# A mode-shaped string; only the fallback for a python payload that does not
# parse (see _py_open_for_write, which reads the actual open() mode argument).
_PY_MODE = re.compile(r"""['"](?:[rbtU]*[wax][rbt+]*|[rbt]*\+[rbt]*)['"]""")
# FILE-WRITE primitives per interpreter: a match means "this payload writes a
# file", so its literal paths are write targets, and a write that names no
# literal path at all cannot be placed (refused).
_WRITE = {
    "python": re.compile(
        r"\.write_(?:text|bytes)\s*\(|\.(?:touch|mkdir|unlink|rmdir|rename|symlink_to|"
        r"hardlink_to|chmod|extractall|extract)\s*\(|\b(?:os|shutil)\.(?:remove|unlink|"
        r"rename|replace|renames|makedirs|mkdir|rmdir|removedirs|symlink|link|truncate|"
        r"chmod|copy\w*|move|rmtree|make_archive|unpack_archive)\b|"
        r"\bsqlite3\.connect\b|\binplace\s*=\s*(?:True|1)\b"
    ),
    "node": re.compile(
        r"\b(?:writeFile|appendFile|createWriteStream|mkdir|mkdtemp|rmdir|rm|unlink|"
        r"rename|copyFile|cp|symlink|link|truncate|chmod|utimes|open)(?:Sync)?\s*\(|"
        r"\bBun\.write\b|\bDeno\.(?:write\w*|remove|mkdir|rename|create|copyFile|"
        r"symlink|link|truncate|chmod)\b"
    ),
    "perl": re.compile(
        r"""\bopen\b[^;]*?['"]\s*(?:\+?>|\+<)|['"]\s*(?:>>?|\+<)\s*['"]|"""
        r"\b(?:unlink|rename|mkdir|rmdir|symlink|link|truncate|chmod|chown|utime|"
        r"sysopen|make_path|mkpath|remove_tree|rmtree)\b|"
        r"""\b(?:copy|move|cp|mv)\s*(?:\(|['"$])"""
    ),
    "ruby": re.compile(
        r"\bFile\.(?:write|delete|unlink|rename|symlink|link|truncate|chmod|chown|"
        r"binwrite)\b|\bIO\.(?:write|binwrite|sysopen)\b|\bFileUtils\b|"
        r"""\b(?:File|IO)\.(?:open|new)\b[^;\n]*(?:['"][rb]*[wa+]|File::(?:WRONLY|RDWR|"""
        r"CREAT|APPEND|TRUNC))|"
        r"\bDir\.(?:mkdir|rmdir|delete|unlink)\b|"
        r"\bPathname\b[^;\n]*\.(?:write|binwrite|delete|unlink|rename|mkpath|mkdir|rmdir|"
        r"rmtree|make_symlink|make_link|truncate|chmod)\b"
    ),
}
# PROCESS-SPAWN primitives: they run another program, not a file write. A
# match sends the payload to _spawn_sites: a spawn whose command is a literal
# is judged with the shell rules; any other (UNPARSED) spawn makes every
# literal path in the payload a candidate (refused if one lands on main), and
# one that names no literal path is an unknown program, like make/cargo, and
# is allowed (residual).
_SPAWN = {
    "python": re.compile(
        r"\bos\.(?:system|popen|exec\w*|spawn\w*|posix_spawn\w*)\b|\bsubprocess\b|"
        r"\bpty\b"
    ),
    "node": re.compile(
        r"\bchild_process\b|\b(?:exec|execFile|spawn|fork)(?:Sync)?\s*\(|"
        r"\bBun\.spawn\w*|\bDeno\.(?:run|Command)\b"
    ),
    "perl": re.compile(r"""\b(?:system|exec|qx)\b|`|\bopen\b[^;]*?['"]\s*\||\|\s*['"]"""),
    "ruby": re.compile(r"\b(?:system|exec|spawn)\b|`|%x|\bOpen3\b|\bIO\.popen\b"),
}
# Mutation-capable WORDS. UNAMBIGUOUS ones count wherever they appear, with
# no module prefix required, so aliasing (`from os import remove`,
# `import os as o`, `__import__('os')`, getattr, `{writeFileSync: w}`,
# `fs['writeFileSync']`) cannot hide the call. AMBIGUOUS ones (`replace`,
# `write`, `copy`, `link`, `rm`, `cp`) are also ordinary string / stream
# methods (`s.replace(…)`, `sys.stdout.write(…)`, `re.findall('copy', …)`), so
# they count only where they are reached through os / shutil / pathlib / fs
# (see _ambiguous_mutation). A payload with a mutation word AND a literal path
# landing on main is refused; merely reading main stays allowed.
_UNAMBIG_MUTATION = re.compile(
    r"\b(?:remove|removedirs|unlink|rmtree|rmdir|rename|renames|symlink|chmod|chown|"
    r"truncate|mkdir|makedirs|copyfile|copytree|copy2|copymode|copystat|move|"
    r"write_text|write_bytes|writeFile|writeFileSync|appendFile|appendFileSync|"
    r"unlinkSync|rmSync|rmdirSync|renameSync|mkdirSync|copyFile|copyFileSync|cpSync|"
    r"symlinkSync|linkSync|chmodSync|chownSync|truncateSync|remove_tree|make_path|"
    r"mkpath|hardlink_to|symlink_to)\b"
)
_AMBIG = r"(?:replace|write|copy|link|rm|cp)\w*"


def _py_ambiguous_methods(code: str) -> bool | None:
    """Python method calls whose ARITY gives them away as filesystem calls,
    whatever the receiver is called (a parameter, a loop variable, …):
    `x.replace(target)` with ONE argument is Path.replace (str.replace needs
    two); `x.copy(…)` / `x.move(…)` / `x.link(…)` / `x.rm(…)` / `x.cp(…)` with
    an argument is a path / shutil-style copy (dict.copy and copy.copy are not
    reached this way: the first takes none, the second is the `copy` module).
    None if the source does not parse."""
    try:
        tree = ast.parse(code)
    except (SyntaxError, ValueError):
        return None
    for node in ast.walk(tree):
        if not (isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute)):
            continue
        nm = node.func.attr
        nargs = len(node.args) + len(node.keywords)
        if nm == "replace" and nargs == 1:
            return True
        if re.fullmatch(r"(?:copy|move|link|rm|cp)\w*", nm) and nargs >= 1:
            if getattr(node.func.value, "id", None) == "copy":
                continue
            return True
    return False


_PY_DYNAMIC = re.compile(
    r"__import__|\bgetattr\b|__dict__|\bvars\s*\(|\bglobals\s*\(|\blocals\s*\(|"
    r"\bimportlib\b|\bexec\s*\(|\beval\s*\(|\bcompile\s*\(|\bfrom\s+(?:os|shutil|"
    r"pathlib)\b[\w.]*\s+import\s+\*"
)
_JS_DYNAMIC = re.compile(
    r"\beval\s*\(|\bFunction\s*\(|\bimport\s*\(\s*[^'\"`\s]|\brequire\s*\(\s*[^'\"`\s]|"
    r"\bprocess\.binding\b"
)


def _ruby_writes(code: str) -> bool:
    """Ruby writes _WRITE cannot see within one statement: a method call on a
    variable bound to `Pathname(…)` / `Pathname.new(…)` earlier in the
    payload, and File/IO.open|new whose mode argument is not a read-only
    literal (a variable mode is a write, as in python)."""
    for m in re.finditer(r"\b(\w+)\s*=\s*Pathname(?:\.new)?\s*\(", code):
        if re.search(rf"\b{re.escape(m.group(1))}\s*\.\s*(?:write|binwrite|delete|unlink|"
                     r"rename|mkpath|mkdir|rmdir|rmtree|make_symlink|make_link|truncate|"
                     r"chmod|chown|utime)\b", code):
            return True
    return bool(re.search(
        r"""\b(?:File|IO)\.(?:open|new)\s*\(?\s*[^,()\n]+,"""
        r"""(?!\s*(?:['"]r[bt]?(?::[\w-]+)?['"]|File::RDONLY\b))""", code))


def _ambiguous_mutation(lang: str, code: str) -> bool:
    """An ambiguous mutation word reached through a filesystem module: an
    attribute of an alias bound to os / shutil / pathlib (python) or fs
    (node), of `Path(…)`, a name imported from one of them, a call whose
    arity only a filesystem method has (_py_ambiguous_methods), or a
    getattr / subscript / destructuring / eval that could hand one out by a
    name this scan cannot read (those count as mutation, 3.)."""
    if lang == "python":
        if _PY_DYNAMIC.search(code):
            return True
        by_arity = _py_ambiguous_methods(code)
        if by_arity is None:
            # Unparsable: fall back to the word itself anywhere.
            return bool(re.search(r"\b" + _AMBIG + r"\b", code))
        if by_arity:
            return True
        aliases = {"os", "shutil", "pathlib", "Path", "PurePath", "PosixPath"}
        for m in re.finditer(r"\bimport\s+([\w.]+(?:\s+as\s+\w+)?(?:\s*,\s*[\w.]+"
                             r"(?:\s+as\s+\w+)?)*)", code):
            for part in m.group(1).split(","):
                bits = part.split()
                if bits and bits[0].split(".")[0] in ("os", "shutil", "pathlib"):
                    aliases.add(bits[-1])
        for m in re.finditer(r"\bfrom\s+(os|shutil|pathlib)\b[\w.]*\s+import\s+"
                             r"\(?([\w\s,]+)", code):
            names = re.findall(r"\w+", m.group(2))
            if any(re.fullmatch(_AMBIG, n) for n in names):
                return True
            aliases.update(names)
        grew = True
        while grew:
            grew = False
            pairs = re.findall(r"\b(\w+)\s*=\s*\(?\s*(?:\w+\s*\.\s*)?(\w+)\b", code)
            pairs += re.findall(r"\bfor\s+(\w+)\s+in\s+[^:\]]*?\b(\w+)\b\s*[.(]", code)
            for new_name, src in pairs:
                if src in aliases and new_name not in aliases:
                    aliases.add(new_name)
                    grew = True
        names = "|".join(sorted(re.escape(a) for a in aliases))
        return bool(re.search(rf"\b(?:{names})\s*(?:\([^()]*\))?\s*\.\s*{_AMBIG}\b",
                              code))
    if lang == "node":
        if _JS_DYNAMIC.search(code):
            return True
        if re.search(r"\.\s*(?:cp|rm|link|copy|copyFile|symlink)(?:Sync)?\s*\(\s*[^)\s]",
                     code):
            # An fs-style copy / remove / link call, whatever the receiver is
            # named (exact fs method names: `copyWithin`, `rmdirAll`-style
            # names of other APIs are not matched).
            return True
        fs_mod = r"""['"`](?:node:)?fs(?:/promises|-extra)?['"`]"""
        if not re.search(fs_mod, code):
            return bool(re.search(rf"\bfs\s*(?:\.\s*promises\s*)?\.\s*{_AMBIG}", code))
        if re.search(rf"\{{[^}}]*\}}\s*=\s*(?:await\s+)?(?:require|import)\s*\(\s*{fs_mod}|"
                     rf"\bimport\s*\{{[^}}]*\}}\s*from\s*{fs_mod}", code) and re.search(
                         rf"\b{_AMBIG}\b", code):
            return True  # destructured from fs: a renamed binding may be one
        if re.search(rf"""\[\s*[^\]]*\]\s*\(""", code):
            return True  # a computed member call: the name cannot be read
        aliases = {"fs"}
        for m in re.finditer(rf"\b(\w+)\s*=\s*(?:await\s+)?(?:require|import)\s*\(\s*{fs_mod}"
                             rf"\s*\)(?!\s*\.\s*(?!promises\b))", code):
            aliases.add(m.group(1))
        for m in re.finditer(rf"\bimport\s+(?:\*\s+as\s+)?(\w+)\s+from\s+{fs_mod}", code):
            aliases.add(m.group(1))
        grew = True
        while grew:
            grew = False
            for new_name, src in re.findall(r"\b(\w+)\s*=\s*(\w+)\b", code):
                if src in aliases and new_name not in aliases:
                    aliases.add(new_name)
                    grew = True
        if re.search(rf"(?:require|import)\s*\(\s*{fs_mod}\s*\)\s*(?:\.\s*promises\s*)?"
                     rf"\.\s*{_AMBIG}", code):
            return True
        names = "|".join(sorted(re.escape(a) for a in aliases))
        return bool(re.search(rf"\b(?:{names})\s*(?:\.\s*promises\s*)?\.\s*{_AMBIG}",
                              code))
    return False


_PERL_REGEX = re.compile(
    r"\b(?:s|tr|y)(/)(?:[^/\\\n;]|\\.)*/(?:[^/\\\n;]|\\.)*/[a-z]*|"
    r"(?:(?<=[=!]~)|(?<=[(,;{!|&?:])|(?<=^)|\b(?:if|unless|and|or|not|split|grep|while|until)\b)"
    r"\s*(?:m|qr)?/(?:[^/\\\n;]|\\.)+/[a-z]*|"
    r"\b(?:m|qr)\s*\{[^{}\n;]*\}[a-z]*"
)


def _strip_perl_regex(code: str) -> str:
    """Perl source with its regex / substitution literals blanked, so the
    pattern text (`print if /copy/`) is not read as a call. Only `/…/` where
    perl itself expects a term (after `=~`, `(`, `,`, `if`, …) and that holds
    no `;` is a regex here; anything else is left in place. A literal that can
    RUN code — an `e` flag on s///, `(?{…})`, `@{[…]}` / `${…}`
    interpolation — is never blanked."""

    def blank(m: re.Match) -> str:
        t = m.group(0)
        flags = re.search(r"[a-z]*$", t).group(0)
        if "e" in flags or any(x in t for x in ("(?{", "(??{", "@{", "${")):
            return t
        return " "

    return _PERL_REGEX.sub(blank, code)


def _py_open_for_write(code: str) -> bool:
    """An `open(…)` (builtin, io/codecs, Path.open, os.fdopen, io.FileIO)
    given a mode with w / a / x / +, a non-literal mode, or **kwargs; or
    os.open with write flags. A mode-shaped string elsewhere in the payload
    (`s.replace('a', 'b')`) is not one. Unparsable source falls back to
    "open( and any mode-shaped string"."""
    if re.search(r"\bO_(?:WRONLY|RDWR|CREAT|APPEND|TRUNC)\b", code):
        return True
    if not re.search(r"\b(?:fd)?open\s*\(|\bFileIO\b", code):
        return False
    try:
        tree = ast.parse(code)
    except (SyntaxError, ValueError):
        return bool(_PY_MODE.search(code)) or "FileIO" in code
    mode_re = re.compile(r"[rbtU]*[wax][rbtU+]*|[rbtU]*\+[rbtU]*")
    for node in ast.walk(tree):
        if not isinstance(node, ast.Call):
            continue
        f = node.func
        nm = f.id if isinstance(f, ast.Name) else f.attr if isinstance(f, ast.Attribute) else None
        if nm not in ("open", "fdopen", "FileIO"):
            continue
        attr = isinstance(f, ast.Attribute)
        owner = getattr(f.value, "id", None) if attr else None
        if nm == "open" and owner == "os":
            continue  # os.open: its write flags are the O_* check above
        if any(k.arg is None for k in node.keywords):
            return True  # **kwargs: the mode cannot be read
        modes = [k.value for k in node.keywords if k.arg == "mode"]
        # builtin / io / codecs open(file, mode), os.fdopen(fd, mode),
        # FileIO(file, mode); a method `x.open(mode)` (Path.open) takes it first.
        pos = 0 if (attr and nm == "open" and owner not in ("io", "codecs")) else 1
        if len(node.args) > pos:
            modes.append(node.args[pos])
        for mv in modes:
            if not (isinstance(mv, ast.Constant) and isinstance(mv.value, str)):
                return True
            if mode_re.fullmatch(mv.value):
                return True
    return False


_PY_SPAWN_CALLS = {"run", "call", "check_call", "check_output", "Popen", "getoutput",
                   "getstatusoutput", "system", "popen"}
_PY_SPAWN_NAMES = _PY_SPAWN_CALLS | {"pty", "spawn", "posix_spawn", "posix_spawnp"}
_PY_SPAWN_RE = re.compile(r"^(?:exec|spawn)\w*$")


def _python_spawns(code: str) -> tuple[list, bool]:
    """(sites, unparsed) for a python payload. A site is ("sh", text) or
    ("argv", [words]) for a subprocess / os.system / os.popen CALL whose first
    argument is a literal string or a literal list of strings and which passes
    no cwd / executable / env / **kwargs. Every other reference to a spawn
    name (an alias, an os.exec*, an f-string, a getattr string) makes the
    payload unparsed."""
    try:
        tree = ast.parse(code)
    except (SyntaxError, ValueError):
        return [], True
    sites: list = []
    unparsed = False
    funcs: set[int] = set()

    def name_of(node) -> str | None:
        if isinstance(node, ast.Name):
            return node.id
        if isinstance(node, ast.Attribute):
            return node.attr
        return None

    for node in ast.walk(tree):
        if isinstance(node, ast.Call):
            nm = name_of(node.func)
            if nm in _PY_SPAWN_CALLS:
                funcs.add(id(node.func))
                ok = bool(node.args) and not any(
                    k.arg in (None, "cwd", "executable", "env", "args", "preexec_fn")
                    for k in node.keywords)
                if ok:
                    a0 = node.args[0]
                    if isinstance(a0, ast.Constant) and isinstance(a0.value, str):
                        sites.append(("sh", a0.value))
                    elif isinstance(a0, (ast.List, ast.Tuple)) and a0.elts and all(
                            isinstance(e, ast.Constant) and isinstance(e.value, str)
                            for e in a0.elts):
                        sites.append(("argv", [e.value for e in a0.elts]))
                    else:
                        ok = False
                if not ok:
                    unparsed = True
    for node in ast.walk(tree):
        # Any other mention of a spawn name: an alias (`r = subprocess.run`),
        # an import of it, os.exec*/spawn*, a getattr string.
        names: list[str] = []
        if isinstance(node, (ast.Name, ast.Attribute)) and id(node) not in funcs:
            names = [name_of(node) or ""]
        elif isinstance(node, ast.Constant) and isinstance(node.value, str):
            names = [node.value]
        elif isinstance(node, ast.alias):
            names = [node.name.split(".")[-1], node.asname or ""]
        if any(n in _PY_SPAWN_NAMES or _PY_SPAWN_RE.match(n) for n in names if n):
            unparsed = True
    return sites, unparsed


_JS_STR = r"""(?:'([^'\\\n]*)'|"([^"\\\n]*)"|`([^`\\\n$]*)`)"""
_JS_OPTS = r"(?:\s*,\s*\{(?![^{}]*\b(?:cwd|env|shell|argv0)\b)[^{}]*\})?"


def _node_spawns(code: str) -> tuple[list, bool]:
    if re.search(r"\bBun\.spawn|\bDeno\.|\bfork\b|\bworker_threads\b", code):
        return [], True
    sites: list = []
    unparsed = False
    for m in re.finditer(r"\b(exec|execSync|execFile|execFileSync|spawn|spawnSync)\b", code):
        tail = code[m.end():]
        if m.group(1) in ("exec", "execSync"):
            c = re.match(rf"\s*\(\s*{_JS_STR}{_JS_OPTS}\s*\)", tail)
            if c:
                sites.append(("sh", c.group(1) or c.group(2) or c.group(3) or ""))
                continue
        else:
            c = re.match(rf"\s*\(\s*{_JS_STR}\s*(?:,\s*\[([^\]]*)\])?{_JS_OPTS}\s*\)",
                         tail)
            if c:
                words = [c.group(1) or c.group(2) or c.group(3) or ""]
                lst = c.group(4)
                if lst is not None and lst.strip():
                    items = [x.strip() for x in lst.split(",") if x.strip()]
                    vals = [re.fullmatch(_JS_STR, x) for x in items]
                    if not all(vals):
                        unparsed = True
                        continue
                    words += [v.group(1) or v.group(2) or v.group(3) or "" for v in vals]
                sites.append(("argv", words))
                continue
        unparsed = True
    return sites, unparsed


_RB_STR = r"""(?:'([^'\\\n]*)'|"([^"\\\n$@#]*)")"""


def _perl_ruby_spawns(lang: str, code: str) -> tuple[list, bool]:
    if lang == "perl" and re.search(r"""\bopen\b[^;]*?['"]\s*\||\|\s*['"]""", code):
        return [], True
    if lang == "ruby" and re.search(r"\bOpen3\b|\bIO\.popen\b|\bspawn\b|\bPTY\b", code):
        return [], True
    sites: list = []
    unparsed = False
    interp = "$@" if lang == "perl" else "#"
    i = 0
    while True:
        j = code.find("`", i)
        if j < 0:
            break
        k = code.find("`", j + 1)
        if k < 0:
            return sites, True
        body = code[j + 1 : k]
        if any(c in body for c in interp):
            unparsed = True
        else:
            sites.append(("sh", body))
        i = k + 1
    for m in re.finditer(r"\b(?:qx|%x)\s*([({\[/|!])", code):
        close = {"(": ")", "{": "}", "[": "]"}.get(m.group(1), m.group(1))
        k = code.find(close, m.end())
        body = code[m.end() : k] if k >= 0 else None
        if body is None or any(c in body for c in interp) or m.group(1) in body:
            unparsed = True
        else:
            sites.append(("sh", body))
    for m in re.finditer(r"\b(?:system|exec)\b", code):
        c = re.match(rf"\s*\(?\s*({_RB_STR}(?:\s*,\s*{_RB_STR})*)\s*\)?\s*(?:;|$|\bif\b|\bor\b|\band\b|\|\||&&)",
                     code[m.end():])
        if not c:
            unparsed = True
            continue
        words = [a or b for a, b in re.findall(_RB_STR, c.group(1))]
        sites.append(("sh", words[0]) if len(words) == 1 else ("argv", words))
    return sites, unparsed


_MOVES_CONTEXT = re.compile(
    r"\bchdir\b|\benviron\b|\bputenv\b|%ENV|\bENV\s*\[|\bprocess\.env\b|\bfchdir\b"
)


def _spawn_sites(lang: str, code: str) -> tuple[list, bool]:
    if lang == "python":
        return _python_spawns(code)
    if lang == "node":
        return _node_spawns(code)
    return _perl_ruby_spawns(lang, code)


def _awk_effects(program: str) -> tuple[bool, bool]:
    """(writes, spawns) for an awk program, judged on its SYNTAX rather than on
    any `>`/`|` character: string literals are blanked first, and only a `>`,
    `>>` or `|` at parenthesis depth 0 inside a print/printf statement is an
    output redirection (`print ($1 > 3)` is a comparison). `system(…)` and
    `"cmd" | getline` spawn a program."""
    sk = re.sub(r'"(?:[^"\\\n]|\\.)*"', '""', program)
    spawns = bool(re.search(r"\bsystem\s*\(|\|\s*getline\b", sk))
    writes = False
    for m in re.finditer(r"\bprintf?\b", sk):
        depth = 0
        i = m.end()
        while i < len(sk) and not (depth == 0 and sk[i] in ";}\n"):
            ch = sk[i]
            if ch == "(":
                depth += 1
            elif ch == ")":
                depth = max(0, depth - 1)
            elif depth == 0 and ch == ">":
                writes = True
            elif depth == 0 and ch == "|":
                if sk[i + 1 : i + 2] == "|" or sk[i - 1 : i] == "|":
                    i += 1  # `||` is a logical or
                else:
                    spawns = True
            i += 1
    return writes, spawns


_ABS_LIT = re.compile(r"(?<![\w.:/~}$\\-])/[\w.@%+=,:~/-]+")
_VAR_LIT = re.compile(r"(?<![\w$])(?:~|\$\{?[A-Za-z_][A-Za-z0-9_]*\}?)/[\w.@%+=,:~/${}-]*")
_STR_LIT = re.compile(r"""'([^'\n]*)'|"([^"\n]*)"|`([^`\n]*)`""")
_REL_PATH = re.compile(r"^(?:\.{1,2}/)?[\w.@+-]+(?:/[\w.@+-]+)*/?$")
_HAS_EXT = re.compile(r"\.[A-Za-z][A-Za-z0-9]{0,7}$")


def _payload_paths(code: str) -> list[str]:
    """Every literal in an interpreter payload that could name a filesystem
    path: absolute paths and `~/…`/`$VAR/…` anywhere in the text (including
    inside a string handed to os.system), plus quoted relative names that carry
    a `/` or a file extension."""
    out = _ABS_LIT.findall(code) + _VAR_LIT.findall(code)
    for m in _STR_LIT.finditer(code):
        s = (m.group(1) or m.group(2) or m.group(3) or "").strip()
        if s and _REL_PATH.match(s) and ("/" in s or _HAS_EXT.search(s)):
            out.append(s)
    return out


def _quoted_substitutions(word: str) -> list[str]:
    """Bodies of `$( … )` and `` `…` `` left inside one token (they were quoted,
    so the tokenizer did not split them out). Unbalanced -> the rest of the
    word, which then fails to tokenize and is refused."""
    out: list[str] = []
    i = 0
    while i < len(word):
        if word.startswith("$(", i) and not word.startswith("$((", i):
            depth, j = 1, i + 2
            while j < len(word) and depth:
                depth += {"(": 1, ")": -1}.get(word[j], 0)
                j += 1
            out.append(word[i + 2 : j - 1] if depth == 0 else word[i + 2 :])
            i = j
        elif word[i] == "`":
            j = word.find("`", i + 1)
            if j < 0:
                break  # a lone backquote from an unquoted `a b` split: not ours
            out.append(word[i + 1 : j])
            i = j + 1
        else:
            i += 1
    return [s for s in out if s.strip()]


_USER_TMP: list = []


def _darwin_user_temp_dir() -> str | None:
    """macOS `confstr(_CS_DARWIN_USER_TEMP_DIR)`, the directory BSD
    `mktemp -t` creates in (not $TMPDIR). None off darwin or if unreadable."""
    if sys.platform != "darwin":
        return None
    if not _USER_TMP:
        try:
            r = subprocess.run(("getconf", "DARWIN_USER_TEMP_DIR"),
                               capture_output=True, text=True, timeout=5)
            val = r.stdout.strip() if r.returncode == 0 else ""
        except (OSError, subprocess.SubprocessError):
            val = ""
        _USER_TMP.append(os.path.realpath(val) if val and os.path.isabs(val) else None)
    return _USER_TMP[0]


def _set_positional(st: _State, args: list[str]) -> None:
    """`set -- A B`, `sh -c '…' _ A B`: $1 $2 … and "$@" / "$*" (a literal
    list joined by spaces, so `"$@"` as the program word-splits like
    `$CMD`)."""
    for k in [k for k in st.vars if k.isdigit() and k != "0"] + ["@", "*"]:
        st.vars.pop(k, None)
    for i, a in enumerate(args, 1):
        st.vars[str(i)] = a
    st.vars["@"] = st.vars["*"] = " ".join(args)


def _mktemp_value(inner: list[str], st: _State, root: str,
                  darwin: bool = sys.platform == "darwin") -> str | None:
    """The path `mktemp …` creates and prints, as `<dir>/` + MKTEMP_MARK, when
    `inner` is exactly one mktemp call whose directory is known; else None.

      * a template with a directory part: that directory, taken relative to
        `-p DIR` / `--tmpdir[=DIR]` when given, else to the cwd (an absolute
        template stands alone); any `..` component -> None;
      * `-t PREFIX` (BSD, darwin): the per-user confstr temp dir (not
        TMPDIR); a PREFIX holding `/` or `..` -> None. Elsewhere (GNU) `-t`
        means "under TMPDIR";
      * otherwise `-p DIR` / `--tmpdir=DIR`; else, on darwin, the per-user
        confstr temp dir (observed: BSD mktemp with no template ignores
        TMPDIR); else TMPDIR (tracked; the hook's own when absolute and
        outside main), else /tmp.
    `darwin=False` gives the GNU / TMPDIR reading of the same call.
    A base that cannot be expanded -> None."""
    if not inner or os.path.basename(inner[0]) not in ("mktemp", "gmktemp"):
        return None
    if any(t in SEPARATORS or t in REDIR_OUT for t in inner):
        return None
    tmpdir = st.vars.get("TMPDIR") or "/tmp"
    explicit: str | None = None
    tflag = False
    prefix: str | None = None
    template: str | None = None
    j = 1
    while j < len(inner):
        a = _expand(inner[j], st)
        j += 1
        if a == "-p" and j < len(inner):
            explicit = _expand(inner[j], st)
            j += 1
        elif a.startswith("-p") and len(a) > 2:
            explicit = a[2:]
        elif a.startswith("--tmpdir"):
            explicit = a.split("=", 1)[1] if "=" in a else tmpdir
        elif a == "-t":
            tflag = True
            if j < len(inner) and not inner[j].startswith("-"):
                prefix = _expand(inner[j], st)
                j += 1
        elif a.startswith("-") and len(a) > 1 and not a.startswith("--"):
            if "p" in a[1:] and j < len(inner):  # `-dp DIR`
                explicit = _expand(inner[j], st)
                j += 1
            tflag = tflag or "t" in a[1:]
        elif a.startswith("-"):
            continue
        else:
            template = a
    if tflag and darwin:
        if prefix is not None and ("/" in prefix or ".." in prefix):
            return None
        base = _darwin_user_temp_dir() or tmpdir
    elif template is not None and "/" in template:
        if ".." in template.split("/"):
            return None
        tdir = os.path.dirname(template)
        if os.path.isabs(template):
            base = tdir
        elif explicit is not None:
            base = os.path.join(explicit, tdir)
        elif tflag:
            base = os.path.join(tmpdir, tdir)
        elif st.rel is not None:
            base = os.path.join(st.rel, tdir)
        else:
            return None
    elif template is not None and explicit is None and not tflag:
        if st.rel is None:
            return None
        base = st.rel  # a bare relative template: the cwd
    elif explicit is not None:
        base = explicit
    else:
        base = (_darwin_user_temp_dir() if darwin else None) or tmpdir
    if not base or any(c in base for c in _UNRESOLVABLE) or not os.path.isabs(base):
        return None
    return os.path.normpath(base) + "/" + MKTEMP_MARK


def _replace_quoted_mktemp(word: str, st: _State, root: str) -> str:
    if "$(" not in word:
        return word
    for inner in _quoted_substitutions(word):
        try:
            toks = shlex.split(inner)
        except ValueError:
            continue
        made = _mktemp_value(toks, st, root)
        if made is not None:
            word = word.replace("$(" + inner + ")", made)
    return word


# Options that take a separate value, for the tools whose destination is the
# LAST operand. rsync (popt) and scp accept options after the operands, so the
# value of such an option must not be taken for the destination.
_VALUE_OPTS = {
    "rsync": ({"e", "f", "T", "B", "M"}, {
        "--rsh", "--exclude", "--include", "--filter", "--exclude-from",
        "--include-from", "--files-from", "--rsync-path", "--log-file",
        "--log-file-format", "--partial-dir", "--temp-dir", "--backup-dir",
        "--suffix", "--chmod", "--chown", "--usermap", "--groupmap",
        "--compare-dest", "--copy-dest", "--link-dest", "--timeout",
        "--contimeout", "--port", "--bwlimit", "--max-size", "--min-size",
        "--block-size", "--password-file", "--out-format", "--sockopts",
        "--skip-compress", "--iconv", "--protocol", "--checksum-seed",
        "--modify-window", "--max-delete", "--info", "--debug", "--outbuf",
        "--remote-option", "--address", "--max-alloc", "--stop-after",
        "--stop-at", "--write-batch", "--only-write-batch", "--read-batch",
        "--early-input", "--compress-choice", "--checksum-choice",
        "--compress-level", "--zc", "--zl", "--cc", "--old-args",
    }),
    "scp": ({"i", "F", "o", "P", "c", "l", "S", "J", "D", "X"}, set()),
}


def _copy_operands(prog: str, args: list[str]) -> list[str]:
    """Non-option operands of rsync / scp / ditto, skipping every option and
    the value of an option that takes one, wherever they appear (`rsync -a SRC
    DST --exclude zz` -> [SRC, DST]). ditto stops at the first operand
    (BSD getopt) and is read with _operands."""
    if prog not in _VALUE_OPTS:
        return _operands(args)
    short, long_ = _VALUE_OPTS[prog]
    out: list[str] = []
    j = 0
    while j < len(args):
        a = args[j]
        j += 1
        if a == "--":
            out += args[j:]
            break
        if a.startswith("--"):
            if "=" not in a and a in long_:
                j += 1
            continue
        if a.startswith("-") and len(a) > 1:
            for q, ch in enumerate(a[1:], 1):
                if ch in short:
                    if q == len(a) - 1:
                        j += 1  # the value is the next word
                    break  # the rest of the bundle is the value
            continue
        out.append(a)
    return out


def _operands(args: list[str]) -> list[str]:
    return [a for a in args if not a.startswith("-")]


class _Analyzer:
    def __init__(self, root: str, guard_main: bool = True):
        self.root = root
        # False = hooks-only pass: there is no main tree to guard from here
        # (the project anchor is a linked worktree or not a repo), but writes
        # into hook machinery are still refused (e033c406).
        self.guard_main = guard_main
        # The resolved absolute path of the target that raised _Hit, when it
        # could be resolved (for the deny ledger); None otherwise.
        self.hit_abs: str | None = None
        self._own: tuple[bool, str | None] = (False, None)
        # marker word -> the command text of an unquoted `$( … )` it replaced
        self.substs: dict[str, str] = {}

    # -- target judgement ---------------------------------------------------
    def own_gitdir(self) -> str | None:
        if not self._own[0]:
            self._own = (True, _own_worktree_gitdir(self.root))
        return self._own[1]

    def check(self, target: str, st: _State) -> None:
        if target == "-":
            return  # stdout
        self.check_hooks(target, st)
        if not self.guard_main:
            return
        if _hits_main(self.root, target, self.own_gitdir(), st):
            self.hit_abs = _abs_target(target, st, self.root)
            raise _Hit(target)

    def check_hooks(self, target: str, st: _State) -> None:
        """Refuse a write into hook machinery in any tree (e033c406): the
        target as written (after expansion), then its resolved literal part."""
        p = _expand(self.gitdir_substs(target), st)
        if _hook_protected(p):
            self.hit_abs = _abs_target(target, st, self.root)
            raise _HookHit(target, "it writes into the repository's hook machinery")
        if any(c in p for c in _UNRESOLVABLE):
            p = _literal_prefix(p)
        if not os.path.isabs(p) and st.rel is None:
            return  # relative to an unknown cwd: main mode refuses it anyway
        resolved = _resolve(st.rel or "/", p) if p else (st.rel or "")
        if resolved and _hook_protected(resolved):
            self.hit_abs = _abs_target(target, st, self.root)
            raise _HookHit(target, "it writes into the repository's hook machinery")

    def gitdir_substs(self, word: str) -> str:
        """`word` with each substitution that prints a git directory (`$(git
        rev-parse --git-dir)`, `--git-common-dir`, `--absolute-git-dir`,
        `--git-path X`), quoted or already replaced by a marker, rewritten to
        `.git` / `.git/X`, so the hook-machinery shape rule sees it."""
        for marker, text in self.substs.items():
            if marker in word:
                g = _gitdir_text(text)
                if g is not None:
                    word = word.replace(marker, g)

        def sub(m: re.Match) -> str:
            g = _gitdir_text(m.group(1) if m.group(1) is not None else m.group(2))
            return g if g is not None else m.group(0)

        return _QUOTED_SUBST.sub(sub, word)

    def check_git_pathspec(self, spec: str, st: _State) -> None:
        """A pathspec of a git subcommand that writes the working tree: refuse
        one that names, lies under, or (as a git glob, whose `*` crosses `/`
        and leading dots) can match `.githooks` or `.git/hooks`."""
        if spec.startswith(":(") and ")" in spec:
            magic, spec = spec[2:spec.index(")")], spec[spec.index(")") + 1:]
            if "exclude" in magic:
                return
        elif spec.startswith((":!", ":^")):
            return  # an exclusion writes nothing
        elif spec.startswith(":/"):
            spec = spec[2:]
        self.check_hooks(spec, st)
        p = _expand(self.gitdir_substs(spec), st)
        if _hook_protected(p, dotglob=True) or (any(ch in p for ch in _GLOB) and any(
                fnmatch.fnmatchcase(rep, _fold(p))
                for rep in (".githooks", ".githooks/x", ".git/hooks/x"))):
            raise _HookHit(spec, "a git pathspec writes into the repository's "
                                 "hook machinery")

    # -- text ---------------------------------------------------------------
    def analyze(self, text: str, st: _State, depth: int) -> _State:
        """Judge a whole command text; return the shell state it leaves."""
        if depth > MAX_DEPTH:
            raise _Undet(f"command wrappers nested more than {MAX_DEPTH} levels")
        stripped, bodies = _strip_heredoc_bodies(text)
        tokens = _tokenize(_gitdir_substs_to_literal(_backquotes_to_subst(stripped)))
        if tokens is None:
            if depth == 0:
                raise _Unparseable()
            raise _Undet("a nested command payload does not tokenize")
        n_here = sum(1 for t in tokens if t == "<<")
        if n_here != len(bodies):
            # Could not line bodies up with their operators; a body that is
            # fed to an interpreter then cannot be read (judged in _stdin_code).
            bodies = [None] * n_here
        return _Walk(self, st, bodies, depth).run(tokens)


class _Walk:
    """One pass over a token stream, tracking cwd and variables segment by
    segment and judging each simple command's effect."""

    def __init__(self, an: _Analyzer, st: _State, bodies: list, depth: int):
        self.an = an
        self.st = st
        self.bodies = bodies
        self.here = 0
        self.depth = depth
        # State that holds instead if the current `&&`/`||` chain stops early.
        self.fallback: _State | None = None
        self.blocks: list[_State] = []
        self.find_cont: list[str] | None = None

    # -- driver -------------------------------------------------------------
    def run(self, tokens: list[str]) -> _State:
        stack: list[tuple] = []
        cur: list[str] = []
        prev: str | None = None
        for idx, tok in enumerate(tokens + [None]):
            if tok is not None and tok not in SEPARATORS:
                cur.append(tok)
                continue
            if tok in ("(", ")") and cur and os.path.basename(cur[0]) == "find":
                cur.append(tok)  # `find … \( … \)`: an argument, not syntax
                continue
            if tok in ("(", "<(", ">("):
                subst = tok == "(" and bool(cur) and cur[-1].endswith("$")
                if subst or tok != "(":
                    # `$( … )` / `<( … )`: judged on its own, continues the
                    # enclosing command with an unresolvable word in its place.
                    marker = f"{SUBST_WORD}{len(self.an.substs)}__"
                    self.an.substs[marker] = ""
                    if subst:
                        cur[-1] = cur[-1][:-1] + marker
                    else:
                        cur.append(marker)
                    stack.append(("cont", cur, self.st.copy(), prev, self.fallback,
                                  idx, marker, subst))
                else:
                    if cur:
                        self.segment(cur, prev, tok)
                    stack.append(("sub", None, self.st.copy(), prev, self.fallback,
                                  idx, None, False))
                cur, prev, self.fallback = [], None, None
                continue
            if tok == ")":
                if cur:
                    self.segment(cur, prev, None)
                self.end_chain()
                cur = []
                if stack:
                    kind, saved, st, prev, fb, start, marker, subst = stack.pop()
                    # A subshell / substitution's cwd and variables do not leak.
                    self.st, self.fallback = st, fb
                    if kind == "cont":
                        cur = saved
                        inner = tokens[start + 1 : idx]
                        self.an.substs[marker] = " ".join(inner)
                        made = _mktemp_value(inner, self.st, self.an.root) if subst else None
                        if made is not None:
                            cur[:] = [w.replace(marker, made) for w in cur]
                continue
            if cur:
                self.segment(cur, prev, tok)
            cur = []
            if tok in _CHAIN_END:
                self.end_chain()
            prev = tok
        while self.blocks:  # an unterminated block: either path may have held
            self.st = _merge(self.st, self.blocks.pop())
        return self.st

    def end_chain(self) -> None:
        if self.fallback is not None:
            self.st = _merge(self.st, self.fallback)
            self.fallback = None

    # -- one simple command -------------------------------------------------
    def segment(self, seg: list[str], prev: str | None, nxt: str | None) -> None:
        if prev == "||" and self.fallback is not None:
            # Runs only if something earlier in the chain FAILED, so it may see
            # the state from before that something.
            self.st = _merge(self.st, self.fallback)
        pre = self.st
        # A `$( … )` or backquote INSIDE a quoted word reaches here as part of
        # one token; its command still runs, so judge it (in a subshell state).
        for w in seg:
            for inner in _quoted_substitutions(w):
                self.an.analyze(inner, pre.copy(), self.depth + 1)
        seg = [_replace_quoted_mktemp(w, pre, self.an.root) for w in seg]
        argv, stdin = self.redirects(seg, pre)

        # Compound-command keywords: track blocks, then judge what follows.
        while argv and argv[0] in LEAD_KEYWORDS:
            kw = argv.pop(0)
            if kw in ("if", "while", "until"):
                self.blocks.append(self.st.copy())
            elif kw in BLOCK_CLOSE and self.blocks:
                self.st = _merge(self.st, self.blocks.pop())
            elif kw in ("else", "elif") and self.blocks:
                self.st = _merge(self.st, self.blocks[-1])
        if argv and argv[0] in ("for", "select", "case"):
            self.blocks.append(self.st.copy())
        if not argv:
            return

        if self.find_cont is not None and argv[0].startswith("-"):
            # The tail of `find … -exec cmd {} \; -delete` after the `;`.
            self.find_expr(self.find_cont, argv, self.st)
        self.find_cont = None

        pre = self.st
        new = self.judge(argv, pre, stdin, nxt)
        if new is None:
            return
        if prev in ("|", "|&") or nxt in ("|", "|&", "&"):
            return  # a pipeline stage / background job runs in a subshell
        if prev in ("&&", "||") or nxt == "||":
            # Conditional (or followed by a branch that runs only if it failed):
            # whatever comes later may still see the state from before it.
            self.fallback = pre if self.fallback is None else _merge(self.fallback, pre)
        self.st = new

    def redirects(self, seg: list[str], st: _State) -> tuple[list[str], tuple | None]:
        argv: list[str] = []
        stdin: tuple | None = None
        i = 0
        while i < len(seg):
            t = seg[i]
            nxt = seg[i + 1] if i + 1 < len(seg) else None
            if t in REDIR_OUT:
                if nxt is not None:
                    self.an.check(nxt, st)
                i += 2
                continue
            if t == ">&":
                if nxt is not None and not (nxt.isdigit() or nxt == "-"):
                    self.an.check(nxt, st)  # `>& file` is `&> file`
                i += 2
                continue
            if t in ("<", "<&"):
                if t == "<" and nxt is not None:
                    stdin = ("file", nxt)
                i += 2
                continue
            if t == "<<":
                body = self.bodies[self.here] if self.here < len(self.bodies) else None
                self.here += 1
                stdin = ("code", body)
                i += 2
                continue
            if t == "<<<":
                stdin = ("code", nxt)
                i += 2
                continue
            argv.append(t)
            i += 1
        return argv, stdin

    # -- dispatch ------------------------------------------------------------
    def judge(self, argv: list[str], st: _State, stdin: tuple | None,
              nxt: str | None = None) -> _State | None:
        """Judge one simple command. Returns the new shell state if the command
        changes it (cd, assignment), else None."""
        k = 0
        while k < len(argv) and _ASSIGN.match(argv[k]):
            k += 1
        if k == len(argv):
            new = st.copy()
            for a in argv:
                m = _ASSIGN.match(a)
                assert m is not None
                new.vars[m.group(1)] = _expand(m.group(2), new)
                self.git_env(m.group(1), new.vars[m.group(1)])
            return new
        env_st = st
        if k:
            # Prefix assignments only reach the command's ENVIRONMENT (mktemp
            # reads TMPDIR from it), not the expansion of its own arguments.
            env_st = st.copy()
            for a in argv[:k]:
                m = _ASSIGN.match(a)
                assert m is not None
                env_st.vars[m.group(1)] = _expand(m.group(2), st)
                self.git_env(m.group(1), env_st.vars[m.group(1)])
        argv = argv[k:]

        word = _expand(argv[0].lstrip("`"), st)
        if "$" in argv[0] and any(c in word for c in " \t\n"):
            # `CMD="rm <path>"; $CMD`: an unquoted expansion word-splits, so
            # the value's words ARE the command.
            return self.judge(word.split() + argv[1:], st, stdin, nxt)
        if any(c in word for c in "$`"):
            self.subst_literals(argv[0], st)
            # A variable carrying a producer's text (`CMD=$(…)`, `printf -v`,
            # `read … <<<`): the producer's literal paths are judged too.
            self.subst_literals(word, st)
            # An unknown PROGRAM (`$CMD`, `$(echo rm)`, a backquoted command):
            # it could be any tool, `rm` included, so every operand it is
            # handed is judged as a possible write target — a literal path in
            # the substitution that produced it (above) or a non-option
            # argument that lands on main is refused. With no such operand it
            # is the same class as make/cargo/an arbitrary binary, which this
            # hook cannot see into (residual).
            for a in argv[1:]:
                a = a.strip("`")
                if a and not a.startswith("-"):
                    self.an.check(a, st)
            return None
        prog = os.path.basename(word)
        rest = argv[1:]

        if prog in ("cd", "pushd"):
            return self.cd(rest, st, nxt)
        if prog == "popd":
            return _State(None, None, dict(st.vars))
        if prog in ("export", "declare", "typeset", "local", "readonly"):
            new = st.copy()
            for a in rest:
                m = _ASSIGN.match(a)
                if m:
                    new.vars[m.group(1)] = _expand(m.group(2), new)
                    self.git_env(m.group(1), new.vars[m.group(1)])
            return new
        if prog == "unset":
            new = st.copy()
            for a in _operands(rest):
                new.vars[a] = ""
            return new
        if prog in ("for", "select"):
            return self.loop_var(rest, st)
        if prog in ("read", "mapfile", "readarray", "getopts"):
            new = st.copy()
            # A here-string is the literal the variable is filled from: keep
            # it as the variable's producer text.
            src = stdin[1] if (stdin and stdin[0] == "code" and stdin[1]) else None
            names = [a for a in _operands(rest) if re.match(r"^[A-Za-z_][A-Za-z0-9_]*$", a)]
            plain = prog == "read" and not any(a.startswith("-a") for a in rest)
            for a in names:
                if src is None:
                    new.vars[a] = UNKNOWN_VAL
                elif plain and len(names) == 1 and "\n" not in src.strip():
                    # `read V <<< 'text'`: V is exactly that one line.
                    new.vars[a] = _expand(src.strip(), st)
                else:
                    new.vars[a] = self.producer(src)
            return new
        if prog == "printf" and rest[:1] == ["-v"] and len(rest) > 1:
            new = st.copy()
            if re.match(r"^[A-Za-z_][A-Za-z0-9_]*$", rest[1]):
                new.vars[rest[1]] = self.producer(
                    " ".join(_expand(a, st) for a in rest[2:]))
            return new
        if prog == "set" and rest and (rest[0] == "--" or rest[0][:1] not in "-+"):
            new = st.copy()
            args = [_expand(a, st) for a in (rest[1:] if rest[0] == "--" else rest)]
            _set_positional(new, args)
            return new
        if prog == "mktemp" or prog == "gmktemp":
            # mktemp CREATES its file / directory: that is a write.
            if "-u" in rest or "--dry-run" in rest:
                return None
            # Judged under BOTH readings (darwin BSD and GNU/TMPDIR) when they
            # differ: either one landing on main is refused.
            for darwin in {sys.platform == "darwin", False}:
                made = _mktemp_value(argv, env_st, self.an.root, darwin)
                if made is None:
                    raise _Undet("cannot tell which directory mktemp creates in")
                self.an.check(made, st)
            return None
        if prog == "case":
            return None

        if prog == "env":
            return self.env(rest, st, stdin)
        if prog in WRAPPERS:
            return self.wrapper(prog, rest, st, stdin, nxt)
        if prog == "eval":
            text = " ".join(rest)
            self.subst_literals(text, st)
            return self.an.analyze(text, st.copy(), self.depth + 1)
        if prog in SHELLS:
            self.shell(rest, st, stdin)
            return None
        if _PYTHON.match(prog):
            self.python(rest, st, stdin)
            return None
        if prog in ("node", "nodejs", "bun", "deno"):
            self.node(prog, rest, st, stdin)
            return None
        if prog in ("perl", "ruby"):
            self.perl_ruby(prog, rest, st, stdin)
            return None
        if prog in AWKS:
            self.awk(rest, st)
            return None

        if prog in TARGET_ALL:
            for a in _operands(rest):
                self.an.check(a, st)
            if prog in ("cp", "mv", "install", "ln", "gcp", "gmv", "ginstall", "gln"):
                # `--target-directory=DIR` / `-tDIR` / `-vtDIR` name the
                # destination inside a flag word (`-t DIR` already leaves DIR
                # as an operand, judged above).
                for a in rest:
                    if a.startswith("--target-directory="):
                        self.an.check(a.split("=", 1)[1], st)
                    elif not a.startswith("--"):
                        m = re.match(r"^-[A-Za-z]*?t(.+)$", a)
                        if m:
                            self.an.check(m.group(1), st)
        elif prog in TARGET_AFTER_FIRST:
            ops = _operands(rest)
            if prog == "chmod" and any(
                    re.fullmatch(r"-[rwxXst]+(?:,\S*)?", a) for a in rest):
                # `chmod -x FILE`: a symbolic mode that starts with `-` is the
                # MODE, not an option, so every plain operand is a target
                # (e033c406: `chmod -x .githooks/pre-commit` used to pass).
                targets = ops
            else:
                targets = ops[1:]
            for a in targets:
                self.an.check(a, st)
        elif prog in TARGET_LAST:
            ops = _copy_operands(prog, rest)
            if ops:
                self.an.check(ops[-1], st)
        elif prog in ("sed", "gsed"):
            self.sed(rest, st)
        elif prog == "dd":
            for a in rest:
                if a.startswith("of="):
                    self.an.check(a[3:], st)
        elif prog == "curl":
            self.curl(rest, st)
        elif prog == "wget":
            self.wget(rest, st)
        elif prog in ("tar", "gtar", "bsdtar"):
            self.tar(rest, st)
        elif prog == "unzip":
            self.unzip(rest, st)
        elif prog == "find":
            starts: list[str] = []
            j = 0
            while j < len(rest) and not (rest[j].startswith("-") or rest[j] in ("(", "!", ")")):
                starts.append(rest[j])
                j += 1
            starts = starts or ["."]
            self.find_expr(starts, rest[j:], st)
            if nxt in (";",) and any(a in ("-exec", "-execdir", "-ok", "-okdir") for a in rest):
                self.find_cont = starts
        elif prog == "git":
            # The git judgements made here (e033c406): rewiring the hook
            # directory, or a subcommand writing into it, disarms every local
            # gate, from any tree.
            self.git_hooks(rest, st)
        # NOTE: git subcommands (rm/mv/apply/checkout/restore/stash/reset/clean)
        # are deliberately NOT handled here. Their effect depends on the cwd they
        # run in (often a worktree), they are frequently RECOVERY or move-to-
        # worktree operations (`git stash`, `git checkout -- <path>` discard
        # changes — they clean the tree, they do not add code to it), and an
        # over-match here false-positived on the sanctioned worktree flow
        # (`git stash push` + `git worktree add`). Any git mutation that actually
        # reaches main is caught by check-worktree-isolation.py at commit time,
        # which is the sound, route-independent gate. `patch` is likewise left to
        # the commit chokepoint rather than over-matched to the whole tree.
        # `git -C <dir>` re-anchors only that one git command: it no longer
        # excuses the rest of the command line.
        return None

    # -- git and the hook machinery (e033c406) ------------------------------------
    def git_env(self, name: str, value: str) -> None:
        why = _git_env_rewires(name, value)
        if why:
            raise _HookHit("core.hooksPath", why)

    def git_hooks(self, rest: list[str], st: _State) -> None:
        """`git REST…`: refuse rewiring core.hooksPath (words expanded with the
        tracked variables first), and a working-tree-writing subcommand whose
        pathspec reaches `.githooks` / `.git/hooks`."""
        words = [_expand(a, st) for a in rest]
        why = _git_rewires_hooks(words)
        if why:
            raise _HookHit("core.hooksPath", why)
        gst = st
        i = 0
        while i < len(words):
            a = words[i]
            if a == "-C" and i + 1 < len(words):
                d = words[i + 1]
                if any(c in d for c in _UNRESOLVABLE) or (
                        not os.path.isabs(d) and gst.rel is None):
                    gst = _State(None, None, dict(st.vars))
                else:
                    r = _resolve(gst.rel or "/", d)
                    gst = _State(r, r, dict(st.vars))
                i += 2
                continue
            if a in ("-c", "--config-env") or a in _GIT_GLOBAL_VALUE_OPTS:
                i += 2
                continue
            if a.startswith("-"):
                i += 1
                continue
            break
        if i >= len(words) or words[i] not in _GIT_TREE_WRITERS:
            return
        sub, args = words[i], rest[i + 1:]
        if sub == "stash":
            if args[:1] == ["push"]:
                args = args[1:]
            elif not (args and args[0].startswith("-")):
                return  # list / show / pop / apply …: no pathspec (residual)
        specs: list[str] = []
        j = 0
        while j < len(args):
            a = args[j]
            if a == "--":
                if sub not in ("apply", "am"):
                    specs += args[j + 1:]
                break
            if sub in ("apply", "am") and a in ("--directory", "--include"):
                specs += args[j + 1:j + 2]
                j += 2
                continue
            if sub in ("apply", "am") and a.startswith(("--directory=", "--include=")):
                specs.append(a.split("=", 1)[1])
                j += 1
                continue
            if a in _GIT_SUB_VALUE_OPTS:
                j += 2
                continue
            if a.startswith("-"):
                j += 1
                continue
            if sub not in ("apply", "am"):  # their operands are patch files
                specs.append(a)
            j += 1
        for spec in specs:
            self.an.check_git_pathspec(spec, gst)

    # -- state changers --------------------------------------------------------
    def loop_var(self, rest: list[str], st: _State) -> _State:
        """`for f in A B …`: $f takes each listed value in turn. Its stand-in
        value is the longest literal prefix the values share, followed by a
        glob, so `for f in <wt>/*.txt` resolves under the worktree while a list
        mixing <wt> and <main> resolves to their common parent (refused). A
        list item this process cannot know (`$(…)`, an unknown variable) — or
        no `in` list at all (`"$@"`) — makes $f unknown."""
        new = st.copy()
        if not rest or not re.match(r"^[A-Za-z_][A-Za-z0-9_]*$", rest[0]):
            return new
        name = rest[0]
        if len(rest) < 3 or rest[1] != "in":
            new.vars[name] = UNKNOWN_VAL
            return new
        items = [_expand(i, st) for i in rest[2:]]
        if not items or any(c in i for i in items for c in "$`"):
            new.vars[name] = UNKNOWN_VAL
        elif len(items) == 1:
            new.vars[name] = items[0]
        else:
            common = os.path.commonprefix(items)
            if any(os.path.isabs(i) for i in items) and not common.startswith("/"):
                new.vars[name] = UNKNOWN_VAL  # absolute and relative mixed
            else:
                new.vars[name] = common[: common.rfind("/") + 1] + "*"
        return new

    def cd(self, rest: list[str], st: _State, nxt: str | None) -> _State:
        ops = [a for a in rest if a not in ("-P", "-L", "-e", "-@", "--")]
        if not ops:
            dest = st.vars.get("HOME", os.environ.get("HOME") or UNKNOWN_VAL)
        elif ops[0] == "-":
            return _State(None, None, dict(st.vars))  # $OLDPWD: not tracked
        else:
            dest = _expand(ops[0], st)
        fresh = dest.endswith("/" + MKTEMP_MARK) and not any(
            c in dest[: -len(MKTEMP_MARK)] for c in _UNRESOLVABLE)
        if fresh:
            # `cd "$(mktemp -d)"`: a known directory (its last component is the
            # invented name) that exists as soon as mktemp returned.
            here = os.path.realpath(dest[: -len(MKTEMP_MARK)]) + "/" + MKTEMP_MARK
            return _State(here, here, dict(st.vars))
        if any(c in dest for c in _UNRESOLVABLE) or (not os.path.isabs(dest) and st.rel is None):
            return _State(None, None, dict(st.vars))
        resolved = _resolve(st.rel or self.an.root, dest)
        new = _State(resolved, resolved, dict(st.vars))
        if nxt != "&&" and not os.path.isdir(resolved):
            # The cd may fail, leaving the cwd where it was, and the next command
            # runs anyway.
            return _merge(st, new)
        return new

    # -- wrappers --------------------------------------------------------------
    def env(self, rest: list[str], st: _State, stdin) -> None:
        st2 = st
        j = 0
        while j < len(rest):
            a = rest[j]
            if a in ("-u", "--unset") and j + 1 < len(rest):
                j += 2
                continue
            if a in ("-C", "--chdir") and j + 1 < len(rest):
                st2 = self.cd([rest[j + 1]], st2, "&&")
                j += 2
                continue
            if a in ("-S", "--split-string") and j + 1 < len(rest):
                self.an.analyze(" ".join(rest[j + 1:]), st2.copy(), self.depth + 1)
                return None
            m = _ASSIGN.match(a)
            if m:
                self.git_env(m.group(1), _expand(m.group(2), st2))
            if a.startswith("-") or m:
                j += 1
                continue
            break
        if j < len(rest):
            self.judge(rest[j:], st2, stdin)
        return None

    def wrapper(self, prog: str, rest: list[str], st: _State, stdin,
                nxt: str | None) -> _State | None:
        arg_opts = WRAPPERS[prog]
        j = 0
        positional = 1 if prog in ("timeout", "gtimeout") else 0
        st2 = st
        while j < len(rest):
            a = rest[j]
            if a == "--":
                j += 1
                break
            if a.startswith("-") and len(a) > 1:
                if prog == "command" and a in ("-v", "-V"):
                    return None  # a lookup, runs nothing
                if a in arg_opts and j + 1 < len(rest):
                    if prog == "time" and a == "-o":
                        self.an.check(rest[j + 1], st)
                    if prog == "sudo" and a == "-D":
                        st2 = self.cd([rest[j + 1]], st2, "&&")
                    j += 2
                    continue
                j += 1
                continue
            if positional:
                positional -= 1
                j += 1
                continue
            break
        inner = rest[j:]
        if not inner:
            return None
        if prog == "xargs":
            inner_prog = os.path.basename(_expand(inner[0], st))
            if inner_prog in STDIN_MUTATORS:
                # Its targets arrive on stdin; the ones this process can place
                # are relative to the cwd.
                self.an.check(".", st2)
        new = self.judge(inner, st2, stdin, nxt)
        return new if prog in SAME_SHELL_WRAPPERS else None

    def producer(self, text: str) -> str:
        """A fresh marker standing for a value whose PRODUCING text is known
        (`printf -v`, a here-string): it expands as unknown, but
        subst_literals judges the text's literal paths where it is run."""
        marker = f"{SUBST_WORD}{len(self.an.substs)}__"
        self.an.substs[marker] = text
        return marker

    def subst_literals(self, text: str, st: _State) -> None:
        """Text that a `$( … )` / backquote PRODUCES and that is then run
        (`eval "$(…)"`, `sh -c "$(…)"`, `$(…)` as a program) cannot be known.
        What can be judged is the substitution's own command text: a literal
        path in it that lands on main (`eval "$(echo rm <main>/f)"`, a printf
        format) is refused. `eval "$(brew shellenv)"` names none and passes."""
        inners = list(_quoted_substitutions(text))
        inners += [t for m, t in self.an.substs.items() if m in text]
        for inner in inners:
            for c in _payload_paths(inner):
                self.an.check(c, st)

    def _stdin_code(self, stdin, what: str) -> str | None:
        """The code an interpreter reads from stdin, if this process can see it."""
        if stdin is None or stdin[0] != "code":
            return None  # a pipe or a file: not visible here (documented residual)
        if stdin[1] is None:
            raise _Undet(f"{what} reads a here-document this gate could not isolate")
        return stdin[1]

    def shell(self, rest: list[str], st: _State, stdin) -> None:
        has_c = False
        j = 0
        while j < len(rest):
            a = rest[j]
            if a == "--":
                j += 1
                break
            if a[:1] in ("-", "+") and len(a) > 1:
                if a.startswith("--"):
                    if a == "--command":  # fish
                        has_c = True
                    j += 1
                    continue
                if "c" in a[1:] and a[0] == "-":
                    has_c = True
                if ("o" in a[1:] or "O" in a[1:]) and j + 1 < len(rest):
                    j += 2
                    continue
                j += 1
                continue
            break
        if has_c:
            if j < len(rest):
                self.subst_literals(rest[j], st)
                inner = st.copy()
                # `sh -c '…' NAME A B`: NAME is $0, A B are $1 $2 / "$@".
                extra = [_expand(a, st) for a in rest[j + 1:]]
                _set_positional(inner, extra[1:])
                if extra:
                    inner.vars["0"] = extra[0]
                self.an.analyze(rest[j], inner, self.depth + 1)
            return
        if j < len(rest):
            return  # a script FILE: its content is not judged (residual)
        code = self._stdin_code(stdin, "the shell")
        if code is not None:
            self.an.analyze(code, st.copy(), self.depth + 1)

    def scan(self, lang: str, code: str, args: list[str], st: _State) -> None:
        """Judge interpreter source that cannot be parsed as shell.

        A process spawn whose command is a LITERAL (`subprocess.run(['cat',
        p])`, `os.system('…')`, `execSync('…')`, perl/ruby backquotes,
        `system('…')`) is judged with the shell rules, like `sh -c`. If the
        payload contains a file-write primitive, a mutation word (see
        _UNAMBIG_MUTATION), or a spawn that could not be parsed that way, every
        literal path in it (and every operand passed to it) is a write-target
        candidate and is refused if it lands on main. Only a FILE WRITE with
        no literal path at all is refused for that reason alone (it cannot be
        placed, 3.); an unparsed spawn without one is an unknown program
        (residual)."""
        if lang == "awk":
            writes, spawns = _awk_effects(code)
            token = False
        else:
            body = _strip_perl_regex(code) if lang == "perl" else code
            writes = bool(_WRITE[lang].search(body))
            if lang == "ruby" and _ruby_writes(body):
                writes = True
            if lang == "python" and _py_open_for_write(code):
                writes = True
            token = bool(_UNAMBIG_MUTATION.search(body)) or _ambiguous_mutation(lang, body)
            spawns = False
            if _SPAWN[lang].search(body):
                sites, spawns = _spawn_sites(lang, body)
                if _MOVES_CONTEXT.search(body):
                    # The payload changes its own cwd / environment before
                    # spawning, so the literal command would run somewhere
                    # this walk does not know: judge it as unparsed too.
                    spawns = True
                for kind, val in sites:
                    text = val if kind == "sh" else " ".join(shlex.quote(w) for w in val)
                    self.an.analyze(text, st.copy(), self.depth + 1)
        if not (writes or spawns or token):
            return  # nothing in it can change a file: a read
        if lang == "perl":
            # `\/` is `/` (and `\\` is `\`) inside a perl string or s///e
            # replacement; read the literals with exactly those two unescaped,
            # and drop the `/e`-style regex flags that the absolute-path
            # pattern would otherwise take for a path. A CHARACTER-CODE escape
            # (`\x2f`, `\x{2f}`, `\057`, `\o{57}`, `\N{…}`, `\cX`) can spell a
            # path this scan cannot read, so in a payload that mutates it is
            # undetermined (refused), never a harmless relative literal.
            plain = re.sub(r"\\([/\\])", r"\1", code)
            if re.search(r"\\(?:x|[0-7]|o\{|N\{|c.)", plain):
                raise _Undet("a perl payload that mutates spells a literal with a "
                             "character-code escape")
            cands = [c for c in _payload_paths(plain)
                     if not re.fullmatch(r"/[msixpodualngcer]{1,8}", c)]
        else:
            cands = _payload_paths(code)
        cands += _operands(args)
        if writes and not cands:
            raise _Undet(f"a {lang} payload writes files, but names no literal path")
        for c in cands:
            if _fragment_of_root(self.an.root, c):
                raise _Hit(c)
            self.an.check(c, st)

    def python(self, rest: list[str], st: _State, stdin) -> None:
        j = 0
        while j < len(rest):
            a = rest[j]
            if a == "-":
                break
            if a.startswith("-") and not a.startswith("--") and "c" in a[1:]:
                idx = a.index("c", 1)
                code = a[idx + 1:] or (rest[j + 1] if j + 1 < len(rest) else "")
                args = rest[j + 1:] if a[idx + 1:] else rest[j + 2:]
                self.scan("python", code, args, st)
                return
            if a.startswith("-") and len(a) > 1:
                if a in ("-m",):
                    return  # a module: not visible here (residual)
                if a in ("-W", "-X") and j + 1 < len(rest):
                    j += 2
                    continue
                j += 1
                continue
            return  # a script file (residual)
        code = self._stdin_code(stdin, "python")
        if code is not None:
            self.scan("python", code, rest[j + 1:], st)

    def node(self, prog: str, rest: list[str], st: _State, stdin) -> None:
        j = 0
        if prog == "deno":
            if rest and rest[0] == "eval" and len(rest) > 1:
                self.scan("node", rest[1], rest[2:], st)
            return
        while j < len(rest):
            a = rest[j]
            if a.startswith("--eval=") or a.startswith("--print="):
                self.scan("node", a.split("=", 1)[1], rest[j + 1:], st)
                return
            if a in ("-e", "-p", "--eval", "--print", "-pe"):
                if j + 1 < len(rest):
                    self.scan("node", rest[j + 1], rest[j + 2:], st)
                return
            if a in ("-r", "--require", "--import", "--loader") and j + 1 < len(rest):
                j += 2
                continue
            if a.startswith("-") and a != "-":
                j += 1
                continue
            if a != "-":
                return  # a script file (residual)
            break
        code = self._stdin_code(stdin, prog)
        if code is not None:
            self.scan("node", code, [], st)

    def perl_ruby(self, prog: str, rest: list[str], st: _State, stdin) -> None:
        codes: list[str] = []
        inplace = False
        st2 = st
        j = 0
        while j < len(rest):
            a = rest[j]
            j += 1
            if a == "--":
                break
            if not a.startswith("-") or a == "-":
                j -= 1
                break
            if a.startswith("--"):
                continue
            b = a[1:]
            q = 0
            while q < len(b):
                ch = b[q]
                q += 1
                if ch in "eE":
                    if b[q:]:
                        codes.append(b[q:])
                    elif j < len(rest):
                        codes.append(rest[j])
                        j += 1
                    break
                if ch == "i":
                    inplace = True
                    break  # the rest of the bundle is the backup suffix
                if ch in "l0":
                    while q < len(b) and b[q].isdigit():
                        q += 1
                    continue
                if ch in "IMmrFxdDC":
                    val = b[q:]
                    if not val and ch in "IMmrC" and j < len(rest):
                        val = rest[j]
                        j += 1
                    if ch == "C" and prog == "ruby":
                        st2 = self.cd([val], st2, "&&")
                    break
        ops = rest[j:]
        if inplace:
            for a in ops:
                self.an.check(a, st2)
        lang = prog
        if codes:
            self.scan(lang, "\n".join(codes), ops, st2)
            return
        if ops:
            return  # a script file (residual)
        code = self._stdin_code(stdin, prog)
        if code is not None:
            self.scan(lang, code, [], st2)

    def awk(self, rest: list[str], st: _State) -> None:
        program: str | None = None
        progfile = False
        inplace = False
        j = 0
        while j < len(rest):
            a = rest[j]
            if a == "--":
                j += 1
                break
            if not a.startswith("-") or a == "-":
                break
            if a in ("-f", "--file"):
                progfile = True
                j += 2
                continue
            if a in ("-e", "--source") and j + 1 < len(rest):
                program = (program or "") + "\n" + rest[j + 1]
                j += 2
                continue
            if a in ("-i", "--include") and j + 1 < len(rest):
                inplace = inplace or rest[j + 1] in ("inplace", "inplace.awk")
                j += 2
                continue
            if a in ("-v", "-F", "--assign", "--field-separator") and j + 1 < len(rest):
                j += 2
                continue
            if a.startswith("-f"):
                progfile = True
            j += 1
        ops = rest[j:]
        if program is None and not progfile and ops:
            program, ops = ops[0], ops[1:]
        files = [a for a in ops if not _ASSIGN.match(a)]
        if inplace:
            for a in files:
                self.an.check(a, st)
        if program is not None:
            self.scan("awk", program, [], st)

    # -- file tools -----------------------------------------------------------
    def sed(self, rest: list[str], st: _State) -> None:
        inplace = False
        has_expr = False
        ops: list[str] = []
        j = 0
        while j < len(rest):
            a = rest[j]
            j += 1
            if a == "--":
                ops += rest[j:]
                break
            if a.startswith("--"):
                name = a[2:].split("=", 1)[0]
                if name == "in-place":
                    inplace = True
                elif name in ("expression", "file"):
                    has_expr = True
                    if "=" not in a:
                        j += 1
                continue
            if a.startswith("-") and len(a) > 1:
                b = a[1:]
                for q, ch in enumerate(b):
                    if ch == "i":
                        inplace = True
                        if q == len(b) - 1 and j < len(rest) and (
                            rest[j] == "" or rest[j].startswith(".")
                        ):
                            j += 1  # BSD `-i ''` / `-i .bak`: the suffix operand
                        break  # GNU: the rest of the bundle is the suffix
                    if ch in "ef":
                        has_expr = True
                        if q == len(b) - 1:
                            j += 1  # the script / script file is the next word
                        break
                    if ch == "l" and q == len(b) - 1:
                        j += 1
                        break
                continue
            ops.append(a)
        if not inplace:
            return
        # `sed -i 's/a/b/' f1 f2` — the FIRST operand is the script, not a file,
        # unless the script came via -e/-f (then all operands are files).
        for a in (ops if has_expr else ops[1:]):
            self.an.check(a, st)

    def curl(self, rest: list[str], st: _State) -> None:
        write_long = {"output", "dump-header", "cookie-jar", "trace", "trace-ascii",
                      "stderr", "libcurl", "etag-save", "hsts", "alt-svc"}
        arg_short = set("AbCdeEFHKmPQrTuUwxXyYzt")
        out_dir: str | None = None
        remote = False
        j = 0
        while j < len(rest):
            a = rest[j]
            j += 1
            if a.startswith("--"):
                name, eq, val = a[2:].partition("=")
                if name in write_long or name == "output-dir":
                    if not eq and j < len(rest):
                        val = rest[j]
                        j += 1
                    if name == "output-dir":
                        out_dir = val
                    else:
                        self.an.check(val, st)
                elif name in ("remote-name", "remote-name-all", "remote-header-name"):
                    remote = True
                continue
            if a.startswith("-") and len(a) > 1:
                b = a[1:]
                for q, ch in enumerate(b):
                    if ch in "oDc":
                        val = b[q + 1:]
                        if not val and j < len(rest):
                            val = rest[j]
                            j += 1
                        self.an.check(val, st)
                        break
                    if ch in arg_short:
                        if q == len(b) - 1:
                            j += 1
                        break
                    if ch in "OJ":
                        remote = True
        if remote:
            self.an.check(out_dir or ".", st)

    def wget(self, rest: list[str], st: _State) -> None:
        arg_short = set("eiBtTwQUlDRAIX")
        doc: str | None = None
        prefix: str | None = None
        spider = False
        j = 0
        while j < len(rest):
            a = rest[j]
            j += 1
            if a.startswith("--"):
                name, eq, val = a[2:].partition("=")
                if name in ("output-document", "output-file", "append-output",
                            "directory-prefix"):
                    if not eq and j < len(rest):
                        val = rest[j]
                        j += 1
                    if name == "output-document":
                        doc = val
                    elif name == "directory-prefix":
                        prefix = val
                    else:
                        self.an.check(val, st)
                elif name == "spider":
                    spider = True
                continue
            if a.startswith("-") and len(a) > 1:
                b = a[1:]
                for q, ch in enumerate(b):
                    if ch in "OoaP":
                        val = b[q + 1:]
                        if not val and j < len(rest):
                            val = rest[j]
                            j += 1
                        if ch == "O":
                            doc = val
                        elif ch == "P":
                            prefix = val
                        else:
                            self.an.check(val, st)
                        break
                    if ch in arg_short:
                        if q == len(b) - 1:
                            j += 1
                        break
        if spider:
            return
        if doc is not None:
            self.an.check(doc, st)
        else:
            self.an.check(prefix or ".", st)

    def tar(self, rest: list[str], st: _State) -> None:
        arg_short = set("fCbLNTVgKXIH")
        mode: set[str] = set()
        dests: list[str] = []
        archive: str | None = None
        j = 0
        if rest and not rest[0].startswith("-"):
            # Old-style `tar xzf a.tgz -C dir`: argument letters take the
            # following words in order.
            pending = [ch for ch in rest[0] if ch in arg_short]
            mode |= set(rest[0])
            j = 1
            for ch in pending:
                if j < len(rest):
                    if ch == "f":
                        archive = rest[j]
                    elif ch == "C":
                        dests.append(rest[j])
                    j += 1
        while j < len(rest):
            a = rest[j]
            j += 1
            if a.startswith("--"):
                name, eq, val = a[2:].partition("=")
                if name in ("extract", "get"):
                    mode.add("x")
                elif name in ("create", "append", "update", "catenate", "concatenate",
                              "delete"):
                    mode.add("c")
                elif name in ("directory", "file"):
                    if not eq and j < len(rest):
                        val = rest[j]
                        j += 1
                    if name == "directory":
                        dests.append(val)
                    else:
                        archive = val
                continue
            if a.startswith("-") and len(a) > 1:
                b = a[1:]
                for q, ch in enumerate(b):
                    if ch in arg_short:
                        val = b[q + 1:]
                        if not val and j < len(rest):
                            val = rest[j]
                            j += 1
                        if ch == "f":
                            archive = val
                        elif ch == "C":
                            dests.append(val)
                        break
                    mode.add(ch)
        if "x" in mode:
            for d in dests or ["."]:
                self.an.check(d, st)
        if mode & set("cruA") and archive:
            self.an.check(archive, st)

    def unzip(self, rest: list[str], st: _State) -> None:
        dest: str | None = None
        j = 0
        while j < len(rest):
            a = rest[j]
            j += 1
            if a.startswith("-") and len(a) > 1:
                b = a[1:]
                for q, ch in enumerate(b):
                    if ch in "ltvpzZ":
                        return  # list / test / pipe: writes nothing
                    if ch == "d":
                        dest = b[q + 1:]
                        if not dest and j < len(rest):
                            dest = rest[j]
                            j += 1
                        break
                    if ch == "x":
                        break
        self.an.check(dest or ".", st)

    def find_expr(self, starts: list[str], expr: list[str], st: _State) -> None:
        j = 0
        while j < len(expr):
            a = expr[j]
            j += 1
            if a == "-delete":
                for s in starts:
                    self.an.check(s, st)
            elif a in ("-fprint", "-fprint0", "-fls", "-fprintf") and j < len(expr):
                self.an.check(expr[j], st)
                j += 1
            elif a in ("-exec", "-execdir", "-ok", "-okdir"):
                inner: list[str] = []
                while j < len(expr) and expr[j] not in (";", "+"):
                    inner.append(expr[j])
                    j += 1
                j += 1
                if inner:
                    for s in starts:
                        # `{}` is replaced wherever it appears in a word
                        # (`{}/p.txt`), by BSD and GNU find alike.
                        self.judge([w.replace("{}", s) for w in inner], st, None)


DENY = """Refused: `{cmd}` mutates this project's MAIN working tree.

CLAUDE.md 最上位の方針 8: nothing edits, adds to, or deletes from the main tree
directly — every mutation happens in a `git worktree` and reaches main through a
merge. Another session is always assumed to be sharing this index.

    git worktree add -b <branch> <path> <base>
    # run the mutation against files inside that worktree, commit, then merge.
"""

DENY_UNPARSEABLE = """Refused: could not determine what `{cmd}` writes to.

The command does not tokenize (usually an unbalanced quote), so this gate cannot
tell whether it mutates the MAIN working tree. "Could not determine" is not
"does not touch main" (CLAUDE.md 最上位の方針 3), so it resolves to a refusal
rather than an allow.

Rewrite it so it parses — balance the quotes, or put the text in a here-document
whose body this gate skips — and run it again. If it does mutate the main tree,
do it in a worktree instead.
"""

DENY_UNDETERMINED = """Refused: could not determine what `{cmd}` writes to.

Reason: {why}.

The command hands code to an interpreter or wrapper (sh -c, eval, python -c,
node -e, awk, …) in a form this gate cannot place on the filesystem. "Could not
determine" is not "does not touch main" (CLAUDE.md 最上位の方針 3), so it
resolves to a refusal rather than an allow.

Rewrite it so the written path appears literally (or `cd <worktree> && …`
first), or do the work in a worktree.
"""

DENY_NO_ROOT = """Refused: could not determine this project's main working tree.

git could not answer where the main checkout is (it was not runnable, timed out,
or failed for a reason other than "not a git repository"), so no target in this
command could be judged against it. A check that could not run has not passed
(CLAUDE.md 最上位の方針 3), so this resolves to a refusal.

Fix whatever is stopping git from answering — or set CLAUDE_PROJECT_DIR to the
main checkout — and run it again.
"""


def _first_line(command: str) -> str:
    stripped = command.strip().splitlines()
    return stripped[0][:120] if stripped else ""


def _start_state(payload: dict, root: str | None = None) -> _State:
    """The shell's cwd when the command starts: the hook payload's `cwd`
    (the session's working directory), else this process's own cwd. Relative
    paths and `$PWD` both resolve against it, so they always agree.

    A `cwd` field that is present but unusable (not a string, not absolute, not
    an existing directory) is NOT replaced by a guess: the cwd is unknown, so
    every relative write is refused while absolute targets are still judged
    normally (3.)."""
    inherited: dict[str, str] = {}
    tmp = os.environ.get("TMPDIR")
    if tmp:
        if os.path.isabs(tmp) and root is not None and not _under_main(
                os.path.realpath(tmp), root) and not any(c in tmp for c in _UNRESOLVABLE):
            # Like HOME: the one other inherited value the hook shares with the
            # session (both are launched by the same Claude Code process).
            inherited["TMPDIR"] = tmp
        else:
            # Set, but relative / under main / unexpandable: unknown, never the
            # /tmp default (mktemp would honour it).
            inherited["TMPDIR"] = UNKNOWN_VAL
    if "cwd" not in payload:
        here = os.path.realpath(os.getcwd())
        return _State(here, here, inherited)
    cwd = payload.get("cwd")
    if isinstance(cwd, str) and os.path.isabs(cwd) and os.path.isdir(cwd):
        here = os.path.realpath(cwd)
        return _State(here, here, inherited)
    return _State(None, None, inherited)


def decide(payload: dict) -> tuple[int, str]:
    """(exit_code, stderr) of this guard's OWN judgement (no ledger)."""
    code, reason, _meta = _judge(payload)
    return code, reason


def _judge(payload: dict) -> tuple[int, str, dict]:
    """(exit_code, stderr, meta). meta carries what the deny ledger records:
    target_abs / raw_target / root (each possibly None)."""
    meta: dict = {"target_abs": None, "raw_target": None, "root": None}
    if payload.get("tool_name") != "Bash":
        return 0, "", meta
    tool_input = payload.get("tool_input")
    if not isinstance(tool_input, dict):
        return 2, DENY_BAD_PAYLOAD.format(why="Bash call without a tool_input object"), meta
    command = tool_input.get("command")
    if not isinstance(command, str):
        return 2, DENY_BAD_PAYLOAD.format(why="Bash call without a string command"), meta
    if not command.strip():
        return 0, "", meta  # an empty command runs nothing, so it mutates nothing

    if _GIT_CONFIG_ENV.search(command) and HOOKS_KEY in command.casefold():
        # GIT_CONFIG_KEY_n / GIT_CONFIG_PARAMETERS / GIT_CONFIG_GLOBAL in the
        # same command as core.hooksPath: config injected through the
        # environment, which the argv walk below does not see (e033c406).
        return 2, DENY_HOOKS.format(
            cmd=_first_line(command),
            why="it sets git config through GIT_CONFIG_* in a command naming "
                "core.hooksPath"), meta

    state, root = _main_root()
    if state == UNDET:
        # Could not establish whether there is a main tree here at all, so every
        # target below would be judged against nothing. A check that could not
        # run has not passed (3.).
        return 2, DENY_NO_ROOT, meta
    guard_main = state == OK
    if not guard_main:
        # git gave a determinate answer: not a repository, or this anchor is a
        # linked worktree. There is no main tree to protect, but the hook
        # machinery still is (e033c406): run the walk in hooks-only mode.
        root = _NO_MAIN
    assert root is not None
    meta["root"] = root if guard_main else None

    an = _Analyzer(root, guard_main=guard_main)
    start = _start_state(payload, root)
    try:
        an.analyze(command, start, 0)
    except _HookHit as hit:
        if guard_main:
            meta["target_abs"] = an.hit_abs
        return 2, DENY_HOOKS.format(cmd=_first_line(command), why=hit.args[1]), meta
    except (_Unparseable, _Undet, _Hit) as e:
        if not guard_main:
            # Hooks-only pass: "cannot determine" here is about a main tree
            # that does not exist, so only a command that names hook
            # machinery is refused (as undetermined) — never silently allowed.
            low = command.casefold()
            if any(m in low for m in (".githooks", ".git/hooks", ".git/config",
                                      "config.worktree", HOOKS_KEY)):
                return 2, DENY_HOOKS.format(
                    cmd=_first_line(command),
                    why="it names the hook machinery in a form this gate "
                        "cannot place on the filesystem"), meta
            return 0, "", meta
        if isinstance(e, _Unparseable):
            # The command does not tokenize, so its filesystem targets are
            # unknown — which is not the same as "it has none". This used to
            # allow, and a here-doc body containing an apostrophe was enough to
            # walk a write to main straight past the gate.
            return 2, DENY_UNPARSEABLE.format(cmd=_first_line(command)), meta
        if isinstance(e, _Hit):
            meta["target_abs"] = an.hit_abs
            raw = e.args[0]
            if isinstance(raw, str) and os.path.isabs(raw):
                meta["raw_target"] = raw
            return 2, DENY.format(cmd=f"{_first_line(command)}` (target `{e.args[0]}"), meta
        return 2, DENY_UNDETERMINED.format(cmd=_first_line(command), why=e.args[0]), meta
    return 0, "", meta


DENY_HOOKS = """Refused: `{cmd}` would disarm this repository's git hooks ({why}).

The local gates live in `.githooks` (CLAUDE.md 最上位の方針 7) and are reached
through `core.hooksPath`. Rewiring that setting, unsetting it, or writing into
`.githooks`, `.git/hooks` or `.git/config` takes every one of them out at once,
so it is refused from any tree and any cwd (backlog e033c406). Reading them is
allowed. If the hooks genuinely need to change, hand it to the human.
"""

DENY_LEDGER_MODULE = """Refused: the maintree deny ledger module could not be loaded ({why}).

scripts/deny_ledger.py records this session's refusals so the same target cannot
be re-reached by another spelling (backlog e033c406). Without it that check
cannot run, and a check that could not run has not passed (CLAUDE.md 3).
"""


DENY_BAD_PAYLOAD = """Refused: could not read the hook payload ({why}).

This gate received no usable JSON object describing the tool call, so it cannot
tell which command is about to run or whether it mutates the MAIN working tree.
A payload this gate could not read is not a command it checked (CLAUDE.md
最上位の方針 3), so it resolves to a refusal rather than an allow.
"""


def main() -> int:
    try:
        payload = json.load(sys.stdin)
    except (json.JSONDecodeError, UnicodeDecodeError, ValueError) as e:
        sys.stderr.write(DENY_BAD_PAYLOAD.format(why=f"not JSON: {e}"))
        return 2
    if not isinstance(payload, dict):
        sys.stderr.write(
            DENY_BAD_PAYLOAD.format(
                why=f"JSON {type(payload).__name__}, not an object"
            )
        )
        return 2
    if deny_ledger is None:
        sys.stderr.write(DENY_LEDGER_MODULE.format(why=_LEDGER_IMPORT_ERROR))
        return 2
    code, reason, meta = _judge(payload)
    if code != 0:
        command = (payload.get("tool_input") or {}).get("command") if isinstance(
            payload.get("tool_input"), dict) else None
        note = deny_ledger.record_deny(
            payload, "guard-maintree-bash.py", meta["target_abs"], meta["root"],
            _first_line(command) if isinstance(command, str) else "<no command>",
            reason, raw_target=meta["raw_target"])
        sys.stderr.write(reason + (("\n" + note + "\n") if note else ""))
        return code
    if payload.get("tool_name") != "Bash":
        return 0
    # This guard's own judgement allowed the call; now the deny ledger
    # (signal 1): the same session re-reaching a refused target.
    rc = deny_ledger.gate(payload)
    return 0 if rc is None else rc


if __name__ == "__main__":
    try:
        _rc = main()
    except Exception as _crash:  # noqa: BLE001
        # An uncaught exception exits 1, which Claude Code treats as a
        # non-blocking error, i.e. the call proceeds. A check that crashed has
        # not passed (CLAUDE.md 3), so it resolves to exit 2 (e033c406).
        sys.stderr.write(
            f"Refused: guard-maintree-bash.py crashed ({type(_crash).__name__}: {_crash}); "
            "a check that could not run has not passed.\n")
        _rc = 2
    sys.exit(_rc)

# KNOWN, UNCLOSED HOLES (recorded, not hidden — CLAUDE.md 4):
#   * paths REBUILT AT RUNTIME inside an interpreter payload from fragments
#     that are not themselves path-shaped (`'/Users/u/src/har' + 'ness'`,
#     `os.path.join(a, b)` with a and b computed, `chr()`-built strings, a
#     path read from a file or the environment). A payload that writes and
#     names no literal path at all is refused, but one that ALSO names an
#     unrelated literal path outside main is judged on that literal and
#     allowed. Single-quoted shell text inside a payload (`'$S'`) is expanded
#     as if the shell had expanded it.
#   * an UNKNOWN PROGRAM is only judged through its literal operands and the
#     literal paths of the command that produced it — directly or through a
#     variable filled by `$( … )`, backquotes, `printf -v`, `read`/`mapfile`
#     from a here-string, or `set --`: the text `eval "$(cmd)"` evaluates is
#     not known (`eval "$(cat script)"` that writes main is not seen), nor is
#     `sh -c "$CMD"` / `$CMD …` with CMD not assigned in the command or filled
#     by a producer with no literal text (`read CMD < file`, a value merged
#     from two branches), nor what any program with an unresolvable name does
#     with a path it was not handed literally.
#   * mktemp on a platform whose `-t` / no-template directory is neither the
#     darwin confstr dir nor TMPDIR/`/tmp` is judged against those two; python
#     `tempfile.mkdtemp()`-derived paths are not tracked (a write that names
#     only such a path and no literal one is refused as undetermined).
#   * perl backslash unescaping is applied to the whole payload text at once;
#     a path spelled with other perl escapes (`\x2f`, `chr(47)`) is not read.
#   * `env`: the separate-word `-C DIR` / `--chdir DIR` re-anchor is applied,
#     but the glued GNU spellings (`--chdir=DIR`, `-CDIR`) are not, and
#     `env VAR=value cmd` assignments (e.g. `env TMPDIR=<main> mktemp`) do not
#     reach the command's judged environment.
#   * GNU mktemp's TMPDIR as seen inside a nested `bash -c` / `sh -c` is the
#     tracked value at the point of the call; an assignment made only inside
#     the outer process's environment by other means (a profile, `env -S`)
#     is not.
#   * GNU long-option abbreviations (`cp --target=DIR`, `--targ=DIR`, `rsync
#     --exc zz`) are not expanded: only the full option names are recognised.
#   * IFS: word splitting is on blanks only; a command that changes IFS
#     (`IFS=/; set -- $P`) is split as if IFS were the default.
#   * ruby Pathname tracking is by direct `v = Pathname(…)` assignment only; a
#     Pathname reached through a method chain on another variable, a block
#     parameter, or `Pathname(…).join(…)` held in a second variable is not.
#   * an interpreter spawn that could not be parsed as a literal command
#     (an alias, an f-string, os.exec*, Popen with cwd=, awk system() /
#     `| "cmd"`) and names no literal path: the spawned command is not
#     judged. A literal spawn is judged as shell, but a payload that changes
#     its cwd/environment through a spelling not listed (e.g. ctypes) is not
#     seen, and the literal command is then judged against the session cwd.
#   * the ambiguous-word rule follows aliases by assignment / import /
#     for-loop and python call arity only; a filesystem object reached another
#     way (python: a function returning `shutil` whose result is called
#     `.copy`-free, e.g. `.replace(a, b)` with two arguments; node: an fs
#     object passed in as an argument and called `.write`) is a read to this
#     scan. The perl regex blanking trusts the term-position
#     heuristic; text it blanks is not scanned for mutation words.
#   * the rules can still OVER-refuse a read of main: a payload naming a main
#     path that also holds an unrelated unparsed spawn, a dynamic lookup
#     (getattr, eval, …), a `.copy(x)` call on a non-path object, or an
#     open(…) whose file name is a bare mode-shaped word (`open('a')`).
#   * git subcommands (`git apply`, `git checkout -- <path>`, `git restore`,
#     `git stash`, `git merge`, …) — deliberately, so merges on main stay
#     possible; the commit-time gate is the backstop.
#   * code this hook cannot read: a script FILE (`bash x.sh`, `python3 x.py`,
#     `node x.js`, `source x`, `python3 -m mod`), code piped on stdin
#     (`curl … | sh`, `cat x | python3`), `osascript -e`, `php -r`, and any
#     other interpreter not listed in the module docstring.
#   * write primitives this hook does not list and that carry no mutation
#     word (a write through an arbitrary library call, `sed 's/a/b/w file'`, a here-string fed to
#     an unlisted interpreter), and `xargs` / `find -exec` targets that arrive
#     ABSOLUTE on stdin (relative ones are judged against the cwd).
#   * a tool that resolves its own paths (a Makefile, cargo), and a function
#     body or alias defined and called in the same command.
#   * shlex loses quoting, so a quoted `';'`, `'>'` or `'$(…)'` is read as shell
#     syntax: that only ever OVER-refuses (an extra separator / redirect /
#     payload), it cannot hide a write that is spelled literally.
#   All of these are caught at the durable moment by check-worktree-isolation.py,
#   which refuses the commit regardless of how the tree was dirtied.
#   The following are NOT backstopped by that commit gate (a config change
#   or a hook-file change on its own makes no main-tree commit):
#   * hook machinery (e033c406): core.hooksPath set through a config FILE
#     (`GIT_CONFIG_GLOBAL=<file>` whose file was written in an earlier call,
#     `include.path`), a git alias, or an interpreter spawn whose command is
#     not a literal; a relative write into `.githooks` against a cwd this walk
#     could not track while in the hooks-only mode; a write target whose
#     unknown variable stands for `.githooks` (`rm $X/pre-commit`); brace
#     expansion (`.git{hooks,x}`).
#   * git subcommands that rewrite hook files WITHOUT a pathspec naming them —
#     deliberately, so merges and resets on main keep working: `git reset
#     --hard`, `git checkout .` / `git checkout <branch>`, `git switch`, `git
#     merge` / `pull` / `rebase` / `cherry-pick`, `git stash pop` / `apply`,
#     `git apply` / `am` of a patch file whose content touches `.githooks`
#     (the patch is not read), and `--pathspec-from-file`. A pathspec that is
#     an ANCESTOR of `.githooks` (`.`, the repo root) is such a broad form.
#   * deny ledger (e033c406): see the RESIDUALS in deny_ledger.py (no
#     session_id key; a retry spelled relatively / through a symlink is not
#     matched at PreToolUse, though its effect on main is still seen at Stop
#     and, if committed, by the commit gate).
