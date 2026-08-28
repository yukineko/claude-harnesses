#!/usr/bin/env python3
"""Verify every plugin is actually LIVE: rolled out at its source version AND enabled.

check-plugin-versions.py only verifies the three SOURCE files agree with each
other (Cargo.toml / plugin.json / marketplace.json). It says nothing about
whether that agreed-upon version was ever rolled out via scripts/rollout-plugins.sh
to `~/.claude/plugins/installed_plugins.json` (the registry the live harness
actually reads from). A commit can bump all three source files, pass that
gate, and still never take effect for any running session — this happened to
5 plugins in one sitting before this script existed (hypothesis, condukt,
compass, blastguard, overwatch all sat committed-but-undeployed).

This script checks two independent dimensions of "is it actually live":

1. ROLLOUT (hard failure). For every plugin under crates/<name>/, compares:
     - crates/<name>/.claude-plugin/plugin.json  .version       (source of truth)
     - installed_plugins.json .plugins["<name>@yukineko"][0].version  (deployed)

2. ENABLEMENT (severity split). A plugin can be installed AND version-correct
   and still be completely inert, because `enabledPlugins` in settings.json
   does not list it (or lists it as false) — no hook of it ever fires. This is
   demonstrated, not theoretical: propguard (a GATE crate) sat installed at the
   right version with every version/rollout gate green while its Stop hook never
   fired, because it was simply missing from enabledPlugins. Severity:
     - a GATE crate that is not enabled is a hard FAILURE. A disabled fleet
       defense gate is a silent hole: everything reports green and nothing
       guards. There is no legitimate reason to run this repo with one off.
     - any other plugin that is not enabled is a WARNING only. Users disable
       plugins on purpose (benchkit / daily-report / ship are off deliberately),
       so this must inform, never block.

Exit codes (distinct per failure CLASS, because the two classes have different
fixes and a caller that conflates them sends the reader to the wrong command —
`rollout-plugins.sh` does NOT enable plugins):
  0 — no rollout drift, no disabled/unaccounted-for GATE crate, no malformed
      input. Warnings about disabled non-gate plugins may still have been
      printed to stderr.
  1 — ROLLOUT class: at least one plugin's source version was never rolled out,
      OR its deployed BINARY is not provably built from current source, OR the
      registry is malformed. Fix: scripts/rollout-plugins.sh.
      Takes precedence when both classes fail; the enablement detail and its
      own fix line are still printed to stderr in that case.
  2 — ENABLEMENT class only: a GATE crate is disabled, or settings.json is
      malformed / has a non-dict enabledPlugins. Fix: edit enabledPlugins in
      settings.json and restart Claude Code.
  3 — UNVERIFIABLE class: some crate under crates/ ships a plugin.json that is
      unparseable or nameless, or an expected GATE plugin has no readable
      plugin.json at all. Outranks both other classes, because neither
      dimension's verdict covers a plugin the checker could not read, and
      because its fix is neither of theirs — repair the plugin.json. (Its own
      code rather than borrowing rc 1 or 2: .githooks/pre-push prints a
      class-specific remedy per code, and both of those remedies are wrong
      here. pre-push's catch-all branch prints this script's full output
      instead, which names the actual problem.)
  4 — PARKED_CONFIG class: scripts/parked-plugins.json exists but cannot be
      trusted (unparseable, wrong shape, an entry with no reason/parked_at, or a
      name matching no plugin). Fix: repair that file. Outranks every other
      class — see the tail of main() for why.

3. PARKED (third state, not a failure at all). A plugin can be deliberately
   left un-rolled-out / un-enabled, and before scripts/parked-plugins.json
   existed there was no way to say so: the checker was binary, and a plugin
   parked on purpose reported red forever with no reachable green. That is not a
   cosmetic problem. On 2026-08-04 the permanent red on taintguard was read as a
   malfunction, the crate was armed to clear it, and its known false positive
   then blocked the user's editing work (backlog a6f165cd). A declared park moves
   that plugin's rollout/enablement findings out of the red lists and into a
   PARKED ON PURPOSE report that prints the reason, the date it was parked, an
   optional revisit pointer, and — verbatim — every finding it is suppressing. It
   does NOT affect the exit code in either direction, and it does not touch the
   source-tree or cache checks (see main()).

The rollout dimension checks FOUR things, because a matching version string is
not evidence that anything in that directory is current:

  (a) VERSION — the registry's version equals the source's plugin.json version.
  (b) PROVENANCE — the binary in that install dir is provably built from source
      that has not moved since. rebuild-plugins.sh drops a `.deployed-from.json`
      beside every host binary it places, recording the commit and whether the
      tree was dirty; this script asks git whether crates/<plugin> or a shared
      crate it links (harness-core) changed since that commit.
  (c) FILE MIRROR — every other file in the install dir is byte-identical to
      crates/<plugin>. rollout deploys with `rsync -a --delete` excluding only
      target/, .git/ and .in_use/, so a complete rollout leaves a full mirror
      plus the artifacts rebuild adds. Checking only (b) was too narrow a
      reading of "is this rolled out": a plugin's payload is its skills, agents,
      hooks, commands and manifests, and for the three skill-only plugins that
      payload is ALL there is, so (b) said nothing about them at all.
  (d) NO SUPERSEDED DIRS — the cache keeps no removable version dir other than
      the current one. Measured before this existed: 265 superseded dirs, 1.29
      GB, 25 versions deep for a single plugin. `claude plugin install` can pin
      to one of those, and (b) and (c) both look only at the directory the
      registry points at. A dir held by a live session (`.in_use/<live pid>`) is
      not reported — it cannot be removed and would be a red nothing can clear;
      a dir whose hold status cannot be determined IS reported. The rule lives
      in scripts/plugin_cache.py, shared with scripts/prune-plugin-cache.py, so
      what the gate demands and what the pruner deletes cannot drift apart.

(b) exists because (a) alone was blind to the most common staleness: every
plugin statically links harness-core, so a change there changes every binary
while no plugin's own version moves — and rollout-plugins.sh, being idempotent
on an unchanged version, never re-points the registry, so (a) keeps reporting a
clean fleet. Note that comparing binary HASHES cannot substitute for (b): this
repo's release builds are not byte-reproducible (measured 2026-07-26 — `cargo
clean -p X && cargo build` yields a different sha256 for identical source), so a
hash comparison flags every plugin on every run and gates nothing.

Every "cannot tell" branch of (b) — no manifest, unreadable manifest, no
recorded commit, a commit git cannot resolve, or a binary built from a dirty
tree — reports drift rather than passing. An unverifiable binary is the state
this check exists to surface; resolving it to clean would make "never recorded"
indistinguishable from "verified current".

(b) applies only to plugins that HAVE a binary, and that is decided from the
SOURCE (does crates/<name> declare a bin target?), never from whether one is
present in the deployed tree. Three plugins — daily-report, scout and flow
(skills-only since 0.2.7, when its one hook was retired and the crate went with
it) — are skills and hooks only, with no Rust crate at all; they have no compiled
artifact whose
provenance could drift, and no rollout could ever write a manifest for them, so
demanding one reported permanent unfixable drift. That exemption is the sole
branch of (b) that passes with no manifest and it is deliberately narrow: a
plugin whose source DOES declare a binary but has none deployed is reported as
drift (a fresh version dir missing its host binary makes the launcher exec
nothing and silently no-op), and a binary sitting on disk must be verifiable
whatever the source says.

Both dimensions fail SOFT on a MISSING input file: an absent registry skips the
rollout check, an absent settings.json skips the enablement check, and neither
absence is a failure (nothing is deployed / nothing is configured yet). A
PRESENT-but-unparseable file is the opposite of that and fails HARD: settings
Claude Code cannot parse is a state where NO plugin is enabled — every gate
inert — so reporting it as "not found ... SKIP (not a failure)" would be both a
lie about the file and fail-open on the exact hole this script exists to close.

Registry path defaults to ~/.claude/plugins/installed_plugins.json; override
with CLAUDE_PLUGIN_REGISTRY (same env var rollout-plugins.sh honors) so this
is testable against a fixture registry. Settings path defaults to
~/.claude/settings.json; override with CLAUDE_SETTINGS.

Run from the repo root:  python3 scripts/check-plugin-rollout.py
"""
import json
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import plugin_cache  # noqa: E402

