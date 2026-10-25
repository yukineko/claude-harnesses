"""Per-session ledger of main-tree refusals, and the gates built on it (e033c406).

WHY THIS EXISTS. A PreToolUse refusal is invisible to every later hook call: the
payload Claude Code hands a hook (crates/harness-core/src/hook.rs, HookInput)
carries session_id / transcript_path / cwd / hook_event_name / tool_name /
tool_input, never "the previous call was denied". So after
guard-maintree-edit.py refuses an Edit/Write into the main working tree, nothing
stopped the agent from reaching the very same file with a different spelling
(the "hack route"). The denier must therefore RECORD its refusals, and that
record is this ledger.

ONLY THE EDIT GUARD USES IT (user ruling 2026-10-04). guard-maintree-bash.py
OBSERVES main before and after each Bash call instead of predicting writes from
command text; it records nothing here, consults nothing here, and does not
protect the ledger directories. A Bash call is therefore neither recorded as a
deny, nor counted as a guarded call, nor refused by signal 1. A Bash write into
main is caught by that guard's own Post comparison; a Bash write that reaches a
target refused by the edit guard is still seen at Stop by signal 2.

WHERE. `$HOME/.claude/state/maintree-deny/<session_id>.jsonl` — one file per
session, under the home directory (`os.path.expanduser("~")`). Deliberately
NEVER under the project root: a shared, one-shot file in the project root is the
failure mode CLAUDE.md §5 forbids (another session would consume or poison it).
A session_id that is not a plain `[A-Za-z0-9._-]{1,128}` word (and not `.` /
`..`) cannot name a file safely, so it is UNDETERMINED, never sanitised into
some other session's name.

FALLBACK. When the home ledger cannot be written (an unwritable or
non-absolute HOME), the line goes to the fallback ledger
`<tmp>/maintree-deny-<uid>/<session_id>.jsonl` (same format, also per
session), where <tmp> is the first writable absolute directory among $TMPDIR,
$TEMP, $TMP, /tmp, /var/tmp, /usr/tmp (tempfile.gettempdir()'s order, computed
by _tempdir without importing tempfile, for latency). There is NO cwd fallback:
when none of those is usable there is no fallback ledger at all. <tmp> is
shared with other users, so the fallback directory is used only when lstat
shows a real directory (not a symlink) owned by this uid with mode 0700; it is
created that way when missing, and anything else is skipped for both reading
and writing. Every ledger file (home and fallback) is opened with O_NOFOLLOW,
so a symlink planted at the file's path is an unreadable ledger (refused), never
a redirect. Readers always consult BOTH files and merge their lines by `epoch`,
so a deny that could only be recorded in the fallback is still seen by later
calls and at Stop. A refusal that can be written to neither keeps its deny and
says so in its message (RESIDUALS).

SELF-PROTECTION. An Edit/Write/MultiEdit/NotebookEdit into either ledger
directory (protected_dirs) is refused from any tree by guard-maintree-edit.py.
Reading them stays allowed. A Bash command that writes, deletes or moves a
ledger file is NOT refused by any hook (user ruling 2026-10-04; see RESIDUALS).

FORMAT. JSON lines, each an object with `"v": 1` and a `kind`:

  deny  {ts, epoch, session_id, denier, target_abs, raw_target, root, raw,
         reason, snapshot}
        target_abs is the realpath of the refused write target, or null when the
        refusal was a "cannot determine" one with no single target (those are
        kept for the record but are never matched — there is no path to match).
        snapshot is the target's state at deny time: exists / is_dir /
        mtime_ns / size, and `git status --porcelain -z -- <target>` run at
        `root` (status null when git could not answer).
        raw and reason are TRUNCATED: raw to its first RAW_LIMIT (120)
        characters (the guards pass the command's first line / "<Tool>
        <path>"), reason to the first non-blank line of the refusal, at most
        REASON_LIMIT (300) characters. The quote shown on a later refusal is
        built from these truncated fields.
  tick  {epoch}  one guarded tool call seen while some deny was still in its
        window; used to count calls since a deny.

  A line that does not decode, is not an object, has an unknown `v`/`kind`, or
  lacks a required field makes the WHOLE ledger corrupt.

SIGNAL 1 — PREVENTION (check_retry, called by guard-maintree-edit.py after its
own judgement allowed the call). A deny entry is ACTIVE for
RETRY_WINDOW_CALLS (25) guarded calls or RETRY_WINDOW_SECS (20 minutes) after
it, whichever ends first. "Guarded calls" are the PreToolUse calls of the edit
guard (Edit/Write/MultiEdit/NotebookEdit) in the same session that reach
this check (calls the guard itself refused are recorded as denies instead); they
are counted by tick lines, which are only written while at least one deny is active
(so a session with no recent refusal pays two failed `open`s — home and
fallback — and no write). If
an active entry's target_abs is still under its recorded main root and appears
in any string of the new call's tool_input — as a substring that is not
continued by another file-name character, so `<main>/f` does not match
`<main>/f2`; folded for case on darwin, like the guards — the call is refused,
quoting the deny verbatim. A redo against a worktree path has a different
target_abs and is silent. This is a SUBSTRING test on purpose: it fires on a
later mention of the exact path in the window, in any string of tool_input
(content / old_string / new_string included), because the point is to make the
agent (or a human) confirm the new call is a genuinely different approach.

NARROWED (issue #1180, backlog 11756f2e, user ruling 2026-10-25 「1180は一旦
せばめて」): when the call's write target (file_path / notebook_path) is
POSITIVELY outside every git repository — e.g. a note in the session
scratchpad — only the target path strings are matched, not its content, so a
scratch note that merely names the denied path is allowed. "Positively
outside" means: an absolute path, not under any live deny's recorded root (as
recorded or by realpath), and `git rev-parse` at its nearest existing directory
failed with git's own "not a git repository" (LC_ALL=C, no
GIT_CEILING_DIRECTORIES, discovery across filesystems). Every other outcome —
a relative or missing path, a repo found (a worktree file included, so a
content retry into any repo stays refused), git unusable, a timeout, any other
failure — keeps the full substring test (CLAUDE.md 3). A scratchpad that sits
inside some repository is therefore NOT narrowed.

SIGNAL 2 — DETECTION (stop_check, called by scripts/stop-verify-worktree.py).
At Stop, every deny entry with a target is re-snapshotted. The target counts as
CHANGED when its git status line differs from the snapshot, or when it is dirty
now (non-empty status) and exists / is_dir / mtime_ns / size differ. A target
that is clean against HEAD now while HEAD moved (a merge on main — the one
sanctioned main-tree operation) is not a change this gate owns; the commit-time
gate check-worktree-isolation.py judges how HEAD moved. This works regardless of
the spelling that made the change.

ASK vs DENY. The prevention signal and every undetermined case "ask": the hook
prints `{"hookSpecificOutput": {"hookEventName": "PreToolUse",
"permissionDecision": "ask", "permissionDecisionReason": ...}}` on stdout and
exits 0. An ask is only an answer when a human can respond, so — mirroring
blastguard (crates/blastguard/src/interactive.rs) — it is emitted ONLY when
CLAUDECODE=1 and CLAUDE_CODE_ENTRYPOINT=cli; any other environment hardens the
ask to a deny (exit 2, reason on stderr). There is no override variable.

UNDETERMINED RESOLVES TO REFUSAL (CLAUDE.md 3). An unreadable ledger, a corrupt
line, a session_id that is present but unusable, or a target / root in an entry
that is not an absolute path refuses (ask, hardened as above; at Stop, a block)
— never allows. The ONE exception: a ledger FILE that does not exist means "no
prior denies".

BRICK BOUND. A CORRUPT ledger file (readable, but with lines that are not
valid entries or bytes that are not UTF-8) refuses only for
RETRY_WINDOW_SECS (20 minutes) measured from that file's mtime — otherwise a
non-interactive run, which never submits a prompt, would be refused on every
Edit/Write call and every Stop for the rest of the run. Once the file is older
than that, it is moved aside to `<name>.corrupt-<unix-ts>` (or
`<name>.corrupt-<unix-ts>-<n>` when that name exists: an aside is never
replaced; left for a human to inspect) and SALVAGED: its lines that are valid `deny` entries are written
into a fresh active file at the original path (O_CREAT|O_EXCL|O_NOFOLLOW, 0600),
so those refusals keep feeding signals 1 and 2; only the lines that are not
valid entries are dropped (ticks are not carried over: the call count restarts).
A stderr notice names the aside path, how many deny lines were carried over,
and the dropped line numbers. A file with no valid deny line leaves no active
file. If writing the salvaged lines fails, the notice says so and they are used
for this call only. A file whose mtime cannot be read is refused without a
bound. If the rename fails, the notice says so and the salvaged lines are used
for this call only (the stale file is re-examined on the next).
An UNREADABLE ledger file (open or read fails: permissions such as mode 000, a
symlink planted at the path, an IO error) has NO bound and is never moved
aside: nothing can be salvaged from it, so moving it aside would turn "the
prior refusals could not be read" into "there were none" (issue #1182). It
refuses every guarded call and every Stop until a human repairs or removes it
or a new user prompt clears the session's ledger (deny-ledger-clear.py). A
non-interactive run is therefore refused for the rest of the run by an
unreadable ledger; Stop is still released by `stop_hook_active` (RESIDUALS).

RESIDUALS (recorded, not hidden — CLAUDE.md 4):
  * a payload with NO session_id key has no ledger: nothing is recorded or
    checked for it. Claude Code always sends session_id (hook.rs), so this is
    the shape of a hand-made payload, not of a real tool call.
  * matching is on the absolute path as the guard resolved it (realpath) and on
    the target as originally spelled when that was absolute. A retry that
    spells the same file relatively, through a symlink, or rebuilt at runtime
    is not matched by signal 1; signal 2 still sees the effect at Stop.
  * signal 2 can block a stop for a change another session made to the same
    main-tree path during this turn; the message says so, and the Stop gate's
    `stop_hook_active` bounded allow still applies.
  * the ledger is cleared by the UserPromptSubmit hook
    (scripts/deny-ledger-clear.py): a new human instruction is new authority.
    A non-interactive session never submits a prompt, so its entries age out
    of the prevention window only; signal 2 keeps them for the whole session.
  * a refusal that can be written to NEITHER the home ledger NOR the fallback
    is not remembered: later calls and Stop cannot see it. The deny itself
    stands, and its message says the record failed.
  * when a tick cannot be written (both locations fail), the call count does
    not advance; the window then ends by RETRY_WINDOW_SECS alone. The tick
    failure itself does not refuse the call.
  * a deny recorded while its ledger file is corrupt is appended to that file
    and refreshes its mtime, which restarts the brick bound for that file.
  * an UNREADABLE ledger file has no brick bound (issue #1182): in a session
    that never submits a prompt it refuses every guarded call for the rest of
    the session, and each Stop blocks once before `stop_hook_active` releases
    it. Restrictive by design — the prior refusals it holds cannot be read.
  * a deny whose recorded root no longer exists (the main checkout was moved
    or deleted) cannot be re-snapshotted, so Stop blocks as undetermined for
    it until the ledger is cleared (the next UserPromptSubmit) or pruned (a
    file older than 7 days, at a later clear). This is deliberate: a vanished
    root is not evidence that the refused path was left alone.
  * ledger self-protection covers the edit tools only. A Bash command can
    write, delete or move a ledger file and no hook refuses it (user ruling
    2026-10-04 dropped the Bash side). A tampered or deleted ledger FILE is not
    detected after the fact.
  * signal 1 never fires on a Bash call: a Bash retry of a target the edit
    guard refused is not prevented; only its effect is seen, at Stop, by
    signal 2 (and, for main, by guard-maintree-bash.py's Post comparison).
"""

