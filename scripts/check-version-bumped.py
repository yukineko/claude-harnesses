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

It also enforces the SHARED-CRATE half of the same rule: crates/harness-core is
statically linked into every plugin binary, so a change there changes ~36 shipped
binaries while no plugin.json moves. The gate used to diff only crates/<plugin>/,
so harness-core could move with no version anywhere moving — leaving a version
string that does not identify what shipped (backlog 32170548). harness-core
carries its own [package].version, and a change to its LINKED source must bump
it.

Why the shared crate rather than the 36 plugins: bumping every linking plugin is
the semantically purest reading, but it moves 108 files and 36 marketplace.json
lines per harness-core edit, which collides with every parallel session (see
CLAUDE.md §8). Byte identity is already machine-checkable without it —
check-plugin-rollout.py's SHARED_SOURCE_PATHS reports every plugin as drifted
when harness-core moves, and .deployed-from.json pins the exact commit — so the
plugin bumps would buy a version string, at that cost, for a repo that does not
redistribute by version. Decided with the repo owner, 2026-07-30.

TWO carve-outs, each argued at its implementation site below and each ANNOUNCED
rather than applied silently:
  1. a plugin whose changed paths are ALL under crates/<name>/bin/ (compiled
     output, not source) does not require a bump. Mixed bin+source is still
     enforced. Covered by scripts/tests/version-bumped-bin-only.sh.
  2. a harness-core change confined to paths that are NOT linked into a plugin
     binary (tests/, *.md) does not require a bump. This is structural, not a
     guess: crates/harness-core/tests/ is a separate integration-test target.
     Anything else under harness-core — including a #[cfg(test)] block inside
     src/, which cannot be identified without parsing — counts as linked and
     demands the bump, because §3 says an undecidable case resolves to the
     restrictive side. Covered by scripts/tests/version-bumped-shared-crate.sh.
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


# The one crate every plugin statically links. A change here changes every
# shipped binary, so its own version is what must move.
SHARED_CRATE = "harness-core"

# Paths under the shared crate that are NOT compiled into a plugin binary.
# Structural, not heuristic: `tests/` is a separate integration-test target and
# `*.md` is prose. Everything else counts as linked (fail-closed).
def _is_linked_shared_path(path):
    if path.startswith(f"{CRATES}/{SHARED_CRATE}/tests/"):
        return False
    if path.endswith(".md"):
        return False
    return True


def package_version_from_toml(text):
    """`[package].version` from Cargo.toml text, or None if absent/unreadable.

    None means "could not determine", and every caller treats that as a failure
    rather than a pass — a version that cannot be read has not been shown to have
    moved (CLAUDE.md §3).
    """
    if text is None:
        return None
    in_package = False
    for raw in text.splitlines():
        line = raw.strip()
        if line.startswith("["):
            in_package = line == "[package]"
            continue
        if in_package:
            m = re.match(r'version\s*=\s*"([^"]+)"', line)
            if m:
                return m.group(1)
    return None


def crate_version_at(ref, path):
    r = git("show", f"{ref}:{path}")
    if r.returncode != 0:
        return None
    return package_version_from_toml(r.stdout)


def crate_version_worktree(path):
    try:
        with open(path, encoding="utf-8") as f:
            return package_version_from_toml(f.read())
    except OSError:
        return None


def check_shared_crate(base):
    """Enforce the shared-crate bump. Returns (verdict, message).

    verdict is "ok", "exempt", "fail", or "error" — four answers rather than a
    bool, so "could not diff" cannot be recorded as "nothing changed".
    """
    manifest = f"{CRATES}/{SHARED_CRATE}/Cargo.toml"
    if not os.path.isfile(manifest):
        # Not this repo's layout (or running in a sandbox without the shared
        # crate). Nothing to enforce, and nothing to claim.
        return "ok", None

    diff = git("diff", "--name-only", base, "--", f"{CRATES}/{SHARED_CRATE}/")
    if diff.returncode != 0:
        return "error", (
            f"cannot diff {CRATES}/{SHARED_CRATE} against {base}: "
            f"{diff.stderr.strip()}"
        )
    changed = [ln for ln in diff.stdout.splitlines() if ln.strip()]
    if not changed:
        return "ok", None

    linked = [p for p in changed if _is_linked_shared_path(p)]
    if not linked:
        return "exempt", (
            f"check-version-bumped: {SHARED_CRATE} changed in "
            f"{len(changed)} file(s), all of which are not linked into any plugin "
            f"binary (tests/ or *.md); no bump required"
        )

    base_v = crate_version_at(base, manifest)
    if base_v is None:
        # Absent at base (new crate), or its manifest was unreadable there. The
        # first is legitimately exempt; the second cannot be told apart here, and
        # a shared crate that did not exist at base cannot have "changed" in the
        # sense this rule is about.
        return "ok", None
    cur_v = crate_version_worktree(manifest)
    if cur_v is None:
        return "fail", (
            f"{SHARED_CRATE}: cannot read [package].version from {manifest} "
            f"(base was {base_v}). A version that cannot be read has NOT been "
            f"shown to have moved."
        )
    bs, cs = semver(base_v), semver(cur_v)
    if bs is None or cs is None or not (cs > bs):
        return "fail", (
            f"{SHARED_CRATE}: version still {cur_v} (base {base_v}), but "
            f"{len(linked)} linked file(s) changed:\n"
            + "\n".join(f"        {p}" for p in linked[:8])
            + (f"\n        ... (+{len(linked) - 8} more)" if len(linked) > 8 else "")
        )
    return "ok", None


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

    # The shared crate (harness-core) is not a plugin, so the loop above skipped
    # it; its own version is what must move when it changes.
    shared_verdict, shared_msg = check_shared_crate(base)
    if shared_verdict == "error":
        print(f"check-version-bumped: {shared_msg}", file=sys.stderr)
        return 2
    if shared_verdict == "exempt":
        print(shared_msg)
    if shared_verdict == "fail":
        offenders_shared = shared_msg
    else:
        offenders_shared = None

    # Announce the carve-out. A gate that narrows its own coverage must say so;
    # a silent exemption reads downstream as "nothing changed there".
    if bin_only:
        total = sum(n for _, n in bin_only)
        names = ", ".join(name for name, _ in bin_only)
        print(
            f"check-version-bumped: {len(bin_only)} plugin(s) changed ONLY under "
            f"bin/ ({total} derived-artifact file(s)); no bump required: {names}"
        )

    if offenders_shared:
        print(
            f"VERSION-BUMP GATE FAILED: {SHARED_CRATE} changed without a version "
            f"bump vs {base}.",
            file=sys.stderr,
        )
        print(f"  - {offenders_shared}", file=sys.stderr)
        print(
            f"\nEvery plugin statically links {SHARED_CRATE}, so this change alters "
            f"every shipped binary while no plugin.json moves. Bump\n"
            f"[package].version in {CRATES}/{SHARED_CRATE}/Cargo.toml (>= micro). "
            f"The 36 linking plugins do NOT need bumps.",
            file=sys.stderr,
        )
        if not offenders:
            return 1

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

    print(
        f"OK: no changed plugin left un-bumped vs {base} ({checked} plugins "
        f"scanned, plus the {SHARED_CRATE} shared crate)"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
