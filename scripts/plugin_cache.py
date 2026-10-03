"""Shared facts about the deployed plugin cache.

Two consumers import this: the GATE (check-plugin-rollout.py), which reports what
is wrong, and the PRUNER (prune-plugin-cache.py), which deletes stale version
dirs. They must agree on what "stale" and "in use" mean. Writing the liveness
rule twice — once in Python for the gate, once in shell for the rollout script —
would be exactly the divergence that lets a dir be reported as removable while
the remover refuses to touch it (or worse, the reverse).

The cache layout is:

    <cache>/<plugin-name>/<version>/        one dir per version ever installed
    <cache>/<plugin-name>/<version>/.in_use/<pid>   a live session holding it

`.in_use` entries are named by the PID of the `claude` process that loaded that
version, plus `.tmp.<hex>` leftovers from interrupted writes. The markers are
NOT cleaned up when a session exits, so the mere presence of `.in_use` proves
nothing — measured 2026-07-26: scout 0.1.0 carried 64 markers and every pid in
it was dead. Liveness has to be asked of the OS, per pid. The ABSENCE of a
marker proves nothing either: Claude Code writes `.in_use` for only some
plugins, so a superseded dir is also held by any live `claude` process that
started before it was superseded (session_age_holds, backlog 18fe626f).

Undetermined is resolved to each consumer's OWN restrictive side, which is not
the same side for both:

  - the PRUNER treats "cannot tell if it is held" as HELD and keeps the dir,
    because deleting is the irreversible action;
  - the GATE treats it as a reported problem, because "I could not inspect the
    cache" is not "the cache is clean".

Same fact, opposite safe directions — which is why the tri-state is preserved
here instead of being collapsed to a bool by whichever caller got there first.
"""
import errno
import os
import re

# A version dir entry that is a live-session marker rather than a payload file.
IN_USE_DIR = ".in_use"
_PID_RE = re.compile(r"^[0-9]+$")


class Holders:
    """Who is holding a version dir. `undetermined` is not "nobody"."""

    __slots__ = ("live_pids", "undetermined", "pinned", "registered", "sessions")

    def __init__(
        self, live_pids=(), undetermined=None, pinned=(), registered=(), sessions=()
    ):
        self.live_pids = tuple(live_pids)
        self.undetermined = undetermined
        self.pinned = tuple(pinned)
        self.registered = tuple(registered)
        # pids of live `claude` processes that STARTED BEFORE this dir was
        # superseded (see session_age_holds): they may have pinned it as their
        # CLAUDE_PLUGIN_ROOT, with or without an `.in_use` marker.
        self.sessions = tuple(sessions)

    @property
    def held(self):
        """True if the dir must not be removed: live holders, unknown, a
        settings.json pin (a hardcoded absolute path outside the registry's
        current-version pointer, which the pruner has no other visibility
        into — see settings_pinned_versions), or an installed_plugins.json
        entry pointing at it (see registry_referenced_versions: deleting the
        dir the registry points at is, by definition, making the plugin
        dark), or a live `claude` process older than the dir's supersession
        (see session_age_holds)."""
        return (
            bool(self.live_pids)
            or self.undetermined is not None
            or bool(self.pinned)
            or bool(self.registered)
            or bool(self.sessions)
        )

    def __repr__(self):  # pragma: no cover - diagnostics only
        return (
            f"Holders(live_pids={self.live_pids}, "
            f"undetermined={self.undetermined!r}, pinned={self.pinned}, "
            f"registered={self.registered}, sessions={self.sessions})"
        )


def _pid_alive_windows(pid):
    """Windows has no signal-0 existence probe: `os.kill(pid, 0)` on Windows
    does not raise ProcessLookupError for a dead pid — CPython maps it to a
    bare OSError (WinError 87, "the parameter is incorrect") indistinguishable
    from a real failure, so pid_alive()'s except chain fell through to `None`
    for every pid, always (measured 2026-08-04: every prune run on this
    platform reported 100% of stale dirs as undetermined-held, so nothing was
    ever prunable and check-plugin-rollout.py could never report clean).
    OpenProcess is the actual Win32 existence probe: a live pid returns a
    handle (closed immediately, we only wanted the answer); a dead pid fails
    with ERROR_INVALID_PARAMETER (87); a live pid we lack rights to query
    (owned by another user) fails with ERROR_ACCESS_DENIED (5) — still alive,
    same EPERM-is-alive reasoning as the POSIX branch below. Any other error
    is genuinely undetermined.
    """
    import ctypes

    PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    handle = kernel32.OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, False, pid)
    if handle:
        kernel32.CloseHandle(handle)
        return True
    err = ctypes.get_last_error()
    if err == 5:  # ERROR_ACCESS_DENIED
        return True
    if err == 87:  # ERROR_INVALID_PARAMETER
        return False
    return None


