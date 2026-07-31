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
#   scripts/rebuild-plugins.sh --only=a,b       # restrict the CACHE REFRESH (copy step) to
#                                                # these plugin names; still builds the whole
#                                                # workspace, but only swaps binaries/hooks for
#                                                # the listed plugins into the live cache
#   CLAUDE_PLUGIN_CACHE=/path scripts/rebuild-plugins.sh   # override plugin cache root
#
# Env:
#   CLAUDE_PLUGIN_CACHE   plugin cache root (default: ~/.claude/plugins/cache/yukineko)
#
# WHY --only EXISTS (Problem-2.3 gap fix)
#   `cargo build --workspace` (below) always rebuilds every workspace binary,
#   and without --only the refresh loop swaps ANY changed binary into the live
#   cache. rollout-plugins.sh calls this script as its rebuild step even for a
#   single-plugin `--plugin backlog` rollout — so, without scoping, a routine
#   non-gate rollout could silently swap in fresh GATE_CRATE binaries (e.g.
#   blastguard/overwatch/propguard/stuckguard) that never went through their
#   required `--canary` staged health-gate. rollout-plugins.sh always passes
#   --only=<the exact plugin set it is rolling out THIS invocation> so a
#   rollout can only ever touch the cache for plugins it actually targeted.
#   Manual/standalone calls (no --only) keep the historic behavior: refresh
#   every installed plugin's binary.
set -euo pipefail
cd "$(dirname "$0")/.."
REPO="$PWD"

clean=1 stage_repo=0 dry=0 only_filter=""
for arg in "$@"; do
  case "$arg" in
    --no-clean)   clean=0 ;;
    --stage-repo) stage_repo=1 ;;
    --dry-run)    dry=1 ;;
    --only=*)     only_filter="${arg#--only=}" ;;
    -h|--help)    sed -n '2,40p' "$0"; exit 0 ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done

# Empty only_filter (default) = no restriction (historic behavior). Non-empty
# = comma-separated plugin names; only these have their cache binary/hooks
# swapped this run. No associative arrays (bash 3.2 / macOS default compat,
# matching rollout-plugins.sh's row_for_name comment).
in_only() {
  [ -z "$only_filter" ] && return 0
  case ",$only_filter," in
    *",$1,"*) return 0 ;;
    *) return 1 ;;
  esac
}

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
  else
    # --no-clean skips the reclaim above, and when .cargo/config.toml redirects
    # the target-dir to a fixed absolute path nothing else empties it either.
    # Apply the size cap BEFORE the build: cleaning after would
    # discard exactly the artifacts this build is about to produce.
    # No-op unless the cap is exceeded. See scripts/cap-target-dir.sh.
    scripts/cap-target-dir.sh
  fi
  echo ">>> cargo build --release --workspace --bins"
  cargo build --release --workspace --bins
fi
echo

# --- plugin name -> crate dir lookup (for hooks/ manifest sync below) ------
# The cache plugin dirname (from plugin.json's "name") does not always match
# the crates/ directory name, so resolve via plugin.json rather than assuming
# they're equal.
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

