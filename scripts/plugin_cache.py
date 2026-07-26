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
import os
import re

# A version dir entry that is a live-session marker rather than a payload file.
IN_USE_DIR = ".in_use"
_PID_RE = re.compile(r"^[0-9]+$")


class Holders:
    """Who is holding a version dir. `undetermined` is not "nobody"."""

    __slots__ = ("live_pids", "undetermined")

    def __init__(self, live_pids=(), undetermined=None):
        self.live_pids = tuple(live_pids)
        self.undetermined = undetermined

    @property
    def held(self):
        """True if the dir must not be removed: live holders OR unknown."""
        return bool(self.live_pids) or self.undetermined is not None

    def __repr__(self):  # pragma: no cover - diagnostics only
        return f"Holders(live_pids={self.live_pids}, undetermined={self.undetermined!r})"


def pid_alive(pid):
    """Is `pid` a live process? True / False / None (cannot tell).

    Signal 0 performs the permission and existence checks without delivering
    anything. EPERM means the process exists but belongs to someone else, which
    is still ALIVE — reading that as dead is how a liveness check turns into a
    delete-someone-else's-running-plugin bug.
    """
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
    """A cached version dir that is not the plugin's current version."""

    __slots__ = ("plugin", "version", "path", "holders")

    def __init__(self, plugin, version, path, holders):
        self.plugin = plugin
        self.version = version
        self.path = path
        self.holders = holders

    @property
    def removable(self):
        return not self.holders.held

    def describe(self):
        if self.holders.undetermined:
            return f"{self.plugin}/{self.version} (undetermined: {self.holders.undetermined})"
        if self.holders.live_pids:
            pids = ",".join(str(p) for p in self.holders.live_pids)
            return f"{self.plugin}/{self.version} (in use by pid {pids})"
        return f"{self.plugin}/{self.version}"


def scan(cache_root, current_versions):
    """Return (stale_dirs, problems) for every plugin dir under `cache_root`.

    A plugin present in the cache but absent from `current_versions` is reported
    as a problem and NONE of its dirs are listed stale. Treating "I don't know
    which version is current" as "every version is stale" would hand the pruner
    the live dir.
    """
    stale = []
    problems = []
    try:
        plugin_names = sorted(os.listdir(cache_root))
    except FileNotFoundError:
        return [], []
    except OSError as exc:
        return [], [f"cannot list plugin cache {cache_root}: {exc}"]

    for pname in plugin_names:
        pdir = os.path.join(cache_root, pname)
        if not os.path.isdir(pdir):
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
            if v == cur or not os.path.isdir(vdir):
                continue
            stale.append(StaleDir(pname, v, vdir, holders_of(vdir)))
    return stale, problems


def default_cache_root():
    """The plugin cache root, honouring the same env var rollout-plugins.sh does."""
    override = os.environ.get("CLAUDE_PLUGIN_CACHE")
    if override:
        return override
    return os.path.expanduser("~/.claude/plugins/cache/yukineko")
