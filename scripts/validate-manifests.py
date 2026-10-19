#!/usr/bin/env python3
"""Validate the marketplace catalog and every plugin manifest.

The harness distributes ~32 plugins via a single `git-subdir` marketplace
(`.claude-plugin/marketplace.json`), each backed by a `crates/<plugin>/.claude-plugin/plugin.json`.
A malformed manifest (missing field, non-kebab name, version drift, dangling
path) only surfaces at *install* time on a user's machine. This script makes
that a merge-blocking CI check instead.

Self-contained: stdlib only, no third-party action or JSON-schema dependency, so
it runs identically in CI and locally (`python3 scripts/validate-manifests.py`).
Exit 0 = all valid; exit 1 = one or more violations (all are printed).
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

KEBAB = re.compile(r"^[a-z0-9]+(?:-[a-z0-9]+)*$")
SEMVER = re.compile(r"^\d+\.\d+\.\d+(?:[-+].+)?$")

REPO = Path(__file__).resolve().parent.parent
MARKETPLACE = REPO / ".claude-plugin" / "marketplace.json"


def load_json(path: Path, errors: list[str]):
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        errors.append(f"{path}: file not found")
    except json.JSONDecodeError as e:
        errors.append(f"{path}: invalid JSON: {e}")
    return None


def check_plugin_manifest(path: Path, errors: list[str]) -> str | None:
    """Validate one crate plugin.json. Returns the declared name (or None)."""
    data = load_json(path, errors)
    if data is None:
        return None
    rel = path.relative_to(REPO)
    name = data.get("name")
    if not name:
        errors.append(f"{rel}: missing required field 'name'")
    elif not KEBAB.match(name):
        errors.append(f"{rel}: name '{name}' is not kebab-case")
    if not data.get("description"):
        errors.append(f"{rel}: missing required field 'description'")
    # version is optional in plugin.json (the marketplace entry is authoritative)
    # but when present it must be valid semver.
    ver = data.get("version")
    if ver is not None and not SEMVER.match(str(ver)):
        errors.append(f"{rel}: version '{ver}' is not semver")
    # NB: the plugin name may legitimately differ from the crate dir name
    # (e.g. crate `run-book` ships plugin `runbook`). The load-bearing invariant
    # — manifest name == marketplace entry for this path — is checked in main().
    return name


def check_marketplace(errors: list[str]) -> dict[str, dict]:
    """Validate marketplace.json. Returns {plugin_name: entry}."""
    data = load_json(MARKETPLACE, errors)
    entries: dict[str, dict] = {}
    if data is None:
        return entries
    for field in ("name", "owner", "plugins"):
        if field not in data:
            errors.append(f"marketplace.json: missing required field '{field}'")
    for i, p in enumerate(data.get("plugins", [])):
        where = f"marketplace.json plugins[{i}]"
        name = p.get("name")
        if not name:
            errors.append(f"{where}: missing 'name'")
            continue
        if not KEBAB.match(name):
            errors.append(f"{where}: name '{name}' is not kebab-case")
        if name in entries:
            errors.append(f"{where}: duplicate plugin name '{name}'")
        entries[name] = p
        ver = p.get("version")
        if ver is None:
            errors.append(f"{where} ({name}): missing 'version'")
        elif not SEMVER.match(str(ver)):
            errors.append(f"{where} ({name}): version '{ver}' is not semver")
        src = p.get("source")
        if not isinstance(src, dict):
            errors.append(f"{where} ({name}): missing 'source' object")
        else:
            for sf in ("source", "url", "path", "ref"):
                if sf not in src:
                    errors.append(f"{where} ({name}): source missing '{sf}'")
            path = src.get("path")
            if path and not (REPO / path).is_dir():
                errors.append(f"{where} ({name}): source.path '{path}' does not exist")
    return entries


def main() -> int:
    errors: list[str] = []

    market = check_marketplace(errors)

    manifests = sorted(REPO.glob("crates/*/.claude-plugin/plugin.json"))
    if not manifests:
        errors.append("no crates/*/.claude-plugin/plugin.json found")
    # crate-path → declared plugin name (from each crate's plugin.json)
    by_path: dict[str, str] = {}
    for m in manifests:
        name = check_plugin_manifest(m, errors)
        crate_path = str(m.parent.parent.relative_to(REPO)).replace("\\", "/")
        if name:
            by_path[crate_path] = name

    # Cross-check the two manifests agree: the marketplace entry pointing at a
    # crate path must carry the same plugin name as that crate's plugin.json.
    for name, entry in market.items():
        path = (entry.get("source") or {}).get("path", "")
        declared = by_path.get(path)
        if declared is not None and declared != name:
            errors.append(
                f"marketplace plugin '{name}' (path {path}) disagrees with that "
                f"crate's plugin.json name '{declared}'"
            )

    if errors:
        print(f"✗ manifest validation failed ({len(errors)} issue(s)):", file=sys.stderr)
        for e in errors:
            print(f"  - {e}", file=sys.stderr)
        return 1

    print(
        f"✓ manifests valid: marketplace ({len(market)} plugins) "
        f"+ {len(manifests)} crate manifests"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