OWNER = "yukineko"

# Exit codes, one per failure CLASS (see the module docstring). Named so callers
# — .githooks/pre-push in particular — can branch on the class instead of on a
# bare non-zero and then guess at which remediation to print.
RC_OK = 0
RC_ROLLOUT = 1
RC_ENABLEMENT = 2
RC_UNVERIFIABLE = 3
# The parked DECLARATION itself is unusable (see load_parked). Its own class
# because its remedy is neither of the three above: nothing is wrong with the
# fleet, the file that says which reds are intentional cannot be trusted — so
# this run cannot tell an intentional red from a real one in either direction.
RC_PARKED_CONFIG = 4

REPO = os.getcwd()
CRATES = os.path.join(REPO, "crates")

# Filename of the provenance manifest rebuild-plugins.sh drops beside every host
# binary it places, recording the commit that binary was built from.
PROVENANCE_FILE = ".deployed-from.json"

# Top-level dirs rollout-plugins.sh's rsync excludes, so they are never expected
# in a deployed tree and must be excluded from both sides of the comparison.
# `.in_use/` is a runtime marker dir written by live sessions, not payload.
# `.claude/` is the same kind of thing: `.gitignore` declares `crates/*/.claude/`
# "crate-local runtime progress artifacts (taskprog etc.) — never track", and
# `git ls-files 'crates/*/.claude/*'` returns 0 files, so nothing in it is
# payload. It was NOT excluded until 2026-08-20, and the consequence was
# concrete: taskprog seeded crates/ctxrot/.claude/progress.md (a content-free
# skeleton naming another session) and this check reported "1 source file(s) not
# deployed", pointing its reader at `--force` — i.e. at copying one session's
# scratch state into the shared plugin cache. This list is kept identical to the
# script's rsync excludes by
# test_check_plugin_rollout.CrateLocalRuntimeArtifacts.
DEPLOY_EXCLUDED_TOP = ("target", ".git", ".in_use", ".claude")

# Deployed text files whose bytes differ from the crate's ONLY by line endings.
# Not drift (the payload is identical; git itself calls the two checkouts equal)
# and not clean either (the bytes really do differ), so the class is reported
# separately and does not touch the exit code. Populated by `_asset_problem`,
# drained by `main`. Module-level because `_asset_problem` returns one string
# and its callers append that to the blocking list — a second return value would
# have to be threaded through every caller and test for a non-blocking note.
EOL_ONLY = []

# The <os>-<arch> suffixes a plugin binary can carry. Binaries are generated,
# never committed for every platform, so bin/<name>-<suffix> is allowed to exist
# in the deployed tree without a counterpart in the crate.
PLATFORM_SUFFIXES = (
    "darwin-arm64",
    "darwin-x86_64",
    "linux-x86_64",
    "linux-arm64",
    "windows-x86_64",
    "windows-arm64",
    # cargo/rustc append .exe to every Windows build output, and
    # rebuild-plugins.sh deploys under that exact name (see its own EXT
    # handling) — so a correctly-deployed Windows binary never matches the two
    # bare suffixes above. Without these, _is_rebuild_artifact() misclassifies
    # a legitimately deployed <name>-windows-*.exe as an unaccounted stray file
    # (measured 2026-08-04: every GATE crate's real deployed .exe was reported
    # as "not a mirror" drift).
    "windows-x86_64.exe",
    "windows-arm64.exe",
)

# Crates every plugin binary statically links. A change to one of these changes
# every binary while no plugin's own version moves, which is precisely the drift
# a version-string comparison cannot see.
SHARED_SOURCE_PATHS = ("crates/harness-core",)