def pid_alive(pid):
    """Is `pid` a live process? True / False / None (cannot tell).

    Signal 0 performs the permission and existence checks without delivering
    anything. EPERM means the process exists but belongs to someone else, which
    is still ALIVE — reading that as dead is how a liveness check turns into a
    delete-someone-else's-running-plugin bug. Windows has no working signal-0
    probe, so it is dispatched to _pid_alive_windows instead (see its
    docstring).
    """
    if os.name == "nt":
        return _pid_alive_windows(pid)
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    except OSError:
        return None
    return True


def holders_of(version_dir):
    """Which live sessions hold `version_dir`."""
    marker_dir = os.path.join(version_dir, IN_USE_DIR)
    try:
        entries = os.listdir(marker_dir)
    except FileNotFoundError:
        return Holders()
    except OSError as exc:
        return Holders(undetermined=f"cannot list {marker_dir}: {exc}")

    live = []
    for name in entries:
        # `.tmp.<hex>` leftovers from interrupted marker writes carry no pid.
        if not _PID_RE.match(name):
            continue
        state = pid_alive(int(name))
        if state is None:
            return Holders(undetermined=f"cannot determine whether pid {name} is alive")
        if state:
            live.append(int(name))
    return Holders(live_pids=sorted(live))


def source_versions(crates_dir):
    """Return (name -> version, problems) read from every crates/*/plugin.json.

    A crate whose plugin.json cannot be read is reported in `problems` rather
    than dropped: a plugin missing from this map would make every one of its
    cached version dirs look stale, and the pruner would then be aimed at the
    live one.
    """
    import json

    versions = {}
    problems = []
    try:
        names = sorted(os.listdir(crates_dir))
    except OSError as exc:
        return {}, [f"cannot list {crates_dir}: {exc}"]
    for d in names:
        pj = os.path.join(crates_dir, d, ".claude-plugin", "plugin.json")
        if not os.path.isfile(pj):
            continue
        try:
            with open(pj, "r", encoding="utf-8") as fh:
                data = json.load(fh)
        except (OSError, ValueError) as exc:
            problems.append(f"{d}: unreadable plugin.json ({exc})")
            continue
        name, ver = data.get("name"), data.get("version")
        if not name or not ver:
            problems.append(f"{d}: plugin.json has no name/version")
            continue
        versions[name] = ver
    return versions, problems


class StaleDir:
    """A cached version dir that is not the plugin's current version.

    `kind` is "dir" for an ordinary version dir and "dangling-link" for a
    symlink whose target no longer exists. The pruner needs the distinction
    because the two are removed by different calls (rmtree vs unlink), and the
    log needs it because "pruned condukt/0.4.2" reads as though a version had
    been reclaimed when in fact only a broken pointer was.
    """

    __slots__ = ("plugin", "version", "path", "holders", "kind")

    def __init__(self, plugin, version, path, holders, kind="dir"):
        self.plugin = plugin
        self.version = version
        self.path = path
        self.holders = holders
        self.kind = kind

    @property
    def removable(self):
        return not self.holders.held

    def describe(self):
        # What it IS and why it was KEPT are two separate facts, so the kind is
        # a prefix rather than an early return: a dangling link kept because
        # settings.json would not parse used to print only "(dangling symlink
        # -> x)" and never say why the pruner left it alone.
        base = f"{self.plugin}/{self.version}"
        if self.kind == "dangling-link":
            try:
                target = os.readlink(self.path)
            except OSError:
                target = "?"
            base = f"{base} (dangling symlink -> {target})"
        if self.holders.undetermined:
            return f"{base} (undetermined: {self.holders.undetermined})"
        if self.holders.registered:
            refs = ", ".join(sorted(self.holders.registered))
            return f"{base} (referenced by installed_plugins.json: {refs})"
        if self.holders.pinned:
            refs = ", ".join(sorted(self.holders.pinned))
            return f"{base} (pinned by settings.json: {refs})"
        if self.holders.live_pids:
            pids = ",".join(str(p) for p in self.holders.live_pids)
            return f"{base} (in use by pid {pids})"
        if self.holders.sessions:
            pids = ",".join(str(p) for p in self.holders.sessions)
            return (
                f"{base} (held by live claude pid {pids}, started before this "
                "version was superseded)"
            )
        return base