from __future__ import annotations

import json
import os
import re
import stat
import subprocess
import sys
import time

LEDGER_SUBDIR = os.path.join(".claude", "state", "maintree-deny")
RETRY_WINDOW_CALLS = 25
RETRY_WINDOW_SECS = 20 * 60
RAW_LIMIT = 120
REASON_LIMIT = 300
_SESSION_RE = re.compile(r"^[A-Za-z0-9._-]{1,128}$")
_FOLD_CASE = sys.platform == "darwin"
# characters that continue a file name: a match followed by one of these is a
# DIFFERENT path (`<main>/f2`, `<main>/f.bak`), not the denied one.
_NAME_CHAR = re.compile(r"[A-Za-z0-9_.@+~-]")


class Undetermined(Exception):
    """The ledger could not be read / trusted, or the payload names no usable
    session. Resolves to a refusal at the call site."""


# ---------------------------------------------------------------------------
# location
# ---------------------------------------------------------------------------
def session_of(payload: dict) -> str | None:
    """The payload's session_id. None when the key is absent (no ledger
    applies); raises Undetermined when it is present but unusable."""
    if "session_id" not in payload:
        return None
    sid = payload.get("session_id")
    if not isinstance(sid, str) or not _SESSION_RE.match(sid) or sid in (".", ".."):
        raise Undetermined(f"the hook payload's session_id {sid!r} cannot name a ledger file")
    return sid


