#!/usr/bin/env python3
"""Verify plugin version lockstep across the three canonical sources.

For every plugin under crates/<name>/ the version MUST agree across:
  - crates/<name>/Cargo.toml           [package].version   (skip if skill-only: no Cargo.toml)
  - crates/<name>/.claude-plugin/plugin.json   .version
  - .claude-plugin/marketplace.json    the entry whose .name matches plugin.json .name

Canonical direction: Cargo.toml == plugin.json is the source of truth; marketplace.json
must never lag. This is a repo bug when it drifts — a stale marketplace entry makes
sync-plugin-assets.sh resolve the wrong cache dir and ships an old version to users.

Exit 0 if fully consistent, 1 if any drift (usable as a pre-commit / pre-rebuild gate).
Run from the repo root:  python3 scripts/check-plugin-versions.py
"""
import json
import os
import re
import sys

REPO = os.getcwd()
CRATES = os.path.join(REPO, "crates")
MP_PATH = os.path.join(REPO, ".claude-plugin", "marketplace.json")


def cargo_package_version(path):
    """Return the [package].version string, or None if absent.

    Line-anchored so `rust-version` is never mistaken for `version`, and
    `version.workspace = true` inheritance is reported as ('ws', ...).
    """
    txt = open(path).read()
    m = re.search(r"\[package\](.*?)(\n\[|\Z)", txt, re.S)
    sec = m.group(1) if m else txt
    if re.search(r"^\s*version\s*\.\s*workspace\s*=\s*true", sec, re.M) or re.search(
        r"^\s*version\s*=\s*\{\s*workspace\s*=\s*true", sec, re.M
    ):
        return "<workspace-inherited>"
    m = re.search(r'^\s*version\s*=\s*"([^"]+)"', sec, re.M)
    return m.group(1) if m else None


def main():
    mp = json.load(open(MP_PATH))
    plugins = mp.get("plugins") if isinstance(mp, dict) else mp
    mp_ver = {p["name"]: p.get("version") for p in plugins if isinstance(p, dict) and "name" in p}

    problems = []
    checked = 0
    for name in sorted(os.listdir(CRATES)):
        d = os.path.join(CRATES, name)
        pj = os.path.join(d, ".claude-plugin", "plugin.json")
        if not os.path.isfile(pj):
            continue  # not a plugin (e.g. harness-core, integration-tests)
        pjd = json.load(open(pj))
        pname, pjv = pjd.get("name"), pjd.get("version")
        checked += 1

        mpv = mp_ver.get(pname, "<MISSING-from-marketplace>")
        if pjv != mpv:
            problems.append(f"{name}: plugin.json={pjv} != marketplace={mpv}")

        cargo = os.path.join(d, "Cargo.toml")
        if os.path.isfile(cargo):
            cv = cargo_package_version(cargo)
            if cv != pjv:
                problems.append(f"{name}: Cargo.toml={cv} != plugin.json={pjv}")

    if problems:
        print(f"VERSION DRIFT ({len(problems)} problem(s) across {checked} plugins):", file=sys.stderr)
        for p in problems:
            print(f"  - {p}", file=sys.stderr)
        print(
            "\nFix: bump the lagging file so Cargo.toml == plugin.json == marketplace.json.",
            file=sys.stderr,
        )
        return 1
    print(f"OK: {checked} plugins version-consistent across Cargo.toml / plugin.json / marketplace.json")
    return 0


if __name__ == "__main__":
    sys.exit(main())