def _link_resolution(path):
    """Classify a cache entry that is not a directory.

    Returns (state, reason) where state is one of:

      "dangling"     — a symlink whose target provably does not exist. Safe to
                       unlink: it addresses nothing and holds no bytes.
      "undetermined" — it IS a symlink, but whether it resolves could not be
                       decided (permissions on a path component, an IO error).
                       `reason` says which errno. Never removable.
      "other"        — not a symlink at all (a plain file, a socket, …), or a
                       symlink that resolves to a non-directory. Reported and
                       left alone.

    The three-way split exists because `os.path.exists()` collapses the first
    two: it returns False both for ENOENT and for EACCES, and the pruner acts
    on that bool by deleting. See the call site for the measurement.
    """
    if not os.path.islink(path):
        return "other", None
    try:
        os.stat(path)  # follows the link; raises with the reason it could not
    except FileNotFoundError as exc:
        return "dangling", str(exc)
    except NotADirectoryError as exc:
        # A path component of the target is a file: the target cannot exist.
        return "dangling", str(exc)
    except OSError as exc:
        if exc.errno == errno.ELOOP:
            # A cycle resolves to nothing by construction, and rmtree/readlink
            # on it can never reach a payload. Removable for the same reason
            # ENOENT is: there is nothing on the other end to lose.
            return "dangling", str(exc)
        return "undetermined", f"{errno.errorcode.get(exc.errno, exc.errno)}: {exc}"
    return "other", None


