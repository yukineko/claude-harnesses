#!/usr/bin/env python3
"""Verify every plugin's source version is actually deployed to the live plugin cache.

check-plugin-versions.py only verifies the three SOURCE files agree with each
other (Cargo.toml / plugin.json / marketplace.json). It says nothing about
whether that agreed-upon version was ever rolled out via scripts/rollout-plugins.sh
to `~/.claude/plugins/installed_plugins.json` (the registry the live harness
actually reads from). A commit can bump all three source files, pass that
gate, and still never take effect for any running session — this happened to
5 plugins in one sitting before this script existed (hypothesis, condukt,
compass, blastguard, overwatch all sat committed-but-undeployed).

For every plugin under crates/<name>/, compares:
  - crates/<name>/.claude-plugin/plugin.json  .version        (source of truth)
  - installed_plugins.json .plugins["<name>@yukineko"][0].version   (deployed)

Exit 0 if every plugin's deployed version matches its source version, 1 if
any plugin is behind (usable as a pre-push / CI gate, mirroring
check-plugin-versions.py and check-version-bumped.py).

Registry path defaults to ~/.claude/plugins/installed_plugins.json; override
with CLAUDE_PLUGIN_REGISTRY (same env var rollout-plugins.sh honors) so this
is testable against a fixture registry.

Run from the repo root:  python3 scripts/check-plugin-rollout.py
"""
import json
import os
import sys

OWNER = "yukineko"
REPO = os.getcwd()
CRATES = os.path.join(REPO, "crates")
REGISTRY_PATH = os.environ.get(
    "CLAUDE_PLUGIN_REGISTRY", os.path.expanduser("~/.claude/plugins/installed_plugins.json")
)


def main():
    if not os.path.isfile(REGISTRY_PATH):
        print(f"installed_plugins.json not found: {REGISTRY_PATH}", file=sys.stderr)
        print("(set CLAUDE_PLUGIN_REGISTRY to override, or install at least one plugin first)", file=sys.stderr)
        print("SKIP: no registry to check against (not a failure — nothing is deployed yet)")
        return 0

    registry = json.load(open(REGISTRY_PATH, encoding="utf-8"))
    entries = registry.get("plugins", {}) if isinstance(registry, dict) else {}

    problems = []
    checked = 0
    for name in sorted(os.listdir(CRATES)):
        d = os.path.join(CRATES, name)
        pj = os.path.join(d, ".claude-plugin", "plugin.json")
        if not os.path.isfile(pj):
            continue  # not a plugin (e.g. harness-core, integration-tests)
        pjd = json.load(open(pj, encoding="utf-8"))
        pname, src_ver = pjd.get("name"), pjd.get("version")
        checked += 1

        key = f"{pname}@{OWNER}"
        entry = entries.get(key)
        if not entry:
            problems.append(f"{name}: source={src_ver} but never installed (no '{key}' in registry)")
            continue
        reg_ver = entry[0].get("version") if isinstance(entry, list) and entry else None
        if reg_ver != src_ver:
            problems.append(f"{name}: source={src_ver} registry={reg_ver} <- rollout-plugins.sh not run since bump")

    if problems:
        print(f"ROLLOUT DRIFT ({len(problems)} problem(s) across {checked} plugins checked):", file=sys.stderr)
        for p in problems:
            print(f"  - {p}", file=sys.stderr)
        print(
            "\nFix: scripts/rollout-plugins.sh --plugin <name> (add --canary for GATE crates: "
            "blastguard/propguard/stuckguard/mutategate/overwatch).",
            file=sys.stderr,
        )
        return 1
    print(f"OK: {checked} plugins deployed at their source version (no rollout drift)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