# Record WHICH SOURCE a deployed binary was built from, beside the binary.
#
# Why this exists: the registry records a plugin's VERSION, and
# rollout-plugins.sh only re-points that entry when the version CHANGES — but
# this script refreshes the binary whenever the bytes differ. So a plugin can
# sit at a matching version with a binary built from source that has since
# moved. The usual cause is harness-core: every plugin statically links it, and
# none of them bump for a change to it. Comparing the deployed binary's HASH
# cannot detect this either — release builds here are NOT byte-reproducible
# (measured 2026-07-26: `cargo clean -p X && cargo build` gives a different
# sha256 for identical source), so a hash comparison reports drift for every
# plugin, every time, and is therefore useless as a gate.
#
# What IS decidable is provenance. Record the commit, and let
# check-plugin-rollout.py ask git whether the plugin's source moved since.
# `dirty` is scoped to the paths that actually determine these bytes (the
# plugin's own crate plus the shared crate it links) so unrelated work in
# progress elsewhere in the tree does not make every rollout unverifiable.
write_provenance() {
  local vdir="$1" pname="$2" cdir commit dirty
  local -a paths=()
  cdir="$(cratedir_for "$pname" 2>/dev/null || true)"
  [ -n "$cdir" ] && paths+=("${cdir#"$REPO"/}")
  paths+=("crates/harness-core")

  # Both git queries are judgements, so the EXIT STATUS is part of the answer.
  # Reading only stdout would collapse "git failed" into an empty string, and an
  # empty `status --porcelain` means CLEAN — i.e. a broken git would certify the
  # tree as pristine. Capture the rc separately (note: `local x=$(cmd)` would
  # make $? the rc of `local`, not of the command, so the declaration is split
  # from the assignment) and resolve any failure to dirty=true.
  # `set -e` is active, and a failing command substitution in a plain assignment
  # aborts the script — so each query is taken as an `if` condition, which is
  # exempt from `set -e` and yields the rc without swallowing it.
  #
  # stderr is deliberately NOT sent to /dev/null: these two commands are silent
  # on success and only speak when something is wrong, which is exactly the case
  # whose diagnostic must survive. Discarding it would leave "git broke" and
  # "tree is clean" looking identical in the log.
  local status_out status_rc commit_rc
  if commit="$(git -C "$REPO" rev-parse HEAD)"; then commit_rc=0; else commit_rc=1; fi
  if status_out="$(git -C "$REPO" status --porcelain -- "${paths[@]}")"; then
    status_rc=0
  else
    status_rc=1
  fi

  if [ "$commit_rc" -ne 0 ] || [ -z "$commit" ]; then
    # No resolvable commit means nothing later can be compared against it.
    dirty=true
  elif [ "$status_rc" -ne 0 ]; then
    # Could not determine whether the determining paths are clean. Undetermined
    # is not clean.
    dirty=true
  elif [ -n "$status_out" ]; then
    dirty=true
  else
    dirty=false
  fi

  # The linked shared-crate version, recorded so the manifest STATES which
  # harness-core these bytes contain instead of leaving it to be re-derived from
  # the commit. Unreadable resolves to "unknown", never to a plausible number:
  # check-plugin-rollout.py treats "unknown" as a problem, and a fabricated
  # version would be worse than an absent one.
  #
  # Asserted as a VALUE by scripts/tests/provenance-core-version.sh (run it by
  # hand — scripts/tests/*.sh are not wired into a hook). The first version of
  # this line had a typo in the path; `bash -n` accepted it and every manifest
  # would have recorded "unknown", the fallback hiding a broken writer.
  local core_version
  core_version="$(awk '/^\[package\]/{p=1;next} /^\[/{p=0} p && $1=="version" {gsub(/"/,"",$3); print $3; exit}' \
    "$REPO/crates/harness-core/Cargo.toml" 2>/dev/null || true)"
  [ -n "$core_version" ] || core_version="unknown"

  printf '{"plugin":"%s","commit":"%s","dirty":%s,"harness_core_version":"%s","deployed_at":%s}\n' \
    "$pname" "$commit" "$dirty" "$core_version" "$(date +%s)" >"$vdir/.deployed-from.json"
}