def ledger_dir() -> str:
    home = os.path.expanduser("~")
    if not home or not os.path.isabs(home):
        raise Undetermined("the home directory is not an absolute path")
    return os.path.join(home, LEDGER_SUBDIR)


def ledger_path(session_id: str) -> str:
    return os.path.join(ledger_dir(), session_id + ".jsonl")


def _tempdir() -> str | None:
    """The directory tempfile.gettempdir() picks, by its candidate order
    ($TMPDIR, $TEMP, $TMP, /tmp, /var/tmp, /usr/tmp; the first that is a
    writable directory) — without importing tempfile, whose import costs
    several milliseconds on every hook call. Its write probe is replaced by
    os.access(W_OK|X_OK). Unlike tempfile it NEVER falls back to the cwd (a
    project tree): None means there is no fallback location."""
    cands = [os.environ.get(k) for k in ("TMPDIR", "TEMP", "TMP")]
    cands += ["/tmp", "/var/tmp", "/usr/tmp"]
    for c in cands:
        if c and os.path.isabs(c) and os.path.isdir(c) and os.access(c, os.W_OK | os.X_OK):
            return os.path.abspath(c)
    return None


def fallback_dir() -> str | None:
    t = _tempdir()
    if t is None:
        return None
    uid = os.getuid() if hasattr(os, "getuid") else "u"
    return os.path.join(t, f"maintree-deny-{uid}")