def scan(
    cache_root,
    current_versions,
    settings_pins=None,
    settings_undetermined=None,
    registry_refs=None,
    registry_undetermined=None,
):
    """Return (stale_dirs, problems) for every plugin dir under `cache_root`.

    A plugin present in the cache but absent from `current_versions` is reported
    as a problem and NONE of its dirs are listed stale. Treating "I don't know
    which version is current" as "every version is stale" would hand the pruner
    the live dir.

    `settings_pins` (from settings_pinned_versions) marks (plugin, version)
    pairs referenced by an absolute path in settings.json — these carry a
    holder just like a live `.in_use` pid does, even with zero markers on
    disk. `settings_undetermined`, if set, means settings.json existed but
    could not be read/parsed; every dir is then treated as pinned, because
    "could not check for a pin" must resolve to the same restrictive side as
    "found a pin", not to "found no pins".

    `registry_refs` / `registry_undetermined` (from
    registry_referenced_versions) are the same contract for
    installed_plugins.json: a (plugin, version) the registry points at is held,
    and an unreadable registry holds EVERY dir. "Not the version in THIS tree"
    is not "nobody uses it" — measured 2026-09-07 (backlog e3366b5c): a
    session whose tree was at specguard 0.2.55 pruned the 0.2.56 another
    session had just deployed and registered, and specguard went dark.

    The rollout lock dir (`ROLLOUT_LOCK_NAME`) at the cache root is not a
    plugin and is skipped by exact name; any other non-plugin entry is still
    reported.
    """
    settings_pins = settings_pins or {}
    registry_refs = registry_refs or {}
    # Either source being unreadable means "could not check for a holder":
    # every dir is kept, same as a found holder.
    undetermined_all = settings_undetermined or registry_undetermined
    stale = []
    problems = []
    try:
        plugin_names = sorted(os.listdir(cache_root))
    except FileNotFoundError:
        # Deliberate, and the one empty-set return in this module that is NOT
        # the fail-open CLAUDE.md 3. forbids: ENOENT is a DETERMINATE
        # observation ("there is no cache"), not a failure to observe. Nothing
        # is cached, so there is nothing stale and nothing uninspected. Every
        # other errno — EACCES, ENOTDIR, EIO — falls to the clause below and
        # becomes a problem, because those mean "could not look", and the gate
        # (check-plugin-rollout.py) turns each problem into a red. Splitting on
        # errno here is the same discrimination _link_resolution makes for the
        # entries inside; do not collapse it back to a bare `except OSError`.
        return [], []
    except OSError as exc:
        return [], [f"cannot list plugin cache {cache_root}: {exc}"]

    for pname in plugin_names:
        if pname == ROLLOUT_LOCK_NAME:
            # rollout-plugins.sh's exclusive lock (a dir with a pid file). The
            # pruner runs INSIDE a locked rollout, so it always sees it; it is
            # not a plugin and holds no version dirs.
            continue
        pdir = os.path.join(cache_root, pname)
        if not os.path.isdir(pdir):
            # The same skip the version loop below used to have, one level up,
            # and it drops exactly the shape of the incident this module is
            # about: a broken pointer at <cache>/<plugin> reads as "0 stale,
            # 0 problems" just as one at <cache>/<plugin>/<version> did.
            # Nothing here can be pruned — there is no version to reason about,
            # so no StaleDir can describe it — but it must not be silent.
            problems.append(
                f"{pname}: {pdir} is in the plugin cache but is not a plugin "
                "dir — left in place, since deletion is the irreversible "
                "action here"
            )
            continue
        cur = current_versions.get(pname)
        if cur is None:
            problems.append(
                f"{pname}: cached but no current version known from crates/ — "
                "cannot tell which of its dirs is live, so none are pruned"
            )
            continue
        try:
            vers = sorted(os.listdir(pdir))
        except OSError as exc:
            problems.append(f"{pname}: cannot list {pdir}: {exc}")
            continue
        for v in vers:
            vdir = os.path.join(pdir, v)
            # The non-dir check comes BEFORE the `v == cur` skip on purpose. A
            # broken pointer sitting on the CURRENT version is the worst case of
            # all — the live plugin is not on disk — and skipping it as "current,
            # nothing to do" would report `0 stale, 0 problems` about exactly
            # that. It is still never removable: the current version is never
            # deleted by this pruner, so it is raised as a problem instead.
            if not os.path.isdir(vdir):
                # Not a version dir. This branch used to `continue` silently,
                # which made any non-dir entry in the cache read as "nothing
                # stale here" — the fail-open CLAUDE.md 3. forbids. Measured
                # 2026-09-08 at 610b47e4: condukt/0.4.2 had been a symlink to a
                # long-pruned 0.6.0 since 2026-07-02, and every prune run
                # reported "0 stale dir(s)" with it sitting there.
                #
                # DANGLING means islink AND does not resolve. `islink` alone is
                # NOT enough: it is true for a link that points at a FILE too,
                # and `isdir` is false for that link, so testing `islink` by
                # itself lands a perfectly resolvable pointer in this branch and
                # deletes it. Measured 2026-09-08: `link -> real.txt` reports
                # islink=True, isdir=False, exists=True, and the first version of
                # this branch called it "dangling symlink -> real.txt" and
                # removed it — deleting the only reference to a file that still
                # existed, on a guess, while the pruner's own docstring promised
                # the opposite. `exists()` follows the link, so it is the half
                # that actually asks whether the pointer resolves.
                #
                # A truly dangling link addresses nothing and holds no bytes, so
                # nothing can be lost by unlinking it: stale, not a problem —
                # unless settings.json itself could not be read, in which case
                # the same "could not check for a pin" rule that keeps every
                # real dir keeps this too.
                #
                # Anything else (a plain file, a link to a file) is unaccounted
                # state: report it and leave it, because deletion is the
                # irreversible action and "I do not know what this is" must not
                # resolve to a delete.
                #
                # `exists()` is NOT the right question either, because it
                # answers False for two different facts: "the target is gone"
                # (ENOENT) and "I was not allowed to look" (EACCES). Measured
                # 2026-09-08: with the parent chmod 000, a link at a live
                # directory resolved as `exists=False`, was described as
                # "dangling symlink -> <target>", and was UNLINKED with exit 0
                # — the pointer to live data deleted because the check could
                # not read it. Ask errno instead: only ENOENT (and ELOOP, a
                # cycle that resolves to nothing by construction) is dangling;
                # every other failure is undetermined and is reported, not
                # removed.
                link_state, link_reason = _link_resolution(vdir)
                if link_state == "dangling":
                    if v == cur:
                        problems.append(
                            f"{pname}: the CURRENT version dir {vdir} is a "
                            "dangling symlink — the plugin is not on disk at "
                            "all. Left in place (the current version is never "
                            f"pruned); re-run the rollout for {pname}"
                        )
                        continue
                    h = Holders(
                        undetermined=undetermined_all,
                        registered=registry_refs.get((pname, v), ()),
                    )
                    stale.append(StaleDir(pname, v, vdir, h, kind="dangling-link"))
                elif link_state == "undetermined":
                    problems.append(
                        f"{pname}: cannot tell what {vdir} is — {link_reason}. "
                        "Left in place: an uninspectable entry is not a "
                        "dangling pointer, and deleting on that guess is how "
                        "live data is lost"
                    )
                else:
                    problems.append(
                        f"{pname}: {vdir} is in the plugin cache but is not a "
                        "version dir — left in place, since deletion is the "
                        "irreversible action here"
                    )
                continue
            if v == cur:
                continue
            h = holders_of(vdir)
            h = Holders(
                live_pids=h.live_pids,
                undetermined=h.undetermined or undetermined_all,
                pinned=settings_pins.get((pname, v), ()),
                registered=registry_refs.get((pname, v), ()),
            )
            stale.append(StaleDir(pname, v, vdir, h))
    return stale, problems


ROLLOUT_LOCK_NAME = ".rollout.lock"


