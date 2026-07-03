#!/usr/bin/env bash
# Clean-rebuild every workspace binary and refresh the *installed* plugin cache
# for the host platform, so source changes actually take effect at runtime.
#
# Each plugin ships a per-platform binary (<name>-<os>-<arch>) that a bin/<name>
# launcher execs. After editing crate source you must rebuild and swap the
# installed copy under the plugin cache — recompiling the repo alone changes
# nothing the running harness sees. This script does both.
#
# Steps:
#   1. cargo clean         (skip with --no-clean)
#   2. cargo build --release --workspace --bins
#   3. copy target/release/<name> over every matching <name>-<os>-<arch> in the
#      live plugin cache   (and, with --stage-repo, the committed crates/*/bin/)
#   4. copy each plugin's hooks/ manifest config (e.g. hooks.json — NOT compiled
#      hook binaries, which step 3 already covers) from crates/<name>/hooks/ into
#      the matching live cache hooks/ dir, so hook config edits also take effect.
#
# Only the HOST platform's binaries are touched. macOS binaries must be built on
# a Mac (see scripts/build-plugin-bin.sh for the cross/single-crate staging tool).
#
# Usage:
#   scripts/rebuild-plugins.sh                  # clean + release build + refresh cache
#   scripts/rebuild-plugins.sh --no-clean       # incremental build (skip cargo clean)
#   scripts/rebuild-plugins.sh --stage-repo     # ALSO overwrite committed crates/*/bin
#   scripts/rebuild-plugins.sh --dry-run        # show what would change; build nothing, copy nothing
#   CLAUDE_PLUGIN_CACHE=/path scripts/rebuild-plugins.sh   # override plugin cache root
#
# Env:
#   CLAUDE_PLUGIN_CACHE   plugin cache root (default: ~/.claude/plugins/cache/yukineko)
set -euo pipefail
cd "$(dirname "$0")/.."
REPO="$PWD"

clean=1 stage_repo=0 dry=0
for arg in "$@"; do
  case "$arg" in
    --no-clean)   clean=0 ;;
    --stage-repo) stage_repo=1 ;;
    --dry-run)    dry=1 ;;
    -h|--help)    sed -n '2,30p' "$0"; exit 0 ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done

CACHE="${CLAUDE_PLUGIN_CACHE:-$HOME/.claude/plugins/cache/yukineko}"
# Ask cargo where it actually puts artifacts rather than assuming
# $REPO/target/release — CARGO_TARGET_DIR or a target-dir override in
# .cargo/config.toml (e.g. redirecting off a full C: drive under WSL) changes
# this without changing where cargo build itself writes.
TARGET_DIR="$(cargo metadata --no-deps --format-version=1 | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')"
REL="${TARGET_DIR:-$REPO/target}/release"

# host <os>-<arch>, matching the launcher's `uname` dispatch and build-plugin-bin.sh
triple="$(rustc -vV | sed -n 's/^host: //p')"
case "$triple" in
  *apple-darwin*) os=darwin ;;
  *linux*)        os=linux ;;
  *windows*)      os=windows ;;
  *)              os=unknown ;;
esac
case "$triple" in
  x86_64-*)  arch=x86_64 ;;
  aarch64-*) arch=arm64 ;;
  *)         arch=unknown ;;
esac
SUF="$os-$arch"

echo "repo:        $REPO"
echo "build dir:   $REL"
echo "cache:       $CACHE"
echo "host target: $triple  ->  $SUF"
echo "clean:       $([ $clean = 1 ] && echo yes || echo 'no (incremental)')   stage-repo: $([ $stage_repo = 1 ] && echo yes || echo no)   dry-run: $([ $dry = 1 ] && echo yes || echo no)"
echo

if [ ! -d "$CACHE" ]; then
  echo "plugin cache not found: $CACHE" >&2
  echo "set CLAUDE_PLUGIN_CACHE to the correct root and retry." >&2
  exit 1
fi

# --- build -----------------------------------------------------------------
if [ $dry = 1 ]; then
  echo "[dry-run] would run:$([ $clean = 1 ] && echo ' cargo clean;') cargo build --release --workspace --bins"
else
  if [ $clean = 1 ]; then
    echo ">>> cargo clean"
    cargo clean
  fi
  echo ">>> cargo build --release --workspace --bins"
  cargo build --release --workspace --bins