def _fallback_dir_safe(d: str, create: bool) -> bool:
    """The fallback dir lives in a SHARED temp dir, so it is used only when it
    is a real directory (not a symlink) owned by this uid with mode 0700.
    `create` makes it (0700) when it is missing. Anything else: not used."""
    try:
        st = os.lstat(d)
    except FileNotFoundError:
        if not create:
            return False
        try:
            os.mkdir(d, 0o700)
            os.chmod(d, 0o700)  # the umask may have narrowed it; never widened
            st = os.lstat(d)
        except OSError:
            return False
    except OSError:
        return False
    if not stat.S_ISDIR(st.st_mode):
        return False
    if hasattr(os, "getuid") and st.st_uid != os.getuid():
        return False
    return stat.S_IMODE(st.st_mode) == 0o700


def fallback_path(session_id: str) -> str | None:
    d = fallback_dir()
    return None if d is None else os.path.join(d, session_id + ".jsonl")


def ledger_paths(session_id: str) -> list[str]:
    """Where this session's lines may live: the home ledger (when HOME is
    usable) and the temp-dir fallback (when there is one), in
    write-preference order."""
    paths = []
    try:
        paths.append(ledger_path(session_id))
    except Undetermined:
        pass  # no usable HOME: the fallback below is the only location
    fb = fallback_path(session_id)
    if fb is not None:
        paths.append(fb)
    return paths


def _is_fallback(path: str) -> bool:
    d = fallback_dir()
    return d is not None and os.path.dirname(path) == d


def protected_dirs() -> list[str]:
    """The ledger directories a TOOL CALL must not write into (realpaths):
    erasing or back-dating the ledger would erase signals 1 and 2."""
    out = []
    try:
        out.append(os.path.realpath(ledger_dir()))
    except Undetermined:
        pass
    fb = fallback_dir()
    if fb is not None:
        out.append(os.path.realpath(fb))
    return out


# ---------------------------------------------------------------------------
# reading
# ---------------------------------------------------------------------------
def _valid_entry(obj) -> bool:
    if not isinstance(obj, dict) or obj.get("v") != 1:
        return False
    kind = obj.get("kind")
    if kind == "tick":
        return isinstance(obj.get("epoch"), (int, float))
    if kind != "deny":
        return False
    if not isinstance(obj.get("epoch"), (int, float)):
        return False
    for key in ("ts", "denier", "raw", "reason"):
        if not isinstance(obj.get(key), str):
            return False
    for key in ("target_abs", "root"):
        val = obj.get(key)
        if val is not None and not (isinstance(val, str) and os.path.isabs(val)):
            return False
    if obj.get("raw_target") is not None and not isinstance(obj.get("raw_target"), str):
        return False
    snap = obj.get("snapshot")
    if snap is not None and not isinstance(snap, dict):
        return False
    return True


def _read_bytes(path: str) -> bytes | None:
    """The file's bytes; None when it does not exist (or, for the fallback,
    when its directory is not a safe one). O_NOFOLLOW: a symlink planted at
    the ledger path is an error, not a redirect."""
    if _is_fallback(path) and not _fallback_dir_safe(os.path.dirname(path), create=False):
        return None
    try:
        fd = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    except FileNotFoundError:
        return None
    except OSError as e:
        raise Undetermined(f"the deny ledger {path} could not be read ({e})") from e
    try:
        chunks = []
        while True:
            b = os.read(fd, 1 << 16)
            if not b:
                break
            chunks.append(b)
    except OSError as e:
        raise Undetermined(f"the deny ledger {path} could not be read ({e})") from e
    finally:
        os.close(fd)
    return b"".join(chunks)


def _parse(path: str, data: bytes) -> tuple[list[dict], list[int]]:
    """(valid entries, numbers of the lines that are not valid entries)."""
    text = data.decode("utf-8", "replace")
    out: list[dict] = []
    bad: list[int] = []
    for n, line in enumerate(text.splitlines(), 1):
        if not line.strip():
            continue
        try:
            obj = json.loads(line)
        except ValueError:
            bad.append(n)
            continue
        if _valid_entry(obj):
            out.append(obj)
        else:
            bad.append(n)
    try:
        data.decode("utf-8")
    except UnicodeDecodeError:
        bad.append(0)  # 0: the file as a whole is not UTF-8
    return out, bad


def _move_aside(path: str, stamp: int) -> str:
    """Move `path` to a name that did not exist, and return that name.

    The name is `<path>.corrupt-<stamp>`, or `<path>.corrupt-<stamp>-<n>` when
    that is taken. The move is os.link (fails with EEXIST, never replaces) then
    unlink, so two salvages in the same second, or racing processes, can never
    overwrite an earlier aside (issue #1183). Raises OSError when no free name
    is obtained or the link fails (the original is left in place), or when the
    unlink fails after the link (the original stays and a hard-linked copy exists
    at the aside name; the message says so)."""
    for n in range(1000):
        aside = f"{path}.corrupt-{stamp}" + (f"-{n}" if n else "")
        try:
            os.link(path, aside)
        except FileExistsError:
            continue
        try:
            os.unlink(path)
        except FileNotFoundError:
            pass  # a racing salvage already moved the same file; both asides hold it
        except OSError as e:
            # The link succeeded, so a copy exists at `aside`, but the original
            # is still in place: say both, and stay restrictive (the caller
            # treats any OSError as "not moved" and uses the salvage this call).
            raise OSError(f"a hard-linked copy exists at {aside} but the "
                          f"original could not be removed: {e}") from e
        return aside
    raise OSError(f"no unused aside name for {path}.corrupt-{stamp}")


