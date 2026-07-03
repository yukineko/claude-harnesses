#!/usr/bin/env bash
# Keep the installed plugin cache in step with this repo's plugin source.
#
# WHY THIS EXISTS
#   `crates/overwatch/` is the single source of truth: the marketplace ships it via
#   `git-subdir`, and `/plugin install` copies it to
#   `~/.claude/plugins/cache/<owner>/overwatch/<version>/` as a PLAIN COPY (no .git).
#   The running /overwatch skill loads its agents + SKILL.md from that cache. So when
#   you improve overwatch itself, it is tempting to edit the cache copy — but those
#   edits live outside git and silently diverge from the repo (exactly the drift
#   this script exists to prevent). Always edit the repo, then run this to refresh
#   your local install; never hand-edit the cache.
#
# USAGE
#   scripts/sync-plugin-assets.sh            # repo -> cache (refresh local install)
#   scripts/sync-plugin-assets.sh --check    # report drift, exit 1 if any (no writes)
#
# Only the text assets the skill loads (agents/, skills/, hooks/) are synced —
# binaries are produced by build-plugin-bin.sh, and src/tests are dev-only.
set -euo pipefail
cd "$(dirname "$0")/.."
root="$PWD"

# The directories that the running skill reads and that /overwatch can edit.
ASSET_DIRS=(agents skills hooks)

# --- locate the installed cache for this plugin -----------------------------
manifest="$root/.claude-plugin/plugin.json"
name=$(sed -n 's/.*"name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$manifest" | head -1)
version=$(sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$manifest" | head -1)
if [ -z "$name" ] || [ -z "$version" ]; then
  echo "sync-plugin-assets: could not read name/version from $manifest" >&2
  exit 2
fi

cache_root="${CLAUDE_PLUGIN_CACHE:-$HOME/.claude/plugins/cache}"
# Owner dir is the marketplace owner; glob it so we don't hardcode "yukineko".
shopt -s nullglob
matches=("$cache_root"/*/"$name"/"$version")
shopt -u nullglob
if [ "${#matches[@]}" -eq 0 ]; then
  echo "sync-plugin-assets: no installed cache for $name@$version under $cache_root" >&2
  echo "  (install the plugin first, or set CLAUDE_PLUGIN_CACHE)" >&2
  exit 2
fi
if [ "${#matches[@]}" -gt 1 ]; then
  echo "sync-plugin-assets: multiple caches matched; refusing to guess:" >&2
  printf '  %s\n' "${matches[@]}" >&2
  exit 2
fi
cache="${matches[0]}"

# --- check mode: report drift, change nothing -------------------------------
if [ "${1:-}" = "--check" ]; then
  drift=0
  for d in "${ASSET_DIRS[@]}"; do
    [ -d "$root/$d" ] || continue
    if ! diff -r "$root/$d" "$cache/$d" >/dev/null 2>&1; then
      echo "DRIFT in $d/ (repo vs cache):"
      diff -rq "$root/$d" "$cache/$d" 2>&1 | sed 's/^/  /'
      drift=1
    fi
  done
  if [ "$drift" -eq 0 ]; then
    echo "in sync: $cache"
  else
    echo "" >&2
    echo "cache has drifted from repo. Run 'scripts/sync-plugin-assets.sh' to fix" >&2
    echo "(repo is canonical — if a cache-only edit is real, port it into the repo first)." >&2
  fi
  exit "$drift"
fi

if [ "${1:-}" != "" ]; then
  echo "sync-plugin-assets: unknown argument '$1' (use --check or no args)" >&2
  exit 2
fi

# --- sync mode: mirror repo -> cache (one way) ------------------------------
for d in "${ASSET_DIRS[@]}"; do
  [ -d "$root/$d" ] || continue
  if command -v rsync >/dev/null 2>&1; then
    rsync -a --delete "$root/$d/" "$cache/$d/"
  else
    rm -rf "${cache:?}/$d"
    mkdir -p "$cache/$d"
    cp -R "$root/$d/." "$cache/$d/"
  fi
  echo "synced $d/ -> $cache/$d/"
done
echo "done: $cache is in step with the repo"