def default_registry_path():
    """installed_plugins.json, honouring the same env var rollout-plugins.sh
    and check-plugin-rollout.py do."""
    override = os.environ.get("CLAUDE_PLUGIN_REGISTRY")
    if override:
        return override
    return os.path.expanduser("~/.claude/plugins/installed_plugins.json")


def registry_referenced_versions(cache_root, path=None):
    """Which (plugin, version) dirs under `cache_root` installed_plugins.json
    points at.

    Returns (refs, undetermined):
      - refs: dict[(plugin, version) -> tuple of "<key>@<version>" strings].
        An entry counts if its `installPath` resolves to
        `<cache_root>/<plugin>/<version>`. A missing registry file (ENOENT)
        is a determinate "nothing is registered" and contributes no refs.
      - undetermined: None, or a reason string when the registry exists but
        could not be read, parsed, or does not have the expected shape. The
        caller MUST then treat every dir as held (see scan()): "could not
        read which dir is live" is not "no dir is live", and the action
        waiting on this answer is an irreversible delete.
    """
    import json

    path = path or default_registry_path()
    try:
        with open(path, "r", encoding="utf-8") as fh:
            data = json.load(fh)
    except FileNotFoundError:
        return {}, None
    except (OSError, ValueError) as exc:
        return {}, f"{path}: unreadable or unparseable ({exc})"

    plugins = data.get("plugins") if isinstance(data, dict) else None
    if not isinstance(plugins, dict):
        return {}, f"{path}: no \"plugins\" object"
    root = os.path.realpath(cache_root)
    refs = {}
    for key, entries in plugins.items():
        if not isinstance(entries, list):
            return {}, f"{path}: entry {key!r} is not a list"
        for ent in entries:
            ip = ent.get("installPath") if isinstance(ent, dict) else None
            if not isinstance(ip, str) or not ip:
                return {}, f"{path}: entry {key!r} has no installPath"
            # Resolve only the cache-root part: the <plugin>/<version> tail is
            # compared by NAME, so a registry pointing at a version entry that
            # is itself a symlink holds that entry, not its target.
            norm = os.path.normpath(ip)
            ver = os.path.basename(norm)
            pname = os.path.basename(os.path.dirname(norm))
            if os.path.realpath(os.path.dirname(os.path.dirname(norm))) != root:
                continue  # another marketplace's cache, or elsewhere
            refs.setdefault((pname, ver), set()).add(f"{key}@{ver}")
    return {k: tuple(sorted(v)) for k, v in refs.items()}, None


def default_cache_root():
    """The plugin cache root, honouring the same env var rollout-plugins.sh does."""
    override = os.environ.get("CLAUDE_PLUGIN_CACHE")
    if override:
        return override
    return os.path.expanduser("~/.claude/plugins/cache/yukineko")


def settings_json_paths():
    """Which settings.json file(s) to scan for hardcoded cache-dir pins.

    Incident 2026-07-27: only ~/.claude/settings.json carried the stale
    absolute paths that broke, so that is the default. CLAUDE_SETTINGS_JSON
    (a PATHSEP-separated list) lets tests and any future project-level
    settings.json be added without touching this function again.
    """
    override = os.environ.get("CLAUDE_SETTINGS_JSON")
    if override:
        return [p for p in override.split(os.pathsep) if p]
    return [os.path.expanduser("~/.claude/settings.json")]


def settings_pinned_versions(cache_root, paths=None):
    """Which (plugin, version) pairs are referenced by an absolute path
    under `cache_root` somewhere in settings.json.

    Returns (pins, undetermined):
      - pins: dict[(plugin, version) -> set of raw matched path strings].
        A missing settings.json file is not an error — it contributes no
        pins and is NOT the same as "could not check".
      - undetermined: None, or a reason string if a settings.json file
        exists but could not be read/parsed. When set, the caller MUST treat
        every version dir as potentially pinned (see scan()) rather than
        reading "we couldn't check the pins" as "there are no pins" ahead of
        an irreversible delete.
    """
    import json

    paths = paths if paths is not None else settings_json_paths()
    root = cache_root.rstrip("/")
    pin_re = re.compile(re.escape(root) + r"/([^/\"'\s]+)/(\d+\.\d+\.\d+)")
    pins = {}
    undetermined = None
    for path in paths:
        if not os.path.isfile(path):
            continue
        try:
            with open(path, "r", encoding="utf-8") as fh:
                text = fh.read()
            json.loads(text)
        except (OSError, ValueError) as exc:
            undetermined = f"{path}: unreadable or unparseable ({exc})"
            continue
        for m in pin_re.finditer(text):
            key = (m.group(1), m.group(2))
            pins.setdefault(key, set()).add(m.group(0))
    return pins, undetermined