def _read_bounded(path: str, now: float) -> list[dict]:
    """Entries of one ledger file. [] when it does not exist; Undetermined for
    anything else that is not a clean read — with the BRICK BOUND for a
    CORRUPT file: it refuses for
    RETRY_WINDOW_SECS from its mtime; after that it is moved aside to a
    unique, never-replaced name (see _move_aside), its
    valid DENY lines are salvaged into a fresh active file, and only the
    unparseable lines are dropped (named in a stderr notice).

    An UNREADABLE file (open/read fails: permissions, a planted symlink, an IO
    error) is not bounded: nothing can be salvaged from it, so moving it aside
    would turn "could not read the prior refusals" into "there were none"
    (issue #1182, CLAUDE.md 3). It refuses on every call, and is never moved
    aside, until a human fixes or removes it or a new user prompt clears it."""
    data = _read_bytes(path)  # Undetermined when unreadable: refuse, unbounded
    if data is None:
        return []
    entries, bad_lines = _parse(path, data)
    if not bad_lines:
        return entries
    where = ("is not UTF-8" if bad_lines == [0]
             else f"line {bad_lines[0] or bad_lines[-1]} is not a valid entry")
    bad = Undetermined(f"the deny ledger {path} {where}")
    try:
        age = now - os.lstat(path).st_mtime
    except OSError:
        raise bad  # cannot measure its age: refuse, unbounded
    if age <= RETRY_WINDOW_SECS:
        until = time.strftime("%H:%M:%S", time.localtime(
            now - age + RETRY_WINDOW_SECS))
        raise Undetermined(f"{bad}; refused until {until}, when the file "
                           "is moved aside") from bad
    # Salvage from the bytes already read: they are the bytes that were judged.
    salvaged = [e for e in entries if e["kind"] == "deny"]
    head = (f"maintree deny ledger: {path} was corrupt for "
            f"more than {RETRY_WINDOW_SECS // 60} minutes ({bad})")
    lines = ", ".join("whole file not UTF-8" if n == 0 else str(n) for n in bad_lines)
    try:
        aside = _move_aside(path, int(now))
    except OSError as e:
        aside = ""
        sys.stderr.write(f"{head}; could NOT move it aside ({e}); using its "
                         f"{len(salvaged)} valid deny line(s) for this call; "
                         f"dropped line(s): {lines}.\n")
        return salvaged
    note = ""
    if salvaged:
        try:
            fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL
                         | getattr(os, "O_NOFOLLOW", 0), 0o600)
            try:
                os.write(fd, "".join(
                    json.dumps(e, ensure_ascii=False, sort_keys=True) + "\n"
                    for e in salvaged).encode("utf-8"))
            finally:
                os.close(fd)
            note = f"; {len(salvaged)} valid deny line(s) carried into a fresh {path}"
        except OSError as e:
            note = (f"; its {len(salvaged)} valid deny line(s) could NOT be "
                    f"written back ({e}) and are used for this call only")
    sys.stderr.write(f"{head}; moved it aside to {aside}{note}; dropped "
                     f"unparseable line(s): {lines}.\n")
    return salvaged


def read(session_id: str, now: float | None = None) -> list[dict]:
    """Every entry of the session's ledgers (home + fallback), merged in epoch
    order. [] when neither file exists. Raises Undetermined for an unreadable
    file, and for a corrupt file still inside its brick bound."""
    now = time.time() if now is None else now
    out: list[dict] = []
    for path in ledger_paths(session_id):
        out.extend(_read_bounded(path, now))
    out.sort(key=lambda e: e["epoch"])  # stable: same-epoch lines keep order
    return out


def _append(session_id: str, obj: dict) -> None:
    """Append one line to the first ledger location that accepts it. Raises
    OSError when none does."""
    line = (json.dumps(obj, ensure_ascii=False, sort_keys=True) + "\n").encode("utf-8")
    errors = []
    for path in ledger_paths(session_id):
        try:
            if _is_fallback(path):
                if not _fallback_dir_safe(os.path.dirname(path), create=True):
                    errors.append(f"{os.path.dirname(path)}: not a 0700 directory "
                                  "owned by this user")
                    continue
            else:
                os.makedirs(os.path.dirname(path), mode=0o700, exist_ok=True)
            # One write() on an O_APPEND descriptor: concurrent hook processes
            # of the same session interleave whole lines, not fragments.
            fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_APPEND
                         | getattr(os, "O_NOFOLLOW", 0), 0o600)
            try:
                os.write(fd, line)
            finally:
                os.close(fd)
            return
        except OSError as e:
            errors.append(f"{path}: {e}")
    raise OSError("no deny ledger location is writable (" + "; ".join(errors) + ")")


