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
it was dead. Liveness has to be asked of the OS, per pid.

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

    __slots__ = ("live_pids", "undetermined", "pinned")

    def __init__(self, live_pids=(), undetermined=None, pinned=()):
        self.live_pids = tuple(live_pids)
        self.undetermined = undetermined
        self.pinned = tuple(pinned)

    @property
    def held(self):
        """True if the dir must not be removed: live holders, unknown, or a
        settings.json pin (a hardcoded absolute path outside the registry's
        current-version pointer, which the pruner has no other visibility
        into — see settings_pinned_versions)."""
        return (
            bool(self.live_pids)
            or self.undetermined is not None
            or bool(self.pinned)
        )

    def __repr__(self):  # pragma: no cover - diagnostics only
        return (
            f"Holders(live_pids={self.live_pids}, "
            f"undetermined={self.undetermined!r}, pinned={self.pinned})"
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
        if self.holders.pinned:
            refs = ", ".join(sorted(self.holders.pinned))
            return f"{base} (pinned by settings.json: {refs})"
        if self.holders.live_pids:
            pids = ",".join(str(p) for p in self.holders.live_pids)
            return f"{base} (in use by pid {pids})"
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


def scan(cache_root, current_versions, settings_pins=None, settings_undetermined=None):
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
    """
    settings_pins = settings_pins or {}
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
                    h = Holders(undetermined=settings_undetermined) \
                        if settings_undetermined else Holders()
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
            if settings_undetermined:
                h = Holders(
                    live_pids=h.live_pids,
                    undetermined=h.undetermined or settings_undetermined,
                    pinned=h.pinned,
                )
            else:
                raws = settings_pins.get((pname, v))
                if raws:
                    h = Holders(
                        live_pids=h.live_pids, undetermined=h.undetermined, pinned=raws
                    )
            stale.append(StaleDir(pname, v, vdir, h))
    return stale, problems


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
