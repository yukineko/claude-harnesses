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
  - any dir whose hold status could not be determined.

That last one is the point: deletion is the irreversible action here, so
"cannot tell" keeps the directory. The gate (check-plugin-rollout.py) resolves
the same uncertainty the other way and reports it, so an undetermined dir is
loud rather than silently skipped — see scripts/plugin_cache.py.

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
    stale, scan_problems = plugin_cache.scan(cache_root, current)
    problems = list(src_problems) + list(scan_problems)

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
        if args.dry_run:
            print(f"[dry-run] would remove {s.describe()}")
            freed += size
            continue
        try:
            shutil.rmtree(s.path)
        except OSError as exc:
            failed.append(f"{s.describe()}: {exc}")
            continue
        freed += size
        print(f"pruned {s.describe()}")

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