# ---------------------------------------------------------------------------
# snapshot
# ---------------------------------------------------------------------------
def snapshot(target_abs: str, root: str | None) -> dict:
    """The target's state now. `status` is None when git could not answer."""
    snap: dict = {"exists": False, "is_dir": False, "mtime_ns": None, "size": None}
    try:
        st = os.lstat(target_abs)
        snap.update(exists=True, is_dir=os.path.isdir(target_abs),
                    mtime_ns=st.st_mtime_ns, size=st.st_size)
    except FileNotFoundError:
        pass
    except OSError:
        snap["exists"] = None  # could not tell
    status = None
    if root is not None:
        try:
            r = subprocess.run(
                ("git", "status", "--porcelain", "-z", "--untracked-files=all",
                 "--", target_abs),
                cwd=root, capture_output=True, timeout=10,
            )
            if r.returncode == 0:
                status = r.stdout.decode("utf-8", "surrogateescape")
        except (OSError, subprocess.SubprocessError):
            status = None
    snap["status"] = status
    return snap


# ---------------------------------------------------------------------------
# recording
# ---------------------------------------------------------------------------
def _first_line(text: str, limit: int = REASON_LIMIT) -> str:
    for line in (text or "").splitlines():
        if line.strip():
            return line.strip()[:limit]
    return ""


def record_deny(payload: dict, denier: str, target_abs: str | None,
                root: str | None, raw: str, reason: str,
                raw_target: str | None = None) -> str | None:
    """Append a deny entry for this payload's session. Returns None on success
    or when the payload has no session_id; otherwise a one-line note saying why
    the refusal could not be recorded (the caller appends it to its refusal —
    the deny itself stands either way)."""
    try:
        sid = session_of(payload)
        if sid is None:
            return None
        if target_abs is not None:
            target_abs = os.path.realpath(target_abs)
        snap = snapshot(target_abs, root) if target_abs is not None else None
        now = time.time()
        _append(sid, {
            "v": 1, "kind": "deny",
            "ts": time.strftime("%Y-%m-%dT%H:%M:%S%z", time.localtime(now)),
            "epoch": now, "session_id": sid, "denier": denier,
            "target_abs": target_abs, "root": root,
            "raw_target": raw_target if isinstance(raw_target, str) else None,
            "raw": raw[:RAW_LIMIT], "reason": _first_line(reason),
            "snapshot": snap,
        })
        return None
    except (Undetermined, OSError, ValueError) as e:
        return f"(this refusal could NOT be written to the deny ledger: {e})"


# ---------------------------------------------------------------------------
# signal 1: prevention
# ---------------------------------------------------------------------------
def _fold(p: str) -> str:
    return p.casefold() if _FOLD_CASE else p


def _under(child: str, parent: str) -> bool:
    child, parent = _fold(child), _fold(parent).rstrip("/")
    return child == parent or child.startswith(parent + "/")


def _strings(obj) -> list[str]:
    out: list[str] = []
    stack = [obj]
    while stack:
        cur = stack.pop()
        if isinstance(cur, str):
            out.append(cur)
        elif isinstance(cur, dict):
            stack.extend(cur.keys())
            stack.extend(cur.values())
        elif isinstance(cur, (list, tuple)):
            stack.extend(cur)
    return out


def _mentions(haystack: str, needle: str) -> bool:
    hay, nd = _fold(haystack), _fold(needle.rstrip("/")) or "/"
    start = 0
    while True:
        i = hay.find(nd, start)
        if i < 0:
            return False
        j = i + len(nd)
        if j >= len(hay) or not _NAME_CHAR.match(hay[j]):
            return True
        start = i + 1


_NOT_A_REPO = "not a git repository"


def _outside_repo_write_target(payload: dict, live: list[dict]) -> list[str] | None:
    """The call's write-target strings when that target is POSITIVELY outside
    every git repository; None otherwise (issue #1180).

    None — the restrictive answer, which keeps the full substring test — for:
    no file_path / notebook_path string, a relative one, a target under any
    live deny's recorded root (as recorded or by realpath), a git probe that
    could not run or timed out, a probe that found a repository, and a probe
    that failed for any reason other than git's own "not a git repository".
    The probe runs with LC_ALL=C (so that message is not localised), without
    GIT_CEILING_DIRECTORIES and with GIT_DISCOVERY_ACROSS_FILESYSTEM=1 (so
    neither can stop discovery short of an enclosing repository)."""
    ti = payload.get("tool_input")
    if not isinstance(ti, dict):
        return None
    raw = None
    for key in ("file_path", "notebook_path"):
        val = ti.get(key)
        if isinstance(val, str) and val:
            raw = val
            break
    if raw is None or not os.path.isabs(raw):
        return None
    target = os.path.realpath(raw)
    for e in live:
        root = e.get("root")
        if not isinstance(root, str) or not os.path.isabs(root):
            return None
        if (_under(target, root) or _under(target, os.path.realpath(root))
                or _under(os.path.abspath(raw), root)):
            return None
    d = os.path.dirname(target) or target
    while d and d != os.path.dirname(d) and not os.path.isdir(d):
        d = os.path.dirname(d)
    if not d or not os.path.isdir(d):
        return None
    env = {k: v for k, v in os.environ.items() if k != "GIT_CEILING_DIRECTORIES"}
    env["LC_ALL"] = "C"
    env["GIT_DISCOVERY_ACROSS_FILESYSTEM"] = "1"
    try:
        out = subprocess.run(["git", "rev-parse", "--absolute-git-dir"], cwd=d,
                             env=env, capture_output=True, text=True, timeout=10)
    except (OSError, subprocess.SubprocessError):
        return None
    if out.returncode == 0 or _NOT_A_REPO not in (out.stderr or ""):
        return None
    return [raw, target]


