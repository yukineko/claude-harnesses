#!/usr/bin/env python3
"""Gate: a touched plugin MUST have its version bumped (禁忌 rule).

Enforces the CLAUDE.md "変更したら必ず version を上げる" rule: if any file under
crates/<name>/ differs from a base ref, that plugin's version (in plugin.json)
must be strictly greater than it was at the base. Committing/pushing a plugin
change with the version unchanged is forbidden.

This complements scripts/check-plugin-versions.py (which checks lockstep across
Cargo.toml / plugin.json / marketplace.json). Run BOTH:
  - check-plugin-versions.py  → the three files agree right now
  - check-version-bumped.py   → a changed plugin actually got bumped vs base

Usage (run from repo root):
  python3 scripts/check-version-bumped.py                 # base = HEAD (pre-commit: working tree vs last commit)
  python3 scripts/check-version-bumped.py --base origin/main   # CI / pre-push: vs a pushed ref

Exit 0 if every changed plugin was bumped (or nothing relevant changed); exit 1
lists each plugin that changed without a version bump. New plugins (absent at
base) are OK. Deleted plugins are ignored.

ONE carve-out, argued at its implementation site below: a plugin whose changed
paths are ALL under crates/<name>/bin/ (compiled output, not source) does not
require a bump. Mixed bin+source changes are still enforced, and every exempted
plugin is printed — the exemption is never silent. Covered by
scripts/tests/version-bumped-bin-only.sh.
"""
import argparse
import json
import os
import re
import subprocess
import sys

CRATES = "crates"
PLUGIN_JSON_REL = ".claude-plugin/plugin.json"


def git(*args):
    return subprocess.run(["git", *args], capture_output=True, text=True, encoding="utf-8")


def semver(v):
    """('1','2','3') tuple for comparison; non-numeric parts sort low. None-safe."""
    if v is None:
        return None
    core = re.split(r"[-+]", v.strip(), 1)[0]  # drop pre-release / build metadata
    parts = core.split(".")
    out = []
    for p in parts:
        out.append(int(p) if p.isdigit() else -1)
    while len(out) < 3:
        out.append(0)
    return tuple(out[:3])


def plugin_version_at(ref, path):
    """version string from plugin.json at `ref`, or None if the file/field is absent there."""
    r = git("show", f"{ref}:{path}")
    if r.returncode != 0:
        return None
    try:
        return json.loads(r.stdout).get("version")
    except json.JSONDecodeError:
        return None


def plugin_version_worktree(path):
    try:
        with open(path, encoding="utf-8") as f:
            return json.load(f).get("version")
    except (FileNotFoundError, json.JSONDecodeError):
        return None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--base", default="HEAD", help="ref to compare against (default: HEAD)")
    args = ap.parse_args()
    base = args.base

    # sanity: base must resolve
    if git("rev-parse", "--verify", "--quiet", base).returncode != 0:
        print(f"check-version-bumped: base ref '{base}' does not resolve", file=sys.stderr)
        return 2

    offenders = []
    bin_only = []
    checked = 0
    if not os.path.isdir(CRATES):
        print("check-version-bumped: no crates/ dir (run from repo root)", file=sys.stderr)
        return 2

    for name in sorted(os.listdir(CRATES)):
        pj_rel = f"{CRATES}/{name}/{PLUGIN_JSON_REL}"
        if not os.path.isfile(pj_rel):
            continue  # not a plugin (harness-core, integration-tests, ...)
        checked += 1

        # did anything under this plugin change vs base? (working tree vs base)
        diff = git("diff", "--name-only", base, "--", f"{CRATES}/{name}/")
        if diff.returncode != 0:
            print(f"check-version-bumped: git diff failed for {name}: {diff.stderr.strip()}", file=sys.stderr)
            return 2
        changed = [ln for ln in diff.stdout.splitlines() if ln.strip()]
        if not changed:
            continue  # unchanged plugin — nothing to enforce

        # Derived-artifact carve-out: crates/<name>/bin/* holds COMPILED output,
        # not source. A rebuild of identical source produces different bytes
        # (a rebuild of identical source produces different bytes, so those
        # diffs are build nondeterminism, not a meaningful signal). Compiled
        # binaries are now gitignored (personal repo, no distribution), so this
        # is a backstop for a stray tracked artifact. Demanding a version bump
        # for one demands a bump for a
        # non-change, and it blocked integrating CI's own `ci: rebuild plugin
        # binaries` commit (observed 2026-07-23: 34 plugins, zero source files).
        # The carve-out is narrow ON PURPOSE: it applies only when EVERY changed
        # path is under bin/, so a source change travelling alongside binaries is
        # still caught. It is also announced below rather than applied silently.
        bin_prefix = f"{CRATES}/{name}/bin/"
        if all(p.startswith(bin_prefix) for p in changed):
            bin_only.append((name, len(changed)))
            continue

        base_v = plugin_version_at(base, pj_rel)
        if base_v is None:
            continue  # new plugin (absent at base) — no bump required
        cur_v = plugin_version_worktree(pj_rel)

        bs, cs = semver(base_v), semver(cur_v)
        if cs is None or bs is None or not (cs > bs):
            offenders.append((name, base_v, cur_v, changed))

    # Announce the carve-out. A gate that narrows its own coverage must say so;
    # a silent exemption reads downstream as "nothing changed there".
    if bin_only:
        total = sum(n for _, n in bin_only)
        names = ", ".join(name for name, _ in bin_only)
        print(
            f"check-version-bumped: {len(bin_only)} plugin(s) changed ONLY under "
            f"bin/ ({total} derived-artifact file(s)); no bump required: {names}"
        )

    if offenders:
        print(
            f"VERSION-BUMP GATE FAILED: {len(offenders)} changed plugin(s) not bumped vs {base}:",
            file=sys.stderr,
        )
        for name, bv, cv, files in offenders:
            print(f"  - {name}: version still {cv} (base {bv}); changed files:", file=sys.stderr)
            for fpath in files[:8]:
                print(f"        {fpath}", file=sys.stderr)
            if len(files) > 8:
                print(f"        ... (+{len(files) - 8} more)", file=sys.stderr)
        print(
            "\nFix: bump this plugin's version (>= micro) in Cargo.toml + plugin.json + marketplace.json.\n"
            "禁忌: a touched plugin must never keep the same version.",
            file=sys.stderr,
        )
        return 1

    print(f"OK: no changed plugin left un-bumped vs {base} ({checked} plugins scanned)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