def cache_command_paths(cache_root, paths=None):
    """Every full filesystem path under `cache_root` mentioned anywhere in
    settings.json (a hook or statusLine `command`'s executable, typically).

    Unlike settings_pinned_versions (which extracts only the plugin/version
    pair), this returns the FULL path so a caller can check whether the file
    it names still exists. That existence check is what closed the
    2026-07-27 hole: check-plugin-rollout.py reported "OK" while 8
    hooks/statusLine entries referenced an already-pruned dir, because
    nothing verified a settings.json-referenced path was still there.

    Returns (paths, undetermined) with the same fail-closed contract as
    settings_pinned_versions: a missing settings.json contributes no paths;
    an unreadable/unparseable one sets `undetermined` rather than silently
    reporting zero paths.
    """
    import json

    paths_arg = paths if paths is not None else settings_json_paths()
    root = cache_root.rstrip("/")
    path_re = re.compile(re.escape(root) + r"/[^\"'\s]+")
    found = set()
    undetermined = None
    for path in paths_arg:
        if not os.path.isfile(path):
            continue
        try:
            with open(path, "r", encoding="utf-8") as fh:
                text = fh.read()
            json.loads(text)
        except (OSError, ValueError) as exc:
            undetermined = f"{path}: unreadable or unparseable ({exc})"
            continue
        for m in path_re.finditer(text):
            found.add(m.group(0))
    return found, undetermined


# ---------------------------------------------------------------------------
# Session-age hold (backlog 18fe626f, user ruling 2026-10-03)
#
# A running Claude Code session pins CLAUDE_PLUGIN_ROOT when it STARTS, and
# Claude Code writes `.in_use/<pid>` for only some plugins. So "no live marker"
# is not "no live user": prune used to delete a dir a live session was still
# executing hooks from, and that session went dark. The environment of another
# process cannot be read on macOS, so the hold is by PROCESS START TIME:
#
#     a superseded dir is held while any live `claude` process that started
#     strictly before the dir was superseded is alive.
#
# Both inputs have a test seam, read from os.environ at CALL time:
#   PLUGIN_CACHE_PROC_LIST_PROBE        shell command printing
#                                       `<pid> <start_epoch> <comm>` lines
#   PLUGIN_CACHE_SUPERSEDED_AT_OVERRIDE JSON {"<plugin>/<version>": epoch}
# ---------------------------------------------------------------------------

PROC_LIST_PROBE_ENV = "PLUGIN_CACHE_PROC_LIST_PROBE"
SUPERSEDED_AT_OVERRIDE_ENV = "PLUGIN_CACHE_SUPERSEDED_AT_OVERRIDE"
_CLAUDE_COMM = "claude"
_INT_RE = re.compile(r"^[0-9]+$")
_ETIME_RE = re.compile(r"^(?:(?:([0-9]+)-)?([0-9]{1,2}):)?([0-9]{1,2}):([0-9]{2})$")


def _parse_probe_lines(text):
    """`<pid> <start_epoch> <comm>` lines -> (list of (pid, start, comm), reason).

    Every non-blank line must parse; one bad line makes the WHOLE answer
    undetermined — a good line next to it does not launder it. Zero parsed
    lines is undetermined too: the pruner itself is a live process, so a
    working probe can never list nothing, and an empty answer read as "no
    sessions" would be the empty-set fail-open CLAUDE.md 3. forbids."""
    procs = []
    for raw in text.splitlines():
        if not raw.strip():
            continue
        parts = raw.split(None, 2)
        if (
            len(parts) != 3
            or not _INT_RE.match(parts[0])
            or not _INT_RE.match(parts[1])
            or not parts[2].strip()
        ):
            return None, f"unparseable process-list line {raw!r}"
        procs.append((int(parts[0]), int(parts[1]), parts[2].strip()))
    if not procs:
        return None, "process list is empty (a working probe always lists at least this process)"
    return procs, None


def _etime_seconds(etime):
    m = _ETIME_RE.match(etime)
    if not m:
        return None
    days, hours, mins, secs = (int(g) if g else 0 for g in m.groups())
    return ((days * 24 + hours) * 60 + mins) * 60 + secs