def quote(entry: dict) -> str:
    return (f"{entry['denier']} refused `{entry['raw']}` at {entry['ts']} "
            f"because {entry['reason']}")


def check_retry(payload: dict, now: float | None = None) -> str | None:
    """Signal 1. None when the call may proceed; otherwise the refusal text.
    Raises Undetermined (refuse) for an unusable session_id or ledger."""
    sid = session_of(payload)
    if sid is None:
        return None
    entries = read(sid)
    if not entries:
        return None
    now = time.time() if now is None else now
    active: list[dict] = []
    ticks_after: list[int] = []
    for e in entries:
        if e["kind"] == "tick":
            ticks_after = [t + 1 for t in ticks_after]
            continue
        active.append(e)
        ticks_after.append(0)
    live = [e for e, t in zip(active, ticks_after)
            if t < RETRY_WINDOW_CALLS and 0 <= now - e["epoch"] <= RETRY_WINDOW_SECS]
    if not live:
        return None
    haystack = "\n".join(_strings(payload.get("tool_input")))
    hit = None
    for e in reversed(live):
        target, root = e.get("target_abs"), e.get("root")
        if target is None:
            continue  # a "cannot determine" refusal names no path to match
        if root is None:
            raise Undetermined(f"a deny ledger entry for {target} records no main root")
        if not _under(target, root):
            continue  # no longer a main-tree target
        needles = [target]
        if os.path.isabs(e.get("raw_target") or ""):
            needles.append(e["raw_target"])
        if any(_mentions(haystack, n) for n in needles):
            hit = e
            break
    # Narrowing (issue #1180, user ruling 2026-10-25): a write whose target is
    # POSITIVELY outside every git repository (a scratch note, a report) is not
    # a retry merely because its content names the path. Only the target path
    # strings are matched then. Anything short of a positive "outside" keeps
    # the full substring test above (CLAUDE.md 3).
    if hit is not None:
        target_strings = _outside_repo_write_target(payload, live)
        if target_strings is not None:
            narrow = "\n".join(target_strings)
            hit = None
            for e in reversed(live):
                target, root = e.get("target_abs"), e.get("root")
                if target is None or not _under(target, root):
                    continue
                needles = [target]
                if os.path.isabs(e.get("raw_target") or ""):
                    needles.append(e["raw_target"])
                if any(_mentions(narrow, n) for n in needles):
                    hit = e
                    break
    # This call is one more guarded call inside the window of every live deny.
    # A tick that cannot be written leaves the count where it is; the time
    # bound still ends the window (RESIDUALS), so it does not refuse the call.
    try:
        _append(sid, {"v": 1, "kind": "tick", "epoch": now})
    except OSError as e:
        sys.stderr.write(f"maintree deny ledger: could not count this call ({e}).\n")
    if hit is None:
        return None
    return RETRY.format(quote=quote(hit), target=hit["target_abs"],
                        n=RETRY_WINDOW_CALLS, mins=RETRY_WINDOW_SECS // 60,
                        path=" / ".join(ledger_paths(sid)))


RETRY = """{quote}. This call reaches the same target {target} by another spelling; confirm it is genuinely a different approach.

(maintree deny ledger, backlog e033c406: a target refused in this session is held
for {n} guarded calls or {mins} minutes, whichever ends first. Any later call naming
it is refused unless a human confirms it. Do the change in a git worktree instead —
a worktree path is a different target and is not affected. Ledger: {path})
"""

UNDETERMINED = """Refused: the maintree deny ledger could not be consulted ({why}).

Earlier refusals in this session are recorded so that the same target cannot be
re-reached by another spelling (backlog e033c406). A ledger this gate could not
read is not a ledger that says "no prior refusals" (CLAUDE.md 最上位の方針 3), so
this call is refused rather than allowed. A human can inspect or remove the file;
a new user prompt clears this session's ledger. A corrupt file is refused only
for {mins} minutes from its last change, then moved aside; an unreadable file
is refused until it is repaired, removed or cleared.
"""