def _git_changed_since(commit, crate):
    """Did `crate`'s source (or a shared crate it links) move since `commit`?

    Returns True (moved), False (unchanged), or None (could not determine —
    e.g. the commit is unknown to this repo, or git failed). None is NOT
    "unchanged": callers must resolve it restrictively.
    """
    import subprocess

    paths = [f"crates/{crate}"] + list(SHARED_SOURCE_PATHS)
    try:
        # `git diff --quiet A HEAD -- <paths>`: rc 0 = no difference, rc 1 =
        # differs. Any other rc (128 = bad object / not a repo) is undetermined,
        # NOT "no difference" — the exit code is the verdict, not just stdout.
        proc = subprocess.run(
            ["git", "diff", "--quiet", commit, "HEAD", "--"] + paths,
            cwd=REPO,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    if proc.returncode == 0:
        return False
    if proc.returncode == 1:
        return True
    return None


# Rebindable so the provenance dimension is testable without a git repo, the
# same way REGISTRY_PATH/SETTINGS_PATH are rebound at a fixture.
SOURCE_CHANGED_SINCE = _git_changed_since


def _source_core_version():
    """`[package].version` of the shared crate in the CURRENT source tree.

    Returns None when it cannot be read. None is not a version: the caller
    reports it rather than assuming the manifest agrees with something it could
    not look at.
    """
    path = os.path.join(REPO, "crates", "harness-core", "Cargo.toml")
    try:
        with open(path, encoding="utf-8") as fh:
            text = fh.read()
    except OSError:
        return None
    in_package = False
    for raw in text.splitlines():
        line = raw.strip()
        if line.startswith("["):
            in_package = line == "[package]"
            continue
        if in_package and line.startswith("version"):
            parts = line.split("=", 1)
            if len(parts) == 2 and parts[0].strip() == "version":
                v = parts[1].strip().strip('"').strip("'")
                if v:
                    return v
    return None


# Rebindable for the same reason as SOURCE_CHANGED_SINCE.
SOURCE_CORE_VERSION = _source_core_version


def _host_suffix():
    """This host's `<os>-<arch>` binary suffix, matching rebuild-plugins.sh's $SUF.

    The suffix must be the HOST's, not "any known platform": a version dir ships
    the committed cross-platform binaries, so accepting any suffix would call a
    plugin deployed whose per-host binary is precisely the one missing — the
    documented failure where the launcher execs nothing and silently no-ops.
    """
    import platform

    sysname = platform.system().lower()
    os_part = sysname if sysname in ("darwin", "linux", "windows") else "unknown"
    mach = platform.machine().lower()
    if mach in ("x86_64", "amd64"):
        arch = "x86_64"
    elif mach in ("aarch64", "arm64"):
        arch = "arm64"
    else:
        arch = "unknown"
    return f"{os_part}-{arch}"


HOST_SUFFIX = _host_suffix()


def _crate_ships_binary(crate):
    """Does crates/<crate> declare a binary target? True / False / None.

    None means undetermined, and callers must resolve it restrictively. This is
    deliberately decided from the SOURCE, never from whether a binary happens to
    be present in the deployed tree: "no bin/ directory" is itself one of the
    failure states this check exists to catch, so reading the deployed side to
    decide whether a binary was expected would let that failure certify itself.

    A plugin may legitimately ship no binary at all (skills/ and hooks/ only,
    with no Rust crate under crates/<name> — daily-report, scout and flow are the
    three such plugins today). Those have no compiled artifact whose provenance could
    drift, and are the sole case allowed to pass with no manifest.
    """
    crate_dir = os.path.join(CRATES, crate)
    if not os.path.isdir(crate_dir):
        return None
    if os.path.isfile(os.path.join(crate_dir, "src", "main.rs")):
        return True
    cargo = os.path.join(crate_dir, "Cargo.toml")
    if not os.path.exists(cargo):
        # No Rust crate at all: skill-only plugin.
        return False
    if not os.path.isfile(cargo):
        return None
    try:
        with open(cargo, "r", encoding="utf-8") as fh:
            text = fh.read()
    except OSError:
        return None
    # A crate can declare its binary explicitly instead of via src/main.rs
    # (context-governor does exactly this), so both forms must be recognised.
    return "[[bin]]" in text


def _host_binary_deployed(install):
    """Is this host's binary present under `install`/bin? True / False / None.

    None means the directory exists but could not be listed — undetermined, not
    "absent" and not "present".
    """
    bindir = os.path.join(install, "bin")
    try:
        entries = os.listdir(bindir)
    except FileNotFoundError:
        return False
    except NotADirectoryError:
        return None
    except OSError:
        return None
    # See the matching PLATFORM_SUFFIXES comment above: on Windows the deployed
    # filename carries a trailing .exe that HOST_SUFFIX alone does not match.
    return any(
        e.endswith("-" + HOST_SUFFIX) or e.endswith("-" + HOST_SUFFIX + ".exe")
        for e in entries
    )
REGISTRY_PATH = os.environ.get(
    "CLAUDE_PLUGIN_REGISTRY", os.path.expanduser("~/.claude/plugins/installed_plugins.json")
)
SETTINGS_PATH = os.environ.get(
    "CLAUDE_SETTINGS", os.path.expanduser("~/.claude/settings.json")
)
# Rebindable at a fixture, like the two paths above.
PLUGIN_CACHE_ROOT = plugin_cache.default_cache_root()

# The fleet GATE crates: fleet defense gates (plus `overwatch`, which computes
# the canary health-gate decision that protects the others). These require a
# --canary rollout, and must never be left disabled.
#
# This is the file's ONE copy — the human-facing hint below is generated from
# it rather than repeating the list, so the two cannot drift apart. Kept equal
# to scripts/rollout-plugins.sh's canonical GATE_CRATES, enforced by
# scripts/check-gate-crates-sync.py (this file is a tracked source).
GATE_CRATES = (
    "blastguard",
    "propguard",
    "specguard",
    "stuckguard",
    "taintguard",
    "mutategate",
    "overwatch",
    "parallelguard",
)

# GATE crates that legitimately ship NO .claude-plugin/plugin.json and so can
# never appear in the plugin scan: `mutategate` is a plain (non-plugin) crate.
# Kept as an explicit, commented exemption rather than an implicit consequence
# of "whatever we happened to find", so that a GATE crate DISAPPEARING from the
# scan (plugin.json deleted / renamed / corrupted) is a failure instead of
# quietly shrinking the denominator of the green "all N GATE plugin(s) enabled".
NON_PLUGIN_GATES = ("mutategate",)
EXPECTED_GATE_PLUGINS = tuple(c for c in GATE_CRATES if c not in NON_PLUGIN_GATES)


def rollout_hint():
    """The fix hint shown on drift, generated from GATE_CRATES (no 2nd copy)."""
    return (
        "\nFix: scripts/rollout-plugins.sh --plugin <name> "
        f"(add --canary for GATE crates: {'/'.join(GATE_CRATES)})."
    )


# _load_json states. ABSENT and MALFORMED must stay distinguishable: collapsing
# them (both -> None) made a corrupt registry/settings print "not found: <path>"
# — a lie, the file is right there — and then SKIP with rc=0, i.e. fail OPEN on
# a state where nothing is enabled at all.
ABSENT = "absent"
MALFORMED = "malformed"
OK = "ok"


def _load_json(path):
    """Read a JSON file. Returns (state, data) with state in ABSENT/MALFORMED/OK.

    `data` is None unless state is OK. A missing file is fail-soft (the caller
    skips its dimension); a present-but-unparseable one is a hard failure with
    the parse error attached, so the message names what is actually wrong.
    """
    if not os.path.isfile(path):
        return ABSENT, None
    try:
        with open(path, encoding="utf-8") as f:
            return OK, json.load(f)
    except (json.JSONDecodeError, OSError, UnicodeDecodeError) as exc:
        return MALFORMED, str(exc)


PARKED_PATH = os.environ.get(
    "PARKED_PLUGINS", os.path.join(REPO, "scripts", "parked-plugins.json")
)

# Keys a parked entry must carry. `reason` because a park with no stated reason
# is indistinguishable from a park someone forgot about, and the next reader —
# the one deciding whether the red is a fault — is exactly who needs it.
# `parked_at` because "how long has this been parked" is the only question that
# tells an operator whether the park is still current, and a park with no start
# date can never be judged stale.
PARKED_REQUIRED = ("reason", "parked_at")
PARKED_DATE_RE = re.compile(r"^\d{4}-\d{2}-\d{2}$")


def load_parked(plugins, path=None):
    """Return (parked, config_problems) from the parked-plugins declaration.

    `parked` maps crate/plugin name -> entry dict. `config_problems` is a list of
    human-readable reasons the DECLARATION cannot be trusted.

    Why this file exists (backlog a6f165cd)
    ---------------------------------------
    This checker used to be binary: a plugin was either live or a failure. There
    is a third real state — deliberately NOT rolled out, because arming it right
    now would do harm. taintguard is the case that forced this: it is committed
    at 0.1.8, its launcher is intentionally left un-rolled-out while a known
    false positive is measured, and so the rollout dimension reported red on
    every single run with no way for it to ever go green short of arming the
    gate.

    A permanent red is not a harmless nuisance, it is an ACTIVE hazard, and this
    is measured, not theoretical: on 2026-08-04 the red was read as a
    malfunction, taintguard was force-enabled to clear it, and the known false
    positive then blocked the user's editing. The gate did not fail; the
    checker's inability to say "this red is on purpose" is what failed.

    Fail-closed on every unusable shape rather than defaulting to "nothing is
    parked". A declaration that cannot be parsed is the one state where this
    file's whole purpose — distinguishing intentional reds from real ones — is
    unavailable, and silently treating it as empty would resurrect the exact
    misreading above (a red with no explanation attached). The one shape that is
    NOT a problem is an ABSENT file: nothing parked is the normal case and the
    default this repo should sit at.

    A name that matches no scanned plugin is a hard problem, not a shrug: a
    typo'd park silently parks NOTHING, so the red persists, and the operator who
    wrote the entry believes it was handled. That is worse than no park at all.
    """
    path = path or PARKED_PATH
    state, data = _load_json(path)
    if state == ABSENT:
        return {}, []
    if state == MALFORMED:
        return {}, [
            f"{path} is present but unparseable ({data}). Whether any red below "
            "is intentional cannot be determined, so no red is treated as "
            "intentional and this run reports the declaration itself as broken."
        ]
    if not isinstance(data, dict) or not isinstance(data.get("parked"), dict):
        return {}, [
            f"{path} must be an object with a \"parked\" object in it, got "
            f"{type(data).__name__} / {type(data.get('parked') if isinstance(data, dict) else None).__name__}. "
            "Nothing is parked by an unreadable declaration."
        ]

    # "Real" means a crate DIR under crates/ or a plugin.json name — not merely a
    # plugin the scan managed to PARSE. A crate whose plugin.json is corrupt is
    # absent from `plugins`, and keying the typo guard on that list alone made a
    # park on such a crate report as a misspelling: the reader was sent to repair
    # parked-plugins.json (where nothing was wrong) instead of the plugin.json
    # that actually broke, and the real UNVERIFIABLE finding was outranked by a
    # bogus PARKED_CONFIG one.
    known = set()
    if os.path.isdir(CRATES):
        known.update(
            name for name in os.listdir(CRATES)
            if os.path.isdir(os.path.join(CRATES, name))
        )
    for crate, pname, _src_ver in plugins:
        known.add(crate)
        if pname:
            known.add(pname)

    parked, problems = {}, []
    for name, entry in sorted(data["parked"].items()):
        if not isinstance(entry, dict):
            problems.append(
                f"{path}: parked entry \"{name}\" must be an object with "
                f"{'/'.join(PARKED_REQUIRED)}, got {type(entry).__name__}."
            )
            continue
        missing = [
            k for k in PARKED_REQUIRED
            if not isinstance(entry.get(k), str) or not entry[k].strip()
        ]
        if missing:
            problems.append(
                f"{path}: parked entry \"{name}\" is missing a non-empty "
                f"{', '.join(missing)}. A park with no stated {missing[0]} cannot "
                "be told apart from one nobody remembers making."
            )
            continue
        if not PARKED_DATE_RE.match(entry["parked_at"]):
            problems.append(
                f"{path}: parked entry \"{name}\" has parked_at="
                f"{entry['parked_at']!r}, which is not a YYYY-MM-DD date. How long "
                "the park has stood is the question this field answers, so an "
                "unparseable one answers nothing."
            )
            continue
        if name not in known:
            problems.append(
                f"{path}: parked entry \"{name}\" matches no plugin under crates/ "
                "(neither a crate dir nor a plugin.json name). A misspelled entry "
                "parks nothing while reading as though it had, so it is reported "
                "instead of ignored."
            )
            continue
        parked[name] = entry
    return parked, problems


def parked_report(name, entry, suppressed):
    """One human-readable line-block for a parked plugin.

    Prints the SUPPRESSED findings verbatim rather than only their count. A park
    silences a detection; hiding what was silenced would turn this feature into
    the fail-open it exists to avoid, and the operator deciding whether the park
    is still right needs to see what it is currently covering for.
    """
    lines = [
        f"{name}: parked since {entry['parked_at']} — {entry['reason']}"
    ]
    revisit = entry.get("revisit")
    if isinstance(revisit, str) and revisit.strip():
        lines.append(f"    revisit: {revisit.strip()}")
    if suppressed:
        lines.append(f"    suppressing {len(suppressed)} finding(s) that would otherwise be red:")
        lines.extend(f"      * {s}" for s in suppressed)
    else:
        lines.append(
            "    NOTE: this park is currently suppressing NOTHING — the plugin is "
            "live and green. Remove the entry so a future red is not silenced by a "
            "declaration nobody re-read."
        )
    return "\n".join(lines)


def partition_parked(problems, parked):
    """Split `problems` into (still_red, {name: [suppressed, ...]}).

    Problem strings for a specific plugin are all built as `f"{crate}: ..."`, so
    a leading `"<parked name>: "` is what identifies one. Matching on that exact
    prefix (not a substring search) keeps fleet-wide findings — the superseded-
    version-dir line, which merely NAMES crates inside it — on the red side where
    they belong: a removable cache dir is removable whether or not the plugin is
    parked, and parking must not quietly excuse it.
    """
    still_red, suppressed = [], {name: [] for name in parked}
    for problem in problems:
        for name in parked:
            if problem.startswith(f"{name}: "):
                suppressed[name].append(problem)
                break
        else:
            still_red.append(problem)
    return still_red, suppressed


def scan_plugins():
    """Return (plugins, unverifiable) for every plugin crate under crates/.

    `plugins` is a list of (crate_dir_name, plugin_name, source_version). The
    plugin NAME can differ from the crate DIR (e.g. crates/run-book ->
    `runbook`), and both the registry and enabledPlugins key off the name, so
    always resolve through plugin.json rather than assuming they match.

    `unverifiable` is a list of human-readable problems for crates that DO ship
    a .claude-plugin/plugin.json but whose file is unparseable or carries no
    "name". Those used to be skipped silently, which is a fail-open in both
    directions:
      * the rollout denominator shrank without a word — "OK: 5 plugins deployed"
        where six exist reads exactly like a clean run;
      * for a GATE crate it was worse still, because the reconciliation meant to
        catch it lived inside check_enabled() behind the early return for an
        absent settings.json, so on a machine with no settings.json the crate
        name appeared nowhere in the output at all.
    An ABSENT plugin.json is different and stays silent here: that is simply how
    a non-plugin crate (harness-core, integration-tests) looks. A GATE crate
    that vanishes that way is caught by unaccounted_gate_plugins() instead.
    """
    plugins, unverifiable = [], []
    if not os.path.isdir(CRATES):
        return plugins, unverifiable
    for name in sorted(os.listdir(CRATES)):
        pj = os.path.join(CRATES, name, ".claude-plugin", "plugin.json")
        if not os.path.isfile(pj):
            continue  # not a plugin (e.g. harness-core, integration-tests)
        state, pjd = _load_json(pj)
        if state != OK:
            unverifiable.append(
                f"{name}: {pj} is present but unparseable ({pjd}). Neither its "
                "rollout nor its enablement can be checked."
            )
            continue
        if not isinstance(pjd, dict) or not pjd.get("name"):
            unverifiable.append(
                f"{name}: {pj} carries no \"name\", so there is no plugin "
                "identity to look up in the registry or in enabledPlugins."
            )
            continue
        plugins.append((name, pjd.get("name"), pjd.get("version")))
    return plugins, unverifiable


def unaccounted_gate_plugins(plugins):
    """EXPECTED_GATE_PLUGINS that no readable plugin.json accounts for.

    Deriving the inspected gate set purely from what the scan happened to find
    means a GATE plugin whose plugin.json was deleted, renamed, or corrupted is
    neither checked nor missed — it just vanishes and the run reports a green
    "all N GATE plugin(s) enabled" for a smaller N.

    This lives at module level, called unconditionally from main(), because it
    used to sit inside check_enabled() AFTER that function's early return for an
    absent settings.json. On a machine without settings.json it therefore never
    ran, and an unreadable GATE plugin.json was a fully silent exit 0.

    Matching is on EITHER the crate dir or the plugin name, so renaming
    crates/specguard -> crates/spec-guard while keeping the plugin name is not a
    false alarm.
    """
    seen = set()
    for crate, pname, _src_ver in plugins:
        seen.add(crate)
        if pname:
            seen.add(pname)
    return sorted(set(EXPECTED_GATE_PLUGINS) - seen)


def check_rollout(plugins):
    """Return (problems, checked) comparing source version to the deployed one.

    `problems is None` means the dimension was SKIPPED (registry absent). A
    malformed registry is not a skip — it is a problem, reported as such.
    """
    load_state, registry = _load_json(REGISTRY_PATH)
    if load_state == ABSENT:
        return None, 0
    if load_state == MALFORMED:
        return [
            f"registry is present but unparseable: {REGISTRY_PATH} ({registry}). "
            "Nothing can be verified as deployed; fix or regenerate the file."
        ], 0
    entries = registry.get("plugins", {}) if isinstance(registry, dict) else {}

    problems = []
    checked = 0
    for crate, pname, src_ver in plugins:
        checked += 1
        key = f"{pname}@{OWNER}"
        # An absent source version is not "no drift". `None != None` is False, so
        # a plugin.json with no "version" and a registry entry with no "version"
        # used to compare equal and count as cleanly deployed — the checker
        # reporting OK for a plugin whose deployed version it never learned.
        if src_ver is None:
            problems.append(
                f"{crate}: .claude-plugin/plugin.json has no \"version\", so there "
                "is nothing to compare the registry against. Add one."
            )
            continue
        if key not in entries:
            problems.append(f"{crate}: source={src_ver} but never installed (no '{key}' in registry)")
            continue
        entry = entries[key]
        # Guard the ELEMENTS, not just the list. `isinstance(entry, list) and
        # entry` let `["1.2.0"]` through to `.get` and raised an uncaught
        # AttributeError; the resulting traceback exited 1, colliding with
        # RC_ROLLOUT, so .githooks/pre-push routed a crash to the rollout
        # remediation as though it were ordinary drift.
        if not isinstance(entry, list) or not entry or not isinstance(entry[0], dict):
            problems.append(
                f"{crate}: registry entry '{key}' is malformed — expected a "
                f"non-empty list of objects, got {entry!r}. The deployed version "
                "cannot be read, so nothing is verified as rolled out."
            )
            continue
        reg_ver = entry[0].get("version")
        if reg_ver != src_ver:
            problems.append(
                f"{crate}: source={src_ver} registry={reg_ver} <- rollout-plugins.sh not run since bump"
            )
            # The version is already known stale; the binary necessarily is too.
            # Reporting both would just double-count one fix.
            continue
        problem = _provenance_problem(crate, entry[0])
        if problem:
            problems.append(problem)
        problem = _asset_problem(crate, entry[0])
        if problem:
            problems.append(problem)
    return problems, checked


def _provenance_problem(crate, entry):
    """Return a drift string if the deployed binary is not provably current.

    A matching version string only says the registry POINTER was re-pointed at
    some point; it says nothing about the bytes sitting in that directory. The
    binary is only provably current when a manifest records the commit it was
    built from, that commit is committed (not a dirty tree), and the plugin's
    source has not moved since.

    Every "cannot tell" branch resolves to drift, not to clean: an unverifiable
    binary is exactly the state this check exists to surface, and returning None
    for it would make "never recorded" indistinguishable from "verified current".
    """
    install = entry.get("installPath")
    if not install:
        return (
            f"{crate}: registry entry has no 'installPath', so the deployed "
            "binary cannot be located and its provenance cannot be verified"
        )
    ships = _crate_ships_binary(crate)
    if ships is None:
        return (
            f"{crate}: could not determine from crates/{crate} whether this "
            "plugin ships a binary (crate directory missing or Cargo.toml "
            "unreadable), so it cannot be told apart from a skill-only plugin "
            "— undetermined is not 'nothing to verify'"
        )
    deployed = _host_binary_deployed(install)
    if deployed is None:
        return (
            f"{crate}: could not list {os.path.join(install, 'bin')}, so whether "
            "a binary is deployed at all is undetermined"
        )
    if not ships and not deployed:
        # Skill-only plugin: no Rust crate in the source, and no binary on disk.
        # There is no compiled artifact whose provenance could drift, so there is
        # nothing here to verify. This is the ONLY branch that passes with no
        # manifest, and it requires BOTH sides to agree — a missing bin/ alone
        # never excuses a plugin whose source declares a binary.
        return None
    if ships and not deployed:
        return (
            f"{crate}: crates/{crate} declares a binary target, but no "
            f"{HOST_SUFFIX} binary is deployed under "
            f"{os.path.join(install, 'bin')} — the plugin is installed but "
            "execs nothing (re-run rollout-plugins.sh)"
        )
    # A binary is on disk (whether or not the source still declares one), so its
    # provenance must be verifiable.
    manifest_path = os.path.join(install, PROVENANCE_FILE)
    state, manifest = _load_json(manifest_path)
    if state == ABSENT:
        return (
            f"{crate}: no {PROVENANCE_FILE} beside the deployed binary — cannot "
            "tell which commit it was built from, so it is not verifiable as "
            "current (re-run rollout-plugins.sh to record provenance)"
        )
    if state == MALFORMED or not isinstance(manifest, dict):
        return (
            f"{crate}: {PROVENANCE_FILE} is present but unreadable ({manifest!r}); "
            "the deployed binary's provenance cannot be established"
        )
    if manifest.get("dirty"):
        return (
            f"{crate}: deployed binary was built from a DIRTY tree at "
            f"{str(manifest.get('commit'))[:12]} — the recorded commit does not "
            "describe those bytes, so currency cannot be checked against it"
        )
    commit = manifest.get("commit")
    if not commit:
        return (
            f"{crate}: {PROVENANCE_FILE} records no 'commit', so there is nothing "
            "to compare the current source against"
        )
    # The shared-crate version these bytes contain, as recorded at rollout
    # (backlog 32170548). harness-core is linked into every plugin binary, so
    # this is the second half of "the version identifies what shipped": the
    # plugin's own version covers its own crate, this covers the shared one.
    #
    # An ABSENT field is deliberately NOT a problem. It means a manifest written
    # before this field existed, and the question it answers is already answered
    # completely by the commit comparison below — SHARED_SOURCE_PATHS includes
    # crates/harness-core, so a harness-core change since `commit` is reported as
    # drift with or without this field. So absence loses no coverage; it only
    # loses the cheaper, self-describing signal. (Stated explicitly because a
    # silently-skipped check is normally exactly the fail-open this file exists to
    # prevent — here the fallback is provably not weaker, not merely assumed so.)
    recorded_core = manifest.get("harness_core_version")
    if recorded_core == "unknown":
        return (
            f"{crate}: {PROVENANCE_FILE} records harness_core_version "
            f"'unknown' — the rollout could not read the shared crate's version, "
            "so which harness-core these bytes contain was never established"
        )
    if recorded_core is not None:
        current_core = SOURCE_CORE_VERSION()
        if current_core is None:
            return (
                f"{crate}: cannot read [package].version from "
                "crates/harness-core/Cargo.toml, so the recorded "
                f"harness_core_version '{recorded_core}' cannot be checked — "
                "undetermined is not 'agrees'"
            )
        if recorded_core != current_core:
            return (
                f"{crate}: deployed binary links harness-core "
                f"{recorded_core}, but the source tree is now at {current_core} "
                "<- rollout-plugins.sh not run since that shared-crate change"
            )

    moved = SOURCE_CHANGED_SINCE(commit, crate)
    if moved is None:
        return (
            f"{crate}: could not determine whether source moved since "
            f"{str(commit)[:12]} (unknown commit, or git unavailable) — "
            "undetermined is not 'unchanged'"
        )
    if moved:
        return (
            f"{crate}: deployed binary was built at {str(commit)[:12]}, but "
            f"crates/{crate} or a shared crate it links has changed since "
            "<- rollout-plugins.sh not run since that change"
        )
    return None


def _walk_files(root):
    """Relative path -> absolute path for every file under `root`, or None.

    Returns None if the walk hit an error. os.walk swallows OSError by default,
    which would silently yield a SHORT listing — and a short listing of the
    source side reads as "fewer files to check" while a short listing of the
    deployed side reads as "files are missing". Both are wrong answers dressed
    as data, so the walk raises and this returns undetermined instead.

    The dirs rollout-plugins.sh excludes are excluded here too, at the top level
    only, exactly as its rsync does: `--exclude '/target/'` is anchored. The two
    lists are not merely intended to agree — a test compares them.
    """
    out = {}

    def _raise(exc):
        raise exc

    try:
        for dirpath, dirnames, filenames in os.walk(root, onerror=_raise):
            rel = os.path.relpath(dirpath, root)
            if rel == ".":
                rel = ""
                dirnames[:] = [d for d in dirnames if d not in DEPLOY_EXCLUDED_TOP]
            for f in filenames:
                out[os.path.join(rel, f)] = os.path.join(dirpath, f)
    except OSError:
        return None
    return out


def _is_rebuild_artifact(rel):
    """Is `rel` a file rebuild-plugins.sh adds to a version dir after the copy?

    Only two kinds exist, and both are generated rather than committed: the
    provenance manifest, and per-platform binaries under bin/. Everything else
    present in the deployed dir but absent from the crate is unaccounted for.
    """
    if rel == PROVENANCE_FILE:
        return True
    parts = rel.split(os.sep)
    return (
        len(parts) == 2
        and parts[0] == "bin"
        and any(parts[1].endswith("-" + s) for s in PLATFORM_SUFFIXES)
    )


def _asset_problem(crate, entry):
    """Return a drift string if the deployed FILES are not the crate's files.

    Checking only the binary was too narrow a reading of "is this rolled out".
    rollout-plugins.sh deploys a plugin with

        rsync -a --delete --exclude '/target/' --exclude '/.git/' \
              --exclude '/.in_use/' --exclude '/.claude/'

    so a complete rollout leaves the install dir a full mirror of crates/<name>,
    plus the two artifacts rebuild-plugins.sh adds afterwards. A plugin's real
    payload is its skills, agents, hooks, commands and manifests; for the two
    skill-only plugins that payload is ALL there is (flow became the third in
    0.2.7), and the binary dimension
    says nothing about any of it. A stale skill file is a stale rollout.

    The allowed delta was not assumed — it was measured across all 39 plugins on
    2026-07-26 at 65c1b3ff: zero files present in source but missing from the
    cache, zero files differing in content, and the only cache-side extras were
    37 provenance manifests and 91 per-platform binaries.
    """
    install = entry.get("installPath")
    if not install:
        # Already reported by the provenance dimension, which runs first and
        # returns its own message for this. Reporting it twice would just
        # double-count one fix.
        return None
    src = _walk_files(os.path.join(CRATES, crate))
    if src is None:
        return (
            f"{crate}: could not read crates/{crate} to compare against what is "
            "deployed — undetermined, not 'nothing to compare'"
        )
    dst = _walk_files(install)
    if dst is None:
        return (
            f"{crate}: could not read the deployed tree at {install} — "
            "undetermined, not 'matches'"
        )

    missing = sorted(set(src) - set(dst))
    extra = sorted(r for r in set(dst) - set(src) if not _is_rebuild_artifact(r))
    differing = []
    eol_only = []
    unreadable = []
    for rel in sorted(set(src) & set(dst)):
        try:
            with open(src[rel], "rb") as a, open(dst[rel], "rb") as b:
                sa, sb = a.read(), b.read()
        except OSError as exc:
            unreadable.append(f"{rel} ({exc})")
            continue
        if sa == sb:
            continue
        # A CRLF-vs-LF difference is a property of the CHECKOUT, not of the
        # payload: `git ls-files --eol` reports i/lf w/lf attr/text=auto for
        # these files, and two checkouts of one commit can legitimately differ
        # here (measured 2026-08-20 at b7302987: the main clone holds CRLF, a
        # linked worktree of the same commit holds LF, and the cache was rsynced
        # from the main clone — so running this from a worktree reported 20
        # plugins as drifted, all representation-only). Byte-exactness is kept
        # for anything with a NUL byte: normalising inside a binary could make a
        # real difference disappear.
        if b"\x00" not in sa and b"\x00" not in sb and (
            sa.replace(b"\r\n", b"\n") == sb.replace(b"\r\n", b"\n")
        ):
            eol_only.append(rel)
            continue
        differing.append(rel)
    if eol_only:
        EOL_ONLY.append((crate, eol_only))

    def _sample(items):
        head = ", ".join(items[:3])
        return head + (f", +{len(items) - 3} more" if len(items) > 3 else "")

    parts = []
    if unreadable:
        parts.append(f"{len(unreadable)} file(s) could not be compared: {_sample(unreadable)}")
    if differing:
        parts.append(f"{len(differing)} deployed file(s) differ from source: {_sample(differing)}")
    if missing:
        parts.append(f"{len(missing)} source file(s) not deployed: {_sample(missing)}")
    if extra:
        parts.append(
            f"{len(extra)} deployed file(s) are not in the crate and are not "
            f"rebuild artifacts: {_sample(extra)}"
        )
    if not parts:
        return None
    # The generic remedy is wrong for this dimension and was measured to be so:
    # rollout-plugins.sh is idempotent on an unchanged version, so a plain run
    # leaves a drifted file exactly as it found it — the gate then stays red and
    # the operator follows advice that cannot work. `--force` recopies.
    return (
        f"{crate}: deployed tree is not a mirror of crates/{crate} — "
        + "; ".join(parts)
        + f" <- fix with: scripts/rollout-plugins.sh --plugin {crate} --force "
        "(a plain rollout is a no-op at an unchanged version and will NOT "
        "repair this)"
    )


def check_stale_version_dirs():
    """Return (problems, checked) for version dirs the cache should no longer keep.

    A rollout that leaves every superseded version behind is only half a
    rollout: `claude plugin install` can pin to a stale cached dir, and nothing
    in the provenance or asset dimensions looks at a directory the registry does
    not point at. Measured 2026-07-26: 265 stale dirs, 1.29 GB, up to 25
    versions deep for one plugin.

    A stale dir held by a LIVE session is not reported — it is expected and
    transient, and the session that holds it will release it. A dir whose hold
    status could not be determined IS reported: the pruner deliberately keeps
    such a dir (deletion is irreversible), so if the gate stayed quiet about it
    nothing would ever surface a cache it cannot inspect.
    """
    cache_root = PLUGIN_CACHE_ROOT
    current, src_problems = plugin_cache.source_versions(CRATES)
    stale, scan_problems = plugin_cache.scan(cache_root, current)

    problems = list(src_problems) + list(scan_problems)
    removable = [s for s in stale if s.removable]
    undetermined = [s for s in stale if s.holders.undetermined]

    if removable:
        sample = ", ".join(s.describe() for s in removable[:4])
        if len(removable) > 4:
            sample += f", +{len(removable) - 4} more"
        problems.append(
            f"{len(removable)} superseded plugin version dir(s) still in the "
            f"cache and removable: {sample} <- run scripts/prune-plugin-cache.py "
            "(rollout-plugins.sh does this automatically)"
        )
    for s in undetermined:
        problems.append(
            f"{s.plugin}/{s.version}: cannot determine whether a live session "
            f"holds this superseded dir ({s.holders.undetermined}) — it is kept, "
            "but undetermined is not clean"
        )
    return problems, len(stale)


def check_settings_pins():
    """Return (problems, checked) for settings.json-referenced cache paths
    that no longer exist on disk.

    2026-07-27 incident: prune deleted ctxrot/0.5.18 and stuckguard/0.1.21
    while 8 hooks/statusLine entries in settings.json still pointed at their
    absolute paths, and this script reported "OK: all N GATE plugin(s)
    enabled" anyway — check_rollout()/check_enabled() only ever look at the
    CURRENT registry-pointed version, never at a hardcoded path elsewhere in
    settings.json. This dimension closes that: every cache-dir path
    settings.json mentions must actually exist, or it is drift, not clean.

    An absent settings.json contributes nothing here (fail-soft, same as
    check_enabled's own skip). An unreadable/unparseable one is reported as
    a problem rather than read as "zero paths pinned" — undetermined must
    not collapse to clean ahead of a green "OK" line.
    """
    paths, undetermined = plugin_cache.cache_command_paths(
        PLUGIN_CACHE_ROOT, paths=[SETTINGS_PATH]
    )
    if undetermined:
        return [f"{undetermined} — cannot verify settings.json-referenced paths exist"], 0
    missing = sorted(p for p in paths if not os.path.exists(p))
    problems = [
        f"settings.json references {p}, which does not exist on disk "
        "(pruned, moved, or never built) — the hook/statusLine using it will "
        "fail at runtime"
        for p in missing
    ]
    return problems, len(paths)


def _bin_launcher_problem(crate):
    """Does crates/<crate> ship the bin/<crate> launcher its own binary needs?

    A plugin's hooks.json execs `bin/<crate>` (a POSIX-sh dispatcher that picks
    the per-platform binary), never the binary directly — so a plugin whose
    source declares a binary target but has no `bin/<crate>` script under its
    own crate directory is one `enabledPlugins` edit away from every one of its
    hooks failing at invocation time, and nothing in the rollout/enablement
    dimensions above would catch it (they only look at the DEPLOYED tree, not
    at whether the source ever had a launcher to deploy). taintguard shipped
    exactly this gap from birth (backlog 4ee2b335) until this check's own task
    added its launcher — this exists so the next new gate crate cannot repeat it.

    Scoped to plugins only (the caller already filtered to crates with a
    readable plugin.json): a bare workspace tool with no plugin.json
    (mutategate) is invoked directly and never through a packaged launcher, so
    it needs no exemption list here — it is simply never a candidate.
    """
    ships = _crate_ships_binary(crate)
    if ships is None:
        return (
            f"{crate}: could not determine from crates/{crate} whether this "
            "plugin ships a binary target, so whether it needs a bin/"
            f"{crate} launcher is undetermined"
        )
    if not ships:
        return None
    launcher = os.path.join(CRATES, crate, "bin", crate)
    if not os.path.isfile(launcher):
        return (
            f"{crate}: crates/{crate} declares a binary target, but there is "
            f"no crates/{crate}/bin/{crate} launcher script — hooks.json execs "
            "that path directly, so every hook of this plugin fails at "
            "invocation time regardless of rollout/enablement state "
            "(see crates/ctxrot/bin/ctxrot for the pattern)"
        )
    return None


def check_bin_launchers(plugins):
    """Return (problems, checked) for plugins missing their bin/<crate> launcher.

    Pure source-tree check, independent of the registry and of settings.json —
    unlike check_rollout/check_enabled, it never skips: there is no "absent
    registry" analogue for "does the source repo contain this file".
    """
    problems, checked = [], 0
    for crate, _pname, _src_ver in plugins:
        checked += 1
        problem = _bin_launcher_problem(crate)
        if problem:
            problems.append(problem)
    return problems, checked


def check_enabled(plugins):
    """Return (gate_failures, warnings, checked) for the enabledPlugins dimension.

    Returns (None, None, (0, 0)) when settings.json is absent — skipped
    fail-soft, exactly as an absent registry skips the rollout dimension. That
    early return is why the "GATE crate has no readable plugin.json"
    reconciliation no longer lives here: behind this return it never ran, so an
    unreadable GATE plugin.json on a machine with no settings.json was a fully
    silent exit 0. It is now unaccounted_gate_plugins(), called unconditionally
    from main().

    A plugin counts as enabled only if its "<name>@yukineko" key is present AND
    truthy: an explicit `false` is just as inert as an absent key, so both are
    treated the same way.
    """
    load_state, settings = _load_json(SETTINGS_PATH)
    if load_state == ABSENT:
        return None, None, (0, 0)
    if load_state == MALFORMED:
        # Settings Claude Code cannot parse == no plugin is enabled at all,
        # every gate inert. That is the failure this dimension exists to catch,
        # not a reason to skip it.
        return [
            f"settings are present but unparseable: {SETTINGS_PATH} ({settings}). "
            "Claude Code cannot read enabledPlugins from this file, so NO plugin "
            "is enabled and every GATE crate is inert."
        ], [], (0, 0)

    raw_enabled = settings.get("enabledPlugins") if isinstance(settings, dict) else None
    # A non-dict enabledPlugins (e.g. a JSON list) used to crash with an
    # uncaught AttributeError on `.get`. Coerce to {} for the lookups below, but
    # treat it as a hard FAILURE rather than "nothing enabled": the same
    # reasoning as a malformed file. A mistyped enabledPlugins is one Claude
    # Code cannot honour, so every plugin — every gate — is inert, and a green
    # "0 GATE plugin(s) enabled" line would be the fail-open we are closing.
    shape_failures = []
    if raw_enabled is not None and not isinstance(raw_enabled, dict):
        shape_failures.append(
            f"enabledPlugins in {SETTINGS_PATH} is a {type(raw_enabled).__name__}, "
            "not an object. Claude Code cannot honour it, so no plugin is enabled "
            "and every GATE crate is inert."
        )
    enabled = raw_enabled if isinstance(raw_enabled, dict) else {}

    # Not every GATE crate is a plugin (mutategate ships no plugin.json), so
    # count the gates actually reachable by this dimension rather than
    # implying all of GATE_CRATES was inspected.
    gate_failures, warnings, checked, gates_seen = list(shape_failures), [], 0, 0
    for crate, pname, _src_ver in plugins:
        checked += 1
        is_gate = crate in GATE_CRATES or pname in GATE_CRATES
        gates_seen += int(is_gate)
        key = f"{pname}@{OWNER}"
        if enabled.get(key):
            continue
        state = "disabled (set to false)" if key in enabled else "absent from enabledPlugins"
        if is_gate:
            gate_failures.append(
                f"{crate}: GATE crate is not enabled — {state} in {SETTINGS_PATH}. "
                "It is installed but inert: none of its hooks fire, so the gate silently guards nothing."
            )
        else:
            warnings.append(f"{crate}: not enabled ({state})")

    return gate_failures, warnings, (checked, gates_seen)


def cpr_eol_only():
    """The line-ending-only findings collected during this run."""
    return EOL_ONLY


def main():
    EOL_ONLY.clear()
    plugins, unverifiable = scan_plugins()
    # Unconditional, before either dimension can decide to skip itself.
    for crate in unaccounted_gate_plugins(plugins):
        unverifiable.append(
            f"{crate}: GATE crate has no readable plugin.json under crates/ "
            "(deleted, renamed, or corrupted). It cannot be verified as rolled "
            "out or as enabled, so it is treated as unguarded rather than "
            "silently dropped."
        )

    parked, parked_config = load_parked(plugins)

    rollout_problems, rollout_checked = check_rollout(plugins)
    gate_failures, warnings, (enabled_checked, gates_seen) = check_enabled(plugins)

    # Superseded version dirs are part of the same "is this actually rolled out"
    # verdict and share its remediation (run the rollout), so they are folded
    # into the rollout problem list rather than given a fourth exit code. They
    # are checked even when the registry is absent: leftover dirs are a property
    # of the cache, not of the registry pointing at them.
    stale_problems, stale_checked = check_stale_version_dirs()

    # Same reasoning as stale_problems: a settings.json pin referencing a
    # dead path is a property of the deployed fleet, not of the registry, so
    # it is checked even when the registry is absent and folded into the same
    # rollout verdict + exit code rather than given a fifth code.
    pin_problems, pin_checked = check_settings_pins()

    # Same reasoning as stale_problems/pin_problems: a missing bin/<crate>
    # launcher is a property of the SOURCE tree, not of the registry, so it is
    # checked even when the registry is absent and folded into the same
    # rollout verdict + exit code rather than given a sixth code.
    launcher_problems, launcher_checked = check_bin_launchers(plugins)

    # Reclassify parked plugins' findings out of the red lists — AFTER every
    # dimension has produced its findings, so a park can only ever move a
    # finding, never prevent one from being computed. Filtering `plugins` up
    # front instead would mean the checker never learns what the park is
    # covering for, and the report below could not print it.
    #
    # Deliberately BEFORE launcher_problems/stale_problems/pin_problems are
    # folded into `rollout_problems`, so those three stay red for a parked plugin
    # too. A park is a statement about DEPLOYMENT ("do not arm this yet"); it
    # says nothing about source-tree invariants (a missing bin/<crate> launcher
    # is a defect whether or not the plugin is armed) or about the cache (a
    # removable superseded dir is removable either way). `unverifiable` is left
    # alone for the same reason: a plugin.json that cannot be read is a broken
    # source tree, not an intentional state.
    parked_suppressed = {name: [] for name in parked}
    if parked:
        if rollout_problems:
            rollout_problems, suppressed = partition_parked(rollout_problems, parked)
            for name, items in suppressed.items():
                parked_suppressed[name].extend(items)
        if gate_failures:
            gate_failures, suppressed = partition_parked(gate_failures, parked)
            for name, items in suppressed.items():
                parked_suppressed[name].extend(items)

    if rollout_problems is None:
        print(f"installed_plugins.json not found: {REGISTRY_PATH}", file=sys.stderr)
        print("(set CLAUDE_PLUGIN_REGISTRY to override, or install at least one plugin first)", file=sys.stderr)
        print("SKIP: no registry to check against (not a failure — nothing is deployed yet)")

    # Folded in only after the SKIP notice above, so an absent registry still
    # reports itself as a skip rather than being masked by a cache finding.
    if stale_problems:
        rollout_problems = list(rollout_problems or []) + stale_problems
    if pin_problems:
        rollout_problems = list(rollout_problems or []) + pin_problems
    if launcher_problems:
        rollout_problems = list(rollout_problems or []) + launcher_problems

    if gate_failures is None:
        print(f"settings.json not found: {SETTINGS_PATH}", file=sys.stderr)
        print("(set CLAUDE_SETTINGS to override)", file=sys.stderr)
        print("SKIP: no settings to check enabledPlugins against (not a failure)")

    if parked:
        print(
            f"\nPARKED ON PURPOSE ({len(parked)} plugin(s) declared in "
            f"{PARKED_PATH} — NOT a failure, and NOT a green either):",
            file=sys.stderr,
        )
        for name in sorted(parked):
            print(f"  - {parked_report(name, parked[name], parked_suppressed[name])}", file=sys.stderr)
        print(
            "\nDo NOT 'fix' these by rolling them out or enabling them. That is "
            "what happened on 2026-08-04 (backlog a6f165cd): the red was read as "
            "a malfunction, taintguard was armed to clear it, and a known false "
            "positive then blocked real work. Clearing a park means resolving the "
            "reason above and deleting the entry — in that order.",
            file=sys.stderr,
        )

    # Line-ending-only differences: reported, never blocking. Printed BEFORE the
    # green rollout line below so a reader cannot take "no rollout drift" as
    # "deployed bytes == source bytes" without seeing this.
    if cpr_eol_only():
        total = sum(len(rels) for _c, rels in cpr_eol_only())
        print(
            f"NOTE ({total} deployed file(s) across {len(cpr_eol_only())} plugin(s) "
            "differ from the crate ONLY in line endings — not drift, not "
            "byte-identical either):",
            file=sys.stderr,
        )
        for crate, rels in cpr_eol_only():
            shown = ", ".join(rels[:3]) + (f", +{len(rels) - 3} more" if len(rels) > 3 else "")
            print(f"  - {crate}: {shown}", file=sys.stderr)
        print(
            "Cause: two checkouts of the same commit can differ here (git calls "
            "them identical: `git ls-files --eol` reports i/lf w/lf "
            "attr/text=auto), and the cache was rsynced from whichever tree ran "
            "the rollout. No rollout is needed. To make the bytes match too, "
            "check the trees out with the same line endings and re-run "
            "scripts/rollout-plugins.sh --plugin <name> --force.",
            file=sys.stderr,
        )

    # Warnings never affect the exit code — disabling a non-gate plugin is a
    # legitimate user choice, so this informs without blocking.
    if warnings:
        print(f"NOTE ({len(warnings)} non-gate plugin(s) not enabled — fine if intentional):", file=sys.stderr)
        for w in warnings:
            print(f"  - {w}", file=sys.stderr)

    # Each dimension reports its own verdict independently, so a failure in one
    # never suppresses the other's result line. That pairing is the diagnostic
    # payload of the propguard case: "deployed at the right version" AND
    # "inert because it isn't enabled" are both true at once, and seeing the
    # green rollout line next to the gate failure is what makes it legible.
    # A green line is a claim that a whole population was verified, so it must
    # not be printed over a population the scan silently lost. `unverifiable`
    # means some crate under crates/ could not be inspected at all: the counts
    # below would then describe a smaller fleet than exists, which reads exactly
    # like a clean run.
    # A park must never be laundered into a green. Every OK line below is a claim
    # about a POPULATION, so each one subtracts the plugins whose findings were
    # moved to the parked report and names the remainder — otherwise "41 plugins
    # deployed at their source version" would be printed over a plugin that is
    # deliberately NOT deployed at its source version, which is the same class of
    # lie as printing it over a plugin the scan lost.
    def _minus_parked(total, names):
        """(reported_total, suffix) with parked members of `names` excluded."""
        hit = sorted(n for n in names if n in parked)
        if not hit:
            return total, ""
        return total - len(hit), f" ({len(hit)} parked, reported above: {', '.join(hit)})"

    all_names = [n for crate, pname, _v in plugins for n in (crate, pname) if n]
    if rollout_problems is not None and not rollout_problems and not unverifiable:
        held = (
            f"; {stale_checked} superseded dir(s) remain, all held by a live session"
            if stale_checked
            else "; no superseded version dir left in the cache"
        )
        shown, parked_note = _minus_parked(rollout_checked, set(all_names))
        # The claim is byte-level, so it must name its own exception when the
        # line-ending class fired — otherwise this sentence is what a reader uses
        # to skip the NOTE printed above it.
        identical = (
            "and file-for-file identical to their crate apart from the "
            f"line-ending difference(s) noted above in {len(cpr_eol_only())} "
            "plugin(s)"
            if cpr_eol_only()
            else "and file-for-file identical to their crate"
        )
        print(
            f"OK: {shown} plugins deployed at their source version "
            f"{identical} (no rollout drift){held}"
            f"{parked_note}"
        )
    if (
        gate_failures is not None
        and not gate_failures
        and gates_seen == len(EXPECTED_GATE_PLUGINS)
    ):
        shown, parked_note = _minus_parked(gates_seen, set(EXPECTED_GATE_PLUGINS))
        checked_shown, _ = _minus_parked(enabled_checked, set(all_names))
        print(
            f"OK: all {shown} GATE plugin(s) enabled "
            f"({checked_shown} plugins checked against enabledPlugins){parked_note}"
        )

    if parked_config:
        print(
            f"\nUNUSABLE PARKED DECLARATION ({len(parked_config)} problem(s)): the "
            "file that says which reds are intentional cannot be trusted, so NO "
            "red below was treated as intentional:",
            file=sys.stderr,
        )
        for p in parked_config:
            print(f"  - {p}", file=sys.stderr)
        print(
            f"\nFix: repair {PARKED_PATH}. Each entry needs a non-empty "
            f"{'/'.join(PARKED_REQUIRED)} and a name that matches a plugin under "
            "crates/. Deleting the file entirely is also valid — it means nothing "
            "is parked.",
            file=sys.stderr,
        )

    if unverifiable:
        print(
            f"\nUNVERIFIABLE PLUGIN ({len(unverifiable)} problem(s)): the checker "
            "could not read these, so neither dimension's verdict covers them:",
            file=sys.stderr,
        )
        for p in unverifiable:
            print(f"  - {p}", file=sys.stderr)
        print(
            "\nFix: repair or restore the plugin.json named above. Neither "
            "rollout-plugins.sh nor an enabledPlugins edit can fix this class.",
            file=sys.stderr,
        )

    if rollout_problems:
        print(
            f"ROLLOUT DRIFT ({len(rollout_problems)} problem(s) across {rollout_checked} plugins checked):",
            file=sys.stderr,
        )
        for p in rollout_problems:
            print(f"  - {p}", file=sys.stderr)
        print(rollout_hint(), file=sys.stderr)

    if gate_failures:
        print(
            # The class now covers three shapes — disabled, unverifiable (no
            # readable plugin.json), and unusable settings — so the old
            # "N of M not enabled" phrasing could read "1 of 0", which is
            # nonsense when the settings never parsed at all.
            f"\nDISABLED OR UNVERIFIABLE GATE CRATE ({len(gate_failures)} problem(s); "
            f"{gates_seen} of {len(EXPECTED_GATE_PLUGINS)} GATE plugin(s) inspected):",
            file=sys.stderr,
        )
        for p in gate_failures:
            print(f"  - {p}", file=sys.stderr)
        print(
            f"\nFix: add \"<name>@{OWNER}\": true to enabledPlugins in {SETTINGS_PATH} "
            "(rollout-plugins.sh does NOT enable plugins), then restart Claude Code.",
            file=sys.stderr,
        )

    # Distinct exit code per failure CLASS so callers can route the reader to
    # the right fix. .githooks/pre-push used to branch on a bare non-zero and
    # unconditionally print "run scripts/rollout-plugins.sh --plugin <name>" —
    # which, by this script's own hint, does NOT enable plugins. Telling someone
    # to run it for a disabled-gate failure sends them to the one command that
    # cannot possibly fix it. Rollout wins the tie because its own remediation
    # is a prerequisite for reasoning about the deployed state at all; the
    # enablement block above still printed its detail and its own fix line.
    # UNVERIFIABLE outranks both: when a plugin.json cannot be read, neither
    # dimension's verdict covers that plugin, so acting on either remediation
    # first is acting on an incomplete picture. It is also the only class whose
    # fix is neither rollout-plugins.sh nor a settings.json edit, which is
    # exactly why it needs its own code rather than borrowing one of theirs.
    #
    # PARKED_CONFIG outranks everything, for the same reason UNVERIFIABLE
    # outranks the two dimensions, one level up: when the parked declaration is
    # unusable, the reader cannot tell which of the reds below were meant to be
    # there. Acting on any of the other remedies first risks arming something
    # that was parked on purpose — the 2026-08-04 incident exactly.
    if parked_config:
        return RC_PARKED_CONFIG
    if unverifiable:
        return RC_UNVERIFIABLE
    if rollout_problems:
        return RC_ROLLOUT
    if gate_failures:
        return RC_ENABLEMENT
    return RC_OK


if __name__ == "__main__":
    sys.exit(main())
