#!/usr/bin/env python3
"""Delete cached plugin version dirs that are no longer current.

rollout-plugins.sh used to state, as designed behaviour, that it "never deletes
old version dirs". The cost of that was measured 2026-07-26: 265 stale dirs
holding 1.29 GB, up to 25 versions deep for a single plugin. Worse than the
disk, a stale dir is a live hazard — `claude plugin install` can pin to one, and
`.deployed-from.json` provenance says nothing about a dir the registry does not
point at. So the rollout now prunes as it deploys.

What is NEVER removed:
  - the plugin's current version dir (read from crates/<name>/plugin.json);
  - any version dir held by a live session (`.in_use/<pid>` for a live pid);
  - any version dir referenced by an absolute path in settings.json (a
    hardcoded pin the registry repoint never reaches — this is exactly what
    broke on 2026-07-27: prune deleted ctxrot/0.5.18 and stuckguard/0.1.21
    while 8 hooks/statusLine in ~/.claude/settings.json still pointed at
    them, verbatim);
  - any dir whose hold status could not be determined (this now also covers
    settings.json existing but failing to parse — see
    plugin_cache.settings_pinned_versions).

That last one is the point: deletion is the irreversible action here, so
"cannot tell" keeps the directory. The gate (check-plugin-rollout.py) resolves
the same uncertainty the other way and reports it, so an undetermined dir is
loud rather than silently skipped — see scripts/plugin_cache.py.

Entries in the cache that are NOT version dirs:
  - a DANGLING symlink (its target PROVABLY does not exist: ENOENT, ENOTDIR,
    or an ELOOP cycle that resolves to nothing by construction) is removed. It
    addresses nothing and holds no bytes, so unlinking it cannot lose anything.
    Measured 2026-09-08: condukt/0.4.2 had pointed at a long-pruned 0.6.0 since
    2026-07-02, and every prune run before this reported "0 stale dir(s)"
    while it sat there — the scan skipped every non-dir entry without a word;
  - a symlink that could not be RESOLVED (EACCES on a path component, an IO
    error) is REPORTED and left in place. "I was not allowed to look" is not
    "the target is gone", and the first version of this code conflated them
    via os.path.exists() — which answers False for both — and unlinked a
    pointer to a live directory with exit 0. See plugin_cache._link_resolution;
  - anything else (a plain file, a link to a file, a non-plugin entry at
    <cache>/<name>) is REPORTED and left in place. Unaccounted state must be
    loud, but it must not be deleted on a guess.

Exit codes:
  0 — pruned cleanly (or nothing to prune), no undetermined state
  1 — at least one dir could not be removed, or the cache could not be scanned
"""
import argparse
import os
import shutil
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import plugin_cache  # noqa: E402


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--repo", default=os.getcwd(), help="repo root (default: cwd)")
    ap.add_argument("--cache", default=None, help="plugin cache root")
    ap.add_argument("--dry-run", action="store_true", help="print, delete nothing")
    args = ap.parse_args(argv)

    cache_root = args.cache or plugin_cache.default_cache_root()
    crates = os.path.join(args.repo, "crates")

    current, src_problems = plugin_cache.source_versions(crates)
    pins, pins_undetermined = plugin_cache.settings_pinned_versions(cache_root)
    stale, scan_problems = plugin_cache.scan(
        cache_root, current, settings_pins=pins, settings_undetermined=pins_undetermined
    )
    problems = list(src_problems) + list(scan_problems)
    if pins_undetermined:
        problems.append(f"{pins_undetermined} — every cached dir is kept as potentially pinned")

    removable = [s for s in stale if s.removable]
    kept = [s for s in stale if not s.removable]

    freed = 0
    failed = []
    for s in removable:
        size = 0
        for dp, _dn, fn in os.walk(s.path):
            for f in fn:
                try:
                    size += os.path.getsize(os.path.join(dp, f))
                except OSError:
                    pass
        # Render BEFORE removing: describe() reads the filesystem (a dangling
        # link's target comes from readlink), so a description taken afterwards
        # degrades to "-> ?" and the log stops saying what was actually removed.
        desc = s.describe()
        if args.dry_run:
            print(f"[dry-run] would remove {desc}")
            freed += size
            continue
        try:
            # A dangling symlink is not a tree: rmtree would raise
            # NotADirectoryError and the entry would be reported as a failure
            # forever instead of being cleaned up.
            if s.kind == "dangling-link":
                os.unlink(s.path)
            else:
                shutil.rmtree(s.path)
        except OSError as exc:
            failed.append(f"{desc}: {exc}")
            continue
        freed += size
        print(f"pruned {desc}")

    verb = "would free" if args.dry_run else "freed"
    print(
        f"--- prune: {len(removable)} stale dir(s) "
        f"{'listed' if args.dry_run else 'removed'}, {len(kept)} kept "
        f"(in use or undetermined), {verb} {freed / 1e6:.1f} MB"
    )
    for s in kept:
        print(f"kept {s.describe()}")
    for p in problems + failed:
        print(f"PROBLEM {p}", file=sys.stderr)

    # An undetermined hold is kept by the pruner but must not read as success:
    # something in the cache could not be inspected.
    undetermined = [s for s in kept if s.holders.undetermined]
    if failed or problems or undetermined:
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