def _ps_process_list():
    """Production probe: `ps -A -o pid=,etime=,comm=`, start = now - etime.

    etime (elapsed wall time) is used instead of lstart because it carries no
    locale or timezone: converting lstart back to an epoch needs the local
    zone and is ambiguous across a DST change. The derived start is rounded
    DOWN by one extra second, so the error can only make a process look OLDER
    (more holding), never younger. The exit status is checked: a ps that
    failed is not a ps that found no sessions."""
    import subprocess
    import time

    try:
        p = subprocess.run(
            ["ps", "-A", "-o", "pid=,etime=,comm="],
            capture_output=True,
            text=True,
            timeout=30,
        )
    except (OSError, subprocess.SubprocessError) as exc:
        return None, f"cannot run ps: {exc}"
    if p.returncode != 0:
        return None, f"ps exited {p.returncode}: {p.stderr.strip()[:200]}"
    now = int(time.time())
    procs = []
    for raw in p.stdout.splitlines():
        if not raw.strip():
            continue
        parts = raw.split(None, 2)
        secs = _etime_seconds(parts[1]) if len(parts) == 3 else None
        if secs is None or not _INT_RE.match(parts[0]) or not parts[2].strip():
            return None, f"unparseable ps line {raw!r}"
        procs.append((int(parts[0]), now - secs - 1, parts[2].strip()))
    if not procs:
        return None, "ps listed no processes (it always lists at least itself)"
    return procs, None


def live_claude_processes():
    """Live `claude` processes as (pid, start_epoch) pairs.

    Returns (procs, undetermined). `undetermined` is a reason string whenever
    the process list could not be read in full — non-zero exit, any
    unparseable line, or an empty list — and the caller MUST then hold every
    dir. A process is a `claude` session iff the basename of its comm is
    exactly `claude` (observed 2026-10-03: the CLI shows as `claude` in
    `ps -o comm=`; zsh/node/the desktop app's `Claude` do not hold)."""
    import subprocess

    probe = os.environ.get(PROC_LIST_PROBE_ENV)
    if probe:
        try:
            p = subprocess.run(
                probe, shell=True, capture_output=True, text=True, timeout=30
            )
        except (OSError, subprocess.SubprocessError) as exc:
            return None, f"{PROC_LIST_PROBE_ENV}: cannot run probe: {exc}"
        if p.returncode != 0:
            return None, f"{PROC_LIST_PROBE_ENV}: probe exited {p.returncode}"
        procs, why = _parse_probe_lines(p.stdout)
        if why:
            return None, f"{PROC_LIST_PROBE_ENV}: {why}"
    else:
        procs, why = _ps_process_list()
        if why:
            return None, why
    return [
        (pid, start)
        for pid, start, comm in procs
        if os.path.basename(comm) == _CLAUDE_COMM
    ], None


def _iso_epoch(value):
    """`2026-10-03T01:02:03.456Z` -> int epoch seconds (rounded UP: a later
    superseded-at holds more, never less), or None."""
    import calendar
    import math
    import time

    if not isinstance(value, str) or not value.endswith("Z"):
        return None
    body = value[:-1]
    frac = 0.0
    if "." in body:
        body, _, f = body.partition(".")
        if not _INT_RE.match(f):
            return None
        frac = float("0." + f)
    try:
        t = time.strptime(body, "%Y-%m-%dT%H:%M:%S")
    except ValueError:
        return None
    return int(math.ceil(calendar.timegm(t) + frac))


def superseded_at_by_plugin(cache_root, path=None):
    """When each plugin's registry entry was last repointed, as epoch seconds.

    Returns (by_plugin, reasons): by_plugin maps plugin -> epoch; reasons maps
    plugin -> why it could not be derived. A plugin in neither has no registry
    entry under `cache_root`.

    WHY THIS SOURCE. A session pins the dir that installed_plugins.json points
    at when it starts, so a version dir stops being picked up by new sessions
    exactly when the registry is repointed away from it, and the registry entry
    records that instant as `lastUpdated` (rollout-plugins.sh writes it on every
    repoint; so does `claude plugin`). For every non-registered version dir of
    that plugin the TRUE superseded-at is at or before the CURRENT entry's
    `lastUpdated`, so using it is an upper bound: it can over-hold an older
    version (kept until every session older than the LATEST repoint has
    exited) but cannot release a dir a live session may still be using.

    WHY NOT THE NEWER SIBLING DIR'S birthtime / mtime. Measured 2026-10-03 on
    APFS: `rsync -a src/ dst/` — the rollout's copy — sets dst's mtime AND
    birthtime to the SOURCE dir's mtime (a fresh dir read back as born on
    2020-01-01). The newer version dir therefore routinely looks older than
    the sessions using the dir it replaced, which would release a held dir.

    `installedAt` is NOT used as a fallback: it is the FIRST install, earlier
    than any repoint, i.e. the unsafe direction. A missing or unparseable
    `lastUpdated` is a per-plugin reason, and every superseded dir of that
    plugin is kept as undetermined. An unreadable registry yields empty maps
    here, so every dir falls to "cannot tell when superseded" (kept); it is
    also independently held by registry_referenced_versions' undetermined."""
    import json

    path = path or default_registry_path()
    try:
        with open(path, "r", encoding="utf-8") as fh:
            data = json.load(fh)
    except (OSError, ValueError):
        return {}, {}
    plugins = data.get("plugins") if isinstance(data, dict) else None
    if not isinstance(plugins, dict):
        return {}, {}
    root = os.path.realpath(cache_root)
    by_plugin = {}
    reasons = {}
    for key, entries in plugins.items():
        if not isinstance(entries, list):
            continue
        for ent in entries:
            ip = ent.get("installPath") if isinstance(ent, dict) else None
            if not isinstance(ip, str) or not ip:
                continue
            norm = os.path.normpath(ip)
            pname = os.path.basename(os.path.dirname(norm))
            if os.path.realpath(os.path.dirname(os.path.dirname(norm))) != root:
                continue
            epoch = _iso_epoch(ent.get("lastUpdated"))
            if epoch is None:
                reasons[pname] = (
                    f"registry entry {key!r} has no parseable lastUpdated "
                    f"({ent.get('lastUpdated')!r})"
                )
                continue
            by_plugin[pname] = max(epoch, by_plugin.get(pname, epoch))
    for pname in list(reasons):
        # One entry without a usable time is enough to make the plugin's
        # superseded-at unknown: the max over the others could be too early.
        by_plugin.pop(pname, None)
    return by_plugin, reasons