# ---------------------------------------------------------------------------
# ask / deny emission (PreToolUse)
# ---------------------------------------------------------------------------
def ask_available() -> bool:
    """True only in an affirmatively interactive terminal session — the same
    positive test as crates/blastguard/src/interactive.rs."""
    return (os.environ.get("CLAUDECODE") == "1"
            and os.environ.get("CLAUDE_CODE_ENTRYPOINT") == "cli")


def emit_ask(reason: str) -> int:
    """Refuse with an ask when a human can answer, else harden to a deny.
    Returns the exit code the hook must exit with."""
    if ask_available():
        sys.stdout.write(json.dumps({"hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "ask",
            "permissionDecisionReason": reason,
        }}))
        return 0
    sys.stderr.write(reason)
    return 2


def gate(payload: dict) -> int | None:
    """Run signal 1 for a call the guard's own judgement allowed. Returns None
    to allow, or the exit code after emitting an ask / deny."""
    try:
        msg = check_retry(payload)
    except (Undetermined, OSError) as e:
        return emit_ask(UNDETERMINED.format(why=e, mins=RETRY_WINDOW_SECS // 60))
    if msg is None:
        return None
    return emit_ask(msg)


# ---------------------------------------------------------------------------
# signal 2: detection at Stop
# ---------------------------------------------------------------------------
def _changed(old: dict, new: dict) -> bool:
    if old.get("status") != new.get("status"):
        return True
    if not new.get("status"):
        return False  # clean against HEAD now: HEAD moved, or nothing changed
    return any(old.get(k) != new.get(k) for k in ("exists", "is_dir", "mtime_ns", "size"))


def stop_check(payload: dict) -> str | None:
    """Signal 2. None to allow the stop; otherwise the block reason."""
    try:
        sid = session_of(payload)
        if sid is None:
            return None
        entries = read(sid)
    except (Undetermined, OSError) as e:
        return STOP_UNDETERMINED.format(why=e)
    for e in entries:
        if e["kind"] != "deny" or e.get("target_abs") is None:
            continue
        target, root, old = e["target_abs"], e.get("root"), e.get("snapshot")
        if root is None or not isinstance(old, dict):
            return STOP_UNDETERMINED.format(
                why=f"the deny of {target} recorded no root or snapshot")
        if not _under(target, root):
            continue
        if old.get("status") is None or old.get("exists") is None:
            return STOP_UNDETERMINED.format(
                why=f"the state of {target} could not be read when it was refused")
        new = snapshot(target, root)
        if new.get("status") is None or new.get("exists") is None:
            return STOP_UNDETERMINED.format(
                why=f"the state of {target} cannot be read now")
        if _changed(old, new):
            return STOP_CHANGED.format(quote=quote(e), target=target)
    return None


STOP_CHANGED = """Do not stop yet: a main-tree path that was REFUSED earlier in this session has changed since.

{quote}.
{target} is no longer in the state it was in when that refusal was recorded, so
the refused change was made anyway by some other route (backlog e033c406).

If this session made it: undo it on main and redo it in a git worktree. If
another session changed this path, say so explicitly in your report — do not
leave it unexplained.
"""

STOP_UNDETERMINED = """Do not stop yet: the maintree deny ledger could not be checked ({why}).

Whether a path refused earlier in this session was changed anyway cannot be
determined, and a check that could not run has not passed (CLAUDE.md 3).
"""


# ---------------------------------------------------------------------------
# lifecycle (UserPromptSubmit)
# ---------------------------------------------------------------------------
PRUNE_AFTER_SECS = 7 * 24 * 3600


def clear(payload: dict) -> str | None:
    """Remove this session's ledgers (home and fallback) and prune any ledger
    file, of any session, older than a week in either directory. A fallback
    directory that is not a 0700 directory owned by this uid is not touched
    (it is not read either). None on success; otherwise a note describing
    what failed."""
    try:
        sid = session_of(payload)
    except Undetermined as e:
        return str(e)
    notes = []
    fb = fallback_dir()
    fb_ok = fb is not None and _fallback_dir_safe(fb, create=False)
    dirs = [fb] if fb_ok else []
    try:
        dirs.insert(0, ledger_dir())
    except Undetermined as e:
        notes.append(str(e))
    if sid is not None:
        for path in ledger_paths(sid):
            if _is_fallback(path) and not fb_ok:
                continue
            try:
                os.remove(path)
            except FileNotFoundError:
                continue
            except OSError as e:
                notes.append(f"could not clear {path}: {e}")
    now = time.time()
    for dd in dirs:
        try:
            names = os.listdir(dd)
        except FileNotFoundError:
            continue
        except OSError as e:
            notes.append(f"could not list {dd}: {e}")
            continue
        for name in names:
            if ".jsonl" not in name:
                continue
            p = os.path.join(dd, name)
            try:
                if now - os.stat(p).st_mtime > PRUNE_AFTER_SECS:
                    os.remove(p)
            except OSError as e:
                # pruning another session's stale file is housekeeping only
                notes.append(f"could not prune {p}: {e}")
    return "; ".join(notes) or None
