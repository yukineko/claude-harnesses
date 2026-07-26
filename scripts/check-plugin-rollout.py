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

The rollout dimension checks TWO things, because a matching version string is
not evidence that the deployed bytes are current:

  (a) VERSION — the registry's version equals the source's plugin.json version.
  (b) PROVENANCE — the binary in that install dir is provably built from source
      that has not moved since. rebuild-plugins.sh drops a `.deployed-from.json`
      beside every host binary it places, recording the commit and whether the
      tree was dirty; this script asks git whether crates/<plugin> or a shared
      crate it links (harness-core) changed since that commit.

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
present in the deployed tree. Two plugins — daily-report and scout — are skills
and hooks only, with no Rust crate at all; they have no compiled artifact whose
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
import sys

OWNER = "yukineko"

# Exit codes, one per failure CLASS (see the module docstring). Named so callers
# — .githooks/pre-push in particular — can branch on the class instead of on a
# bare non-zero and then guess at which remediation to print.
RC_OK = 0
RC_ROLLOUT = 1
RC_ENABLEMENT = 2
RC_UNVERIFIABLE = 3

REPO = os.getcwd()
CRATES = os.path.join(REPO, "crates")

# Filename of the provenance manifest rebuild-plugins.sh drops beside every host
# binary it places, recording the commit that binary was built from.
PROVENANCE_FILE = ".deployed-from.json"

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
    with no Rust crate under crates/<name> — daily-report and scout are the two
    such plugins today). Those have no compiled artifact whose provenance could
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
    return any(e.endswith("-" + HOST_SUFFIX) for e in entries)
REGISTRY_PATH = os.environ.get(
    "CLAUDE_PLUGIN_REGISTRY", os.path.expanduser("~/.claude/plugins/installed_plugins.json")
)
SETTINGS_PATH = os.environ.get(
    "CLAUDE_SETTINGS", os.path.expanduser("~/.claude/settings.json")
)

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
    "mutategate",
    "overwatch",
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


def main():
    plugins, unverifiable = scan_plugins()
    # Unconditional, before either dimension can decide to skip itself.
    for crate in unaccounted_gate_plugins(plugins):
        unverifiable.append(
            f"{crate}: GATE crate has no readable plugin.json under crates/ "
            "(deleted, renamed, or corrupted). It cannot be verified as rolled "
            "out or as enabled, so it is treated as unguarded rather than "
            "silently dropped."
        )

    rollout_problems, rollout_checked = check_rollout(plugins)
    gate_failures, warnings, (enabled_checked, gates_seen) = check_enabled(plugins)

    if rollout_problems is None:
        print(f"installed_plugins.json not found: {REGISTRY_PATH}", file=sys.stderr)
        print("(set CLAUDE_PLUGIN_REGISTRY to override, or install at least one plugin first)", file=sys.stderr)
        print("SKIP: no registry to check against (not a failure — nothing is deployed yet)")

    if gate_failures is None:
        print(f"settings.json not found: {SETTINGS_PATH}", file=sys.stderr)
        print("(set CLAUDE_SETTINGS to override)", file=sys.stderr)
        print("SKIP: no settings to check enabledPlugins against (not a failure)")

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
    if rollout_problems is not None and not rollout_problems and not unverifiable:
        print(f"OK: {rollout_checked} plugins deployed at their source version (no rollout drift)")
    if (
        gate_failures is not None
        and not gate_failures
        and gates_seen == len(EXPECTED_GATE_PLUGINS)
    ):
        print(
            f"OK: all {gates_seen} GATE plugin(s) enabled "
            f"({enabled_checked} plugins checked against enabledPlugins)"
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
    if unverifiable:
        return RC_UNVERIFIABLE
    if rollout_problems:
        return RC_ROLLOUT
    if gate_failures:
        return RC_ENABLEMENT
    return RC_OK


if __name__ == "__main__":
    sys.exit(main())