fi
echo

# --- plugin name -> crate dir lookup (for hooks/ manifest sync below) ------
# The cache plugin dirname (from plugin.json's "name") does not always match
# the crates/ directory name (e.g. crates/run-book/ ships plugin "runbook"),
# so resolve via plugin.json rather than assuming they're equal.
plugin_names=() plugin_dirs=()
shopt -s nullglob
for pj in "$REPO"/crates/*/.claude-plugin/plugin.json; do
  pname=$(sed -n 's/.*"name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$pj" | head -1)
  [ -n "$pname" ] || continue
  plugin_names+=("$pname")
  plugin_dirs+=("$(dirname "$(dirname "$pj")")")
done
shopt -u nullglob

cratedir_for() {
  local want="$1" i
  for i in "${!plugin_names[@]}"; do
    if [ "${plugin_names[$i]}" = "$want" ]; then
      echo "${plugin_dirs[$i]}"
      return 0
    fi
  done
  return 1
}

# --- refresh ---------------------------------------------------------------
updated_cache=0 updated_repo=0 updated_hooks=0 missing="" checked=0
shopt -s nullglob
for binfile in "$CACHE"/*/*/bin/*-"$SUF"; do
  checked=$((checked+1))
  base=$(basename "$binfile")     # e.g. condukt-<os>-<arch>
  binname=${base%-$SUF}           # e.g. condukt
  src="$REL/$binname"

  # 0) hooks/ manifest config (e.g. hooks.json) — repo -> live cache. Only the
  #    JSON manifest, not the compiled hook binary (handled below). Runs
  #    regardless of whether the release binary was built this run, since it
  #    doesn't depend on the build step.
  version_dir="$(dirname "$(dirname "$binfile")")"   # $CACHE/<plugin-name>/<version>
  plugin_name="$(basename "$(dirname "$version_dir")")"
  if cratedir="$(cratedir_for "$plugin_name")" && [ -d "$cratedir/hooks" ]; then
    cache_hooks_dir="$version_dir/hooks"
    for cfg in "$cratedir"/hooks/*.json; do
      cfgname=$(basename "$cfg")
      target="$cache_hooks_dir/$cfgname"
      if [ ! -f "$target" ] || ! cmp -s "$cfg" "$target"; then
        if [ $dry = 1 ]; then
          echo "hooks  would update $plugin_name/hooks/$cfgname"
        else
          mkdir -p "$cache_hooks_dir"
          cp -f "$cfg" "$target"
          echo "hooks  updated $plugin_name/hooks/$cfgname"
        fi
        updated_hooks=$((updated_hooks+1))
      fi
    done
  fi

  if [ ! -x "$src" ]; then
    missing="$missing $binname"
    continue
  fi
  # 1) live cache copy — what the running harness actually execs
  if ! cmp -s "$src" "$binfile"; then
    if [ $dry = 1 ]; then
      echo "cache  would update $base"
    else
      cp -f "$src" "$binfile"; chmod +x "$binfile"
      echo "cache  updated $base"
    fi
    updated_cache=$((updated_cache+1))
  fi
  # 2) committed repo copy — what /plugin install ships (opt-in via --stage-repo)
  if [ $stage_repo = 1 ]; then
    repofile=$(ls "$REPO"/crates/*/bin/"$base" 2>/dev/null | head -n1 || true)
    if [ -n "$repofile" ] && ! cmp -s "$src" "$repofile"; then
      if [ $dry = 1 ]; then
        echo "repo   would update ${repofile#$REPO/}"
      else
        cp -f "$src" "$repofile"; chmod +x "$repofile"
        echo "repo   updated ${repofile#$REPO/}"
      fi
      updated_repo=$((updated_repo+1))
    fi
  fi
done
shopt -u nullglob

echo "---"
echo "cache bins scanned: $checked | cache updated: $updated_cache$([ $stage_repo = 1 ] && echo " | repo bin updated: $updated_repo") | hooks config updated: $updated_hooks"
if [ -n "$missing" ]; then
  echo "WARNING: no release artifact for:$missing" >&2
  echo "(these cache plugins had a $SUF binary but no matching target/release/<name> — a non-workspace or renamed bin?)" >&2
fi
[ $checked = 0 ] && echo "note: no *-$SUF binaries found under $CACHE (wrong cache root, or no host-platform plugins installed)."
exit 0