# --- refresh ---------------------------------------------------------------
updated_cache=0 updated_repo=0 updated_hooks=0 missing="" checked=0 skipped_filter=0
shopt -s nullglob
for binfile in "$CACHE"/*/*/bin/*-"$SUF"; do
  checked=$((checked+1))
  base=$(basename "$binfile")     # e.g. condukt-<os>-<arch>
  binname=${base%-$SUF}           # e.g. condukt
  src="$REL/$binname"

  version_dir="$(dirname "$(dirname "$binfile")")"   # $CACHE/<plugin-name>/<version>
  plugin_name="$(basename "$(dirname "$version_dir")")"
  if ! in_only "$plugin_name"; then
    skipped_filter=$((skipped_filter+1))
    continue
  fi

  # 0) hooks/ manifest config (e.g. hooks.json) — repo -> live cache. Only the
  #    JSON manifest, not the compiled hook binary (handled below). Runs
  #    regardless of whether the release binary was built this run, since it
  #    doesn't depend on the build step.
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
  # Provenance describes the bytes deployed RIGHT NOW, so it is recorded whether
  # or not this run replaced them. Writing it only on replacement was wrong: a
  # binary that already matched the current build is equally current, but would
  # keep no manifest and so stay permanently unverifiable — the checker would
  # report drift forever and no rollout could ever clear it.
  [ $dry = 1 ] || write_provenance "$version_dir" "$plugin_name"
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

# --- seed host binary into a FRESH current-version dir ---------------------
# A version-bumped rollout (scripts/rollout-plugins.sh) rsyncs crates/<name>/
# into a brand-new cache/<plugin>/<newver>/ that ships only the launcher +
# committed cross-platform bins — never the per-host <name>-$SUF binary (built
# per host, not committed). The main loop above globs *existing* *-$SUF files, so
# it never touches such a fresh dir; the live wrapper then execs a missing binary
# and silently no-ops ("no bundled binary for $SUF") — the plugin looks deployed
# but runs nothing. Seed the freshly-built host binary into the plugin's CURRENT
# canonical-version dir when it is missing the host bin. Only the current version
# dir is targeted (not stale inactive ones — seeding those would put the current
# build under an old version number in a dir nothing execs).
shopt -s nullglob
for i in "${!plugin_names[@]}"; do
  pname="${plugin_names[$i]}"
  if ! in_only "$pname"; then
    continue
  fi
  pj="${plugin_dirs[$i]}/.claude-plugin/plugin.json"
  ver=$(sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$pj" | head -1)
  [ -n "$ver" ] || continue
  bindir="$CACHE/$pname/$ver/bin"
  [ -d "$bindir" ] || continue          # current version not rolled out to cache yet
  # bin name = the launcher (the bin/ entry with no -<os>-<arch> platform suffix)
  binname=""
  for f in "$bindir"/*; do
    b=$(basename "$f")
    case "$b" in
      *-darwin-arm64|*-darwin-x86_64|*-linux-x86_64|*-linux-arm64|*-windows-x86_64|*-windows-arm64)
        continue ;;
    esac
    binname="$b"; break
  done
  [ -n "$binname" ] || continue
  hostbin="$bindir/$binname-$SUF"
  [ -e "$hostbin" ] && continue         # host bin already present (main loop handled it)
  src="$REL/$binname"
  [ -x "$src" ] || continue             # only seed a plugin we actually built this run
  checked=$((checked+1))
  if [ $dry = 1 ]; then
    echo "cache  would seed $binname-$SUF (fresh version dir $pname/$ver)"
  else
    cp -f "$src" "$hostbin"; chmod +x "$hostbin"
    write_provenance "$CACHE/$pname/$ver" "$pname"
    echo "cache  seeded $binname-$SUF (fresh version dir $pname/$ver)"
  fi
  updated_cache=$((updated_cache+1))
done
shopt -u nullglob

echo "---"
[ -n "$only_filter" ] && echo "only:        $only_filter (skipped $skipped_filter bin(s) outside this set)"
echo "cache bins scanned: $checked | cache updated: $updated_cache$([ $stage_repo = 1 ] && echo " | repo bin updated: $updated_repo") | hooks config updated: $updated_hooks"
if [ -n "$missing" ]; then
  echo "WARNING: no release artifact for:$missing" >&2
  echo "(these cache plugins had a $SUF binary but no matching target/release/<name> — a non-workspace or renamed bin?)" >&2
fi
[ $checked = 0 ] && echo "note: no *-$SUF binaries found under $CACHE (wrong cache root, or no host-platform plugins installed)."
exit 0