def superseded_at_override():
    """Parse PLUGIN_CACHE_SUPERSEDED_AT_OVERRIDE -> ({(plugin, ver): epoch}, reason)."""
    import json

    raw = os.environ.get(SUPERSEDED_AT_OVERRIDE_ENV)
    if not raw:
        return {}, None
    try:
        data = json.loads(raw)
    except ValueError as exc:
        return {}, f"{SUPERSEDED_AT_OVERRIDE_ENV}: not JSON ({exc})"
    if not isinstance(data, dict):
        return {}, f"{SUPERSEDED_AT_OVERRIDE_ENV}: not a JSON object"
    out = {}
    for k, v in data.items():
        pname, sep, ver = k.partition("/")
        if not sep or not pname or not ver or "/" in ver:
            return {}, f"{SUPERSEDED_AT_OVERRIDE_ENV}: key {k!r} is not <plugin>/<version>"
        if isinstance(v, bool) or not isinstance(v, int):
            return {}, f"{SUPERSEDED_AT_OVERRIDE_ENV}: value for {k!r} is not an integer epoch"
        out[(pname, ver)] = v
    return out, None


def session_age_holds(stale, cache_root, registry_path=None):
    """Apply the session-age hold to `stale` (from scan). Returns (stale, problem).

    Each version dir gets `sessions` = the live `claude` pids that started
    strictly before its superseded-at. A dir whose superseded-at is unknown is
    marked undetermined (kept, reported). If the process list itself is
    undetermined, every entry not already held — dangling links included — is
    marked undetermined and `problem` says why: nothing may be removed on a
    process list that could not be read. An entry already held for another
    reason keeps that reason (it is kept either way).

    Dangling links are otherwise exempt: they address nothing, so no session
    can be executing from them."""
    procs, proc_why = live_claude_processes()
    override, override_why = superseded_at_override()
    by_plugin, reasons = superseded_at_by_plugin(cache_root, registry_path)
    blanket = proc_why or override_why
    out = []
    for s in stale:
        h = s.holders
        und = h.undetermined
        sessions = ()
        if h.held:
            # Already kept for a DETERMINATE reason (live marker, settings pin,
            # registry ref) or an existing undetermined one. Adding a session-age
            # undetermined on top would change nothing about the keep and would
            # mask the real reason in describe(); a blanket probe failure is
            # still reported by the caller as a problem (exit 1).
            pass
        elif blanket:
            und = und or f"session-age hold: {blanket}"
        elif s.kind == "dir":
            at = override.get((s.plugin, s.version))
            if at is None:
                at = by_plugin.get(s.plugin)
            if at is None:
                und = und or (
                    "session-age hold: cannot tell when this version was "
                    "superseded — "
                    + reasons.get(
                        s.plugin, "no installed_plugins.json entry for this plugin"
                    )
                )
            else:
                sessions = tuple(sorted(pid for pid, start in procs if start < at))
        nh = Holders(
            live_pids=h.live_pids,
            undetermined=und,
            pinned=h.pinned,
            registered=h.registered,
            sessions=sessions,
        )
        out.append(StaleDir(s.plugin, s.version, s.path, nh, kind=s.kind))
    return out, blanket
