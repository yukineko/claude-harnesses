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
#   3. copy target/release/<name> over the matching <name>-<os>-<arch> in the
#      plugin's CURRENT version dir of the live plugin cache (and, with
#      --stage-repo, the committed crates/*/bin/)
#   4. copy each plugin's hooks/ manifest config (e.g. hooks.json — NOT compiled
#      hook binaries, which step 3 already covers) from crates/<name>/hooks/ into
#      the CURRENT version dir's hooks/, so hook config edits also take effect.
#
# CURRENT VERSION ONLY — SUPERSEDED VERSION DIRS ARE FROZEN (backlog 8acb117a)
#   "Current" is the version in the repo's crates/<dir>/.claude-plugin/plugin.json
#   (plugin name -> crate dir resolved via plugin.json "name", since the two can
#   differ, e.g. run-book -> runbook). Every other cache/<plugin>/<version>/ dir is
#   left byte-for-byte untouched: no binary, no hooks config, no
#   .deployed-from.json. This refresh used to glob "$CACHE"/*/*/bin/* and overwrite
#   EVERY version dir, so a session pinned to an old version silently ran new code
#   and a canary rollback (re-pointing the registry at the prior dir) restored
#   nothing. Accepted consequence: a running session keeps executing the code of
#   the version dir it started with until it is restarted.
#   A plugin whose current version cannot be determined (no crate with that
#   plugin.json "name", or no readable "version") is NOT refreshed at all — never
#   "refresh every dir" as a fallback — and the run exits non-zero (CLAUDE.md §3).
#   Pinned by scripts/test_rebuild_preserves_superseded_versions.py and
#   scripts/test_rebuild_current_version_only.py.
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
#   Manual/standalone calls (no --only) refresh every installed plugin — in its
#   CURRENT version dir only (see "CURRENT VERSION ONLY" above).
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
    -h|--help)    sed -n '2,64p' "$0"; exit 0 ;;
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

# --- honour the rollout lock (backlog ea39fbc9, lock from e3366b5c) ---------
# rollout-plugins.sh holds "$CACHE/.rollout.lock" (holder pid in ./pid) across
# read-registry -> copy -> repoint -> prune. A standalone rebuild used to ignore
# it and swap cache binaries in the middle of another session's rollout. Now:
#   - run BY the holding rollout: accepted only if HARNESS_ROLLOUT_LOCK_HOLDER_PID
#     equals the pid recorded in the lock AND that pid is an ancestor of this
#     process. A bare env var is not a key (see rollout-plugins.sh).
#   - run standalone: take the lock exactly as rollout's acquire_rollout_lock
#     does (mkdir; a held lock is never stolen, not even when its holder pid
#     looks dead), so a rollout cannot start mid-rebuild either.
# Every "cannot tell" (no readable pid, ps failure, mkdir failure) refuses, and
# all of it happens BEFORE cargo is invoked or the cache is written.
# --dry-run builds and copies nothing, so like rollout --dry-run it neither
# takes nor needs the lock.
ROLLOUT_LOCK_DIR="$CACHE/.rollout.lock"
rebuild_lock_held=0
# 0 = $1 is an ancestor of this process, 1 = it is not, 2 = cannot tell.
pid_is_ancestor() {
  local target="$1" p="$PPID" next i=0
  while [ "$i" -lt 256 ]; do
    [ "$p" = "$target" ] && return 0
    [ "$p" -le 1 ] && return 1
    # ps's own stderr is left visible: a failed lookup refuses (return 2) and
    # the operator should see why.
    if ! next="$(ps -o ppid= -p "$p")"; then return 2; fi
    next="${next//[[:space:]]/}"
    [[ "$next" =~ ^[0-9]+$ ]] || return 2
    p="$next"; i=$((i + 1))
  done
  return 2
}
rollout_lock_state() {
  local holder=""
  holder="$(cat "$ROLLOUT_LOCK_DIR/pid" 2>/dev/null || true)"
  if [ -z "$holder" ] || ! [[ "$holder" =~ ^[0-9]+$ ]]; then
    echo "holder pid unknown (no readable $ROLLOUT_LOCK_DIR/pid)"
  elif kill -0 "$holder" 2>/dev/null; then
    echo "held by live pid $holder"
  elif ps -p "$holder" >/dev/null 2>&1; then
    echo "held by live pid $holder (another user)"
  else
    echo "STALE? holder pid $holder is not running"
  fi
}
refuse_rollout_lock() {
  echo "ERROR: rebuild: $1" >&2
  echo "       Refusing to proceed: nothing was built and nothing in the cache was touched." >&2
  echo "       A rebuild during a rollout swaps binaries between its copy and its registry" >&2
  echo "       repoint (backlog ea39fbc9 / e3366b5c). If you have confirmed no rollout is" >&2
  echo "       running, remove the lock by hand:" >&2
  echo "         rm -r \"$ROLLOUT_LOCK_DIR\"" >&2
  exit 6
}
release_rebuild_lock() {
  if [ "$rebuild_lock_held" = 1 ]; then
    local owner=""
    owner="$(cat "$ROLLOUT_LOCK_DIR/pid" 2>/dev/null || true)"
    if [ "$owner" = "$$" ]; then
      rm -r "$ROLLOUT_LOCK_DIR" || echo "rebuild: could not remove $ROLLOUT_LOCK_DIR — remove it by hand: rm -r \"$ROLLOUT_LOCK_DIR\"" >&2
    else
      echo "rebuild: $ROLLOUT_LOCK_DIR no longer names this process (pid $$, file says '${owner}') — left in place" >&2
    fi
    rebuild_lock_held=0
  fi
}
if [ "$dry" = 0 ]; then
  if [ ! -d "$CACHE" ]; then
    echo "plugin cache not found: $CACHE" >&2
    echo "set CLAUDE_PLUGIN_CACHE to the correct root and retry." >&2
    exit 1
  fi
  if [ -n "${HARNESS_ROLLOUT_LOCK_HOLDER_PID:-}" ]; then
    lock_owner="$(cat "$ROLLOUT_LOCK_DIR/pid" 2>/dev/null || true)"
    if [ ! -d "$ROLLOUT_LOCK_DIR" ]; then
      refuse_rollout_lock "HARNESS_ROLLOUT_LOCK_HOLDER_PID=$HARNESS_ROLLOUT_LOCK_HOLDER_PID is set but $ROLLOUT_LOCK_DIR does not exist."
    fi
    if [ "$lock_owner" != "$HARNESS_ROLLOUT_LOCK_HOLDER_PID" ]; then
      refuse_rollout_lock "$ROLLOUT_LOCK_DIR is $(rollout_lock_state), not by HARNESS_ROLLOUT_LOCK_HOLDER_PID=$HARNESS_ROLLOUT_LOCK_HOLDER_PID."
    fi
    anc=0; pid_is_ancestor "$lock_owner" || anc=$?
    case "$anc" in
      0) echo "rollout lock: running under holder pid $lock_owner (ancestor of this process)" ;;
      1) refuse_rollout_lock "$ROLLOUT_LOCK_DIR is held by pid $lock_owner, which is not an ancestor of this process." ;;
      *) refuse_rollout_lock "could not determine whether lock holder pid $lock_owner is an ancestor of this process (ps failed)." ;;
    esac
  elif mkdir "$ROLLOUT_LOCK_DIR" 2>/dev/null; then
    rebuild_lock_held=1
    trap release_rebuild_lock EXIT
    trap 'exit 130' INT
    trap 'exit 143' TERM
    if ! echo "$$" > "$ROLLOUT_LOCK_DIR/pid"; then
      refuse_rollout_lock "took $ROLLOUT_LOCK_DIR but could not record the pid in it."
    fi
  elif [ -d "$ROLLOUT_LOCK_DIR" ]; then
    refuse_rollout_lock "a rollout holds $ROLLOUT_LOCK_DIR — $(rollout_lock_state)."
  else
    refuse_rollout_lock "could not create $ROLLOUT_LOCK_DIR, so cannot tell whether a rollout is running."
  fi
fi

# Ask cargo where it actually puts artifacts rather than assuming
# $REPO/target/release — CARGO_TARGET_DIR or a target-dir override in
# .cargo/config.toml (e.g. redirecting off a full C: drive under WSL) changes
# this without changing where cargo build itself writes.
#
# cargo metadata is the ONLY authority for the build dir; there is no
# "$REPO/target" fallback (backlog a0525604). Two ways to not get an answer:
#   - cargo metadata exits non-zero: `set -o pipefail` aborts the script on
#     this assignment (pinned by BacklogA0525604CargoMetadataFailureIsNotMasked).
#   - cargo metadata exits 0 but yields no "target_directory" (empty or
#     unparsable output): TARGET_DIR is empty. This used to fall back to
#     $REPO/target/release, which on a machine whose .cargo/config.toml redirects
#     target-dir does not hold this build's artifacts — every plugin then took
#     the 'missing' branch and the run still exited 0 with a green summary.
#     "Could not determine the build dir" is not "nothing to refresh"
#     (CLAUDE.md §3), so it aborts here, before anything is built or copied.
#     Pinned by BacklogA0525604EmptyMetadataIsNotGreen in
#     scripts/test_backlog_audit_b1_0.py.
TARGET_DIR="$(cargo metadata --no-deps --format-version=1 | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')"
if [ -z "$TARGET_DIR" ]; then
  echo "ERROR: cargo metadata succeeded but reported no \"target_directory\"." >&2
  echo "  Cannot determine where cargo writes release artifacts, so nothing can be" >&2
  echo "  refreshed. Refusing to guess \$REPO/target (wrong whenever CARGO_TARGET_DIR" >&2
  echo "  or .cargo/config.toml redirects target-dir)." >&2
  echo "  Check: cargo metadata --no-deps --format-version=1" >&2
  exit 1
fi
REL="$TARGET_DIR/release"

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
# cargo/rustc append .exe to every build output on Windows; the deployed cache
# filename and the launcher's own `binary-$os-$arch$ext` lookup follow suit
# (see crates/*/bin/<name>). Without this, every "$REL/$binname" / cache-glob
# below silently misses on Windows: [ -x ] happens to auto-resolve .exe via
# the MSYS access() shim, but cp/ls/basename globbing do not, so builds
# "succeed" while zero bytes actually land in the cache (measured 2026-08-04).
EXT=""
[ "$os" = windows ] && EXT=".exe"

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
plugin_names=() plugin_dirs=() plugin_versions=()
shopt -s nullglob
for pj in "$REPO"/crates/*/.claude-plugin/plugin.json; do
  pname=$(sed -n 's/.*"name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$pj" | head -1)
  [ -n "$pname" ] || continue
  plugin_names+=("$pname")
  plugin_dirs+=("$(dirname "$(dirname "$pj")")")
  # May be empty (no/unreadable "version"); current_version_for reports that
  # as undetermined rather than letting it match anything.
  plugin_versions+=("$(sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$pj" | head -1)")
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

# The plugin's CURRENT version = the repo plugin.json "version" of the crate whose
# plugin.json "name" is $1. Prints it and returns 0; returns 1 (prints nothing)
# when there is no such crate or its version is empty — "cannot tell which dir is
# current", which callers must treat as undetermined, never as "every dir".
current_version_for() {
  local want="$1" i
  for i in "${!plugin_names[@]}"; do
    if [ "${plugin_names[$i]}" = "$want" ]; then
      [ -n "${plugin_versions[$i]}" ] || return 1
      echo "${plugin_versions[$i]}"
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
# Superseded version dirs deliberately left untouched (see "CURRENT VERSION ONLY"
# in the header), and plugins whose current version could not be determined.
# The latter makes the run exit non-zero — see the tail.
frozen_superseded=0 undetermined_current=""
# In-scope current-version bins (main loop) and fresh-dir launchers (seed pass)
# for which a release artifact WAS found. Zero, with `missing` non-empty, on a
# real (non --dry-run) run means the build deployed nothing — see the tail.
found_src=0
# Launchers in a FRESH current-version dir that this run could not seed a host
# binary for, as "<plugin>/<version>:<launcher>". Separate from `missing` (which
# the main refresh loop fills for dirs that ALREADY had a host binary) because
# the two states have different severities: `missing` is a warning about a
# possibly-renamed non-workspace bin (fatal only when NOTHING was found — see
# `found_src`), whereas an unseeded fresh dir is a plugin
# that is installed, version-consistent and execs NOTHING. Non-empty makes this
# script exit non-zero — see the tail.
seed_missing=""
# Release artifacts that were NOT observed to be stripped, as "<bin>(<reason>)".
# None of them is copied anywhere; non-empty makes this script exit non-zero.
unstripped=""

# assert_stripped <artifact> — exit 0 ONLY when the artifact was OBSERVED to be
# stripped of symbols and debug info; otherwise print the reason and return 1.
#
# Why verify instead of trusting `[profile.release] strip = true`: that profile
# key is a request, not an observation. CARGO_PROFILE_RELEASE_STRIP=false in the
# environment overrides it with no diagnostic, and on macOS rustc only WARNS
# when the external `strip` tool fails (e.g. the Xcode-license exit 69) — in
# both cases `cargo build` exits 0 and the artifact keeps its full symbol table.
# Copying without looking deployed exactly that, with exit 0 (CLAUDE.md §3).
#
# Fail closed: a missing/failing inspection tool, an unrecognised format or an
# unsupported host OS (no verifier is implemented for Windows/PE) is "cannot
# determine", and resolves to "not stripped" — the artifact is not deployed.
# Pinned by scripts/tests/rebuild-ships-stripped-binaries.sh.
assert_stripped() {
  local f="$1" out extra
  case "$os" in
    darwin)
      # Mach-O. `nm -a` lists every symbol-table entry including stabs (the
      # debug map, type "-"). A stripped executable keeps only undefined
      # imports ("U") and `__mh_execute_header`; any other entry is a local or
      # debug symbol strip would have removed. nm's exit status is part of the
      # verdict: a failed nm is not an empty symbol table. stderr is kept in
      # `out`: on failure it is the reason; on success any stray diagnostic
      # line counts as an unexplained entry below, i.e. it refuses.
      if ! out="$(nm -a "$f" 2>&1)"; then
        echo "nm -a failed (cannot inspect the symbol table): $(printf '%s' "$out" | head -n 1)"
        return 1
      fi
      extra="$(printf '%s\n' "$out" | awk 'NF && !($1=="U" || $2=="U") && $NF!="__mh_execute_header"' | wc -l | tr -d ' ')"
      if [ "$extra" != 0 ]; then
        echo "$extra non-import symbol/debug entries remain"
        return 1
      fi
      ;;
    linux)
      # ELF. `file` reports ", stripped" / ", not stripped" and "with debug_info".
      if ! out="$(file -b "$f" 2>&1)"; then
        echo "file failed (cannot inspect the binary): $(printf '%s' "$out" | head -n 1)"
        return 1
      fi
      case "$out" in
        *"not stripped"*|*"with debug_info"*) echo "file: $out"; return 1 ;;
        *", stripped"*) ;;
        *) echo "cannot determine strip state from file: $out"; return 1 ;;
      esac
      ;;
    *)
      echo "no strip verifier for host os '$os'"
      return 1
      ;;
  esac
  return 0
}
# check_src_stripped <artifact> <binname> — memoised wrapper; records refusals.
stripped_ok=" " stripped_bad=" "
check_src_stripped() {
  local reason
  case "$stripped_ok" in *" $2 "*) return 0 ;; esac
  case "$stripped_bad" in *" $2 "*) return 1 ;; esac
  if reason="$(assert_stripped "$1")"; then
    stripped_ok="$stripped_ok$2 "
    return 0
  fi
  stripped_bad="$stripped_bad$2 "
  unstripped="$unstripped $2($reason)"
  echo "REFUSED $2: release artifact is not stripped: $reason" >&2
  return 1
}

shopt -s nullglob
for binfile in "$CACHE"/*/*/bin/*-"$SUF$EXT"; do
  checked=$((checked+1))
  base=$(basename "$binfile")     # e.g. condukt-<os>-<arch>[.exe]
  binname=${base%-$SUF$EXT}       # e.g. condukt
  src="$REL/$binname$EXT"

  version_dir="$(dirname "$(dirname "$binfile")")"   # $CACHE/<plugin-name>/<version>
  plugin_name="$(basename "$(dirname "$version_dir")")"
  if ! in_only "$plugin_name"; then
    skipped_filter=$((skipped_filter+1))
    continue
  fi

  # Only the plugin's CURRENT version dir is written (header: "CURRENT VERSION
  # ONLY"). Everything below — hooks config, binary, provenance, --stage-repo —
  # sits behind this guard, so a superseded dir is never touched.
  if ! cur_ver="$(current_version_for "$plugin_name")"; then
    case " $undetermined_current " in
      *" $plugin_name "*) ;;
      *) undetermined_current="$undetermined_current $plugin_name" ;;
    esac
    continue
  fi
  if [ "$(basename "$version_dir")" != "$cur_ver" ]; then
    frozen_superseded=$((frozen_superseded+1))
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
  found_src=$((found_src+1))
  # 0b) never deploy an artifact not observed to be stripped (neither the cache
  #     copy nor the --stage-repo copy below). See assert_stripped.
  if ! check_src_stripped "$src" "$binname"; then
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
    # Resolve WITHOUT `ls`. This loop runs under `shopt -s nullglob`, so a glob
    # that matches nothing DISAPPEARS — and the previous
    #
    #     repofile=$(ls "$REPO"/crates/*/bin/"$base" 2>/dev/null | head -n1 || true)
    #
    # then left `ls` with zero arguments, which makes it list $PWD (the repo
    # root) and SUCCEED. `head -n1` returned the first entry in C-locale order,
    # `CLAUDE.md`, and the `cp -f "$src" "$repofile"` below overwrote the repo's
    # CLAUDE.md with a plugin ELF — 38 times in one run on 2026-08-21, because
    # only 2 of 39 plugins actually carry a staged crates/<name>/bin/<base>.
    # `2>/dev/null` hid nothing (ls succeeded) and `|| true` was inert.
    # "No staged copy exists" is a resolution FAILURE and must stay empty so the
    # `[ -n "$repofile" ]` guard skips the copy; it must never be rewritten into
    # a plausible-looking destination (CLAUDE.md §3). Pinned by
    # scripts/tests/rebuild-stage-repo-destination.sh.
    repofile=""
    for cand in "$REPO"/crates/*/bin/"$base"; do
      repofile="$cand"
      break
    done
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
  ver="${plugin_versions[$i]}"   # same "current" as the main refresh loop
  [ -n "$ver" ] || continue
  bindir="$CACHE/$pname/$ver/bin"
  [ -d "$bindir" ] || continue          # current version not rolled out to cache yet
  # launcher = a bin/ entry with no -<os>-<arch> platform suffix. A plugin may
  # ship SEVERAL: specguard ships `specguard` (the hook) and `specforge` (the
  # source-side spec loop, specs/spec-loop.toml R4/R5). Each needs its OWN host
  # binary, so every launcher is seeded — this loop used to take the first one
  # in glob order and `break`, which silently left the rest exec'ing a binary
  # that was never placed. That is not a degraded deploy but a DARK one:
  # installed, version-consistent, running nothing, and emitting no finding to
  # notice it by (CLAUDE.md §3 — "could not run" must never look like
  # "ran and found nothing"). With two launchers the `break` also picked
  # `specforge` over `specguard`, i.e. it darkened the GATE hook itself.
  # Pinned by scripts/tests/rebuild-seeds-every-launcher.sh.
  for f in "$bindir"/*; do
    binname=$(basename "$f")
    case "$binname" in
      *-darwin-arm64|*-darwin-x86_64|*-linux-x86_64|*-linux-arm64|*-windows-x86_64.exe|*-windows-arm64.exe)
        continue ;;
    esac
    hostbin="$bindir/$binname-$SUF$EXT"
    # `-e` is NOT the predicate the consumer applies. The launcher requires the
    # file to be EXECUTABLE and non-empty (crates/tdd/bin/tdd:52-55 — `if [ -x
    # "$binary" ]; then exec ...`), so a host bin sitting at mode 0644 or
    # truncated to zero bytes was accepted here as "already handled" while the
    # launcher refused it: dark, not red. The main refresh loop does not save
    # that case either — it only copies when `cmp -s` says the bytes DIFFER, so
    # a non-executable file whose bytes already match is chmod'd by nobody and
    # no code path ever restores the exec bit. Skipping now demands both.
    # Pinned by scripts/tests/rebuild-seed-skip-is-silent.sh (case D).
    if [ -x "$hostbin" ] && [ -s "$hostbin" ]; then
      continue                          # host bin present AND runnable
    fi
    src="$REL/$binname$EXT"
    # No artifact to seed from. This used to be a bare silent `continue`, and
    # that silence — not the skip — was the fail-open: this pass is the ONLY
    # path that populates a freshly rolled-out version dir (see the comment
    # block above), so a launcher it declines to seed execs a binary that does
    # not exist, and the run still printed a clean summary and exited 0. The
    # `missing` WARNING below could not cover it: that accumulator is fed by the
    # main refresh loop, which globs *existing* `*-$SUF` files and therefore
    # never visits a fresh dir at all. Record it and fail loudly at the end,
    # mirroring the identical `[ ! -x "$src" ]` condition the main loop already
    # reports (see its `missing="$missing $binname"` branch above).
    # CLAUDE.md §3: "could not seed" must not resolve to the same output as
    # "nothing needed seeding".
    if [ ! -x "$src" ]; then
      seed_missing="$seed_missing $pname/$ver:$binname"
      continue
    fi
    checked=$((checked+1))
    found_src=$((found_src+1))
    if ! check_src_stripped "$src" "$binname"; then
      continue
    fi
    if [ $dry = 1 ]; then
      echo "cache  would seed $binname-$SUF$EXT (fresh version dir $pname/$ver)"
    else
      cp -f "$src" "$hostbin"; chmod +x "$hostbin"
      write_provenance "$CACHE/$pname/$ver" "$pname"
      echo "cache  seeded $binname-$SUF$EXT (fresh version dir $pname/$ver)"
    fi
    updated_cache=$((updated_cache+1))
  done
done
shopt -u nullglob

echo "---"
[ -n "$only_filter" ] && echo "only:        $only_filter (skipped $skipped_filter bin(s) outside this set)"
echo "cache bins scanned: $checked | cache updated: $updated_cache$([ $stage_repo = 1 ] && echo " | repo bin updated: $updated_repo") | hooks config updated: $updated_hooks"
echo "superseded version dir bin(s) left frozen (not current per repo plugin.json): $frozen_superseded"
if [ -n "$missing" ]; then
  echo "WARNING: no release artifact for:$missing" >&2
  echo "(these cache plugins had a $SUF binary but no matching target/release/<name> — a non-workspace or renamed bin?)" >&2
fi
[ $checked = 0 ] && echo "note: no *-$SUF binaries found under $CACHE (wrong cache root, or no host-platform plugins installed)."

# A fresh version dir this run left holding only its launcher is a DARK deploy:
# the plugin is installed and version-consistent, its hooks fire, and the
# launcher then execs a binary that is not there — so no finding is emitted to
# notice it by. That is the state the seed pass exists to prevent, and exiting 0
# after failing to prevent it is exactly CLAUDE.md §3's forbidden collapse of
# "could not check" into "checked and fine".
#
# Non-zero is deliberate rather than a warning-only line. scripts/rollout-
# plugins.sh calls this script under `set -euo pipefail` (its run_rebuild_and_
# sync), so a non-zero exit ABORTS the rollout with an error instead of letting
# it continue to prune superseded dirs and print "done." over a fleet that
# execs nothing. It also does not depend on anyone reading stderr, which a
# warning does.
#
# Pinned by scripts/tests/rebuild-seed-skip-is-silent.sh (cases B and D).
rc=0
# A cached plugin whose current version cannot be determined was refreshed in NO
# version dir. Writing all of them instead is exactly the defect this guard
# removed (backlog 8acb117a), and staying silent would let "could not decide
# which dir is current" read as "nothing needed refreshing" (CLAUDE.md §3).
if [ -n "$undetermined_current" ]; then
  echo "ERROR: cannot determine the CURRENT version for cached plugin(s):$undetermined_current" >&2
  echo "  No crates/*/.claude-plugin/plugin.json has that \"name\" with a readable" >&2
  echo "  \"version\", so no version dir of these plugins was refreshed (superseded" >&2
  echo "  dirs are frozen, and the current one is unknown). Fix the plugin.json, or" >&2
  echo "  record a removed plugin in scripts/retired-plugins.json and prune its cache." >&2
  rc=1
fi
# Every in-scope plugin took the main loop's `missing` branch and nothing was
# found to seed either: the build produced NOT ONE artifact this refresh needs,
# so nothing was deployed. A partial miss stays the warning above (a renamed or
# non-workspace bin), but "zero of N" is not that — it is a build dir that does
# not hold this build's output (the empty-metadata half of backlog a0525604
# produced exactly this) or a build that wrote nothing, and exiting 0 would
# report "deployed nothing" as a green run (CLAUDE.md §3). --dry-run is exempt:
# it builds nothing, so absent artifacts say nothing about the real run; it
# still prints the WARNING. Pinned by
# scripts/test_rebuild_zero_artifacts_not_green.py.
if [ $dry = 0 ] && [ -n "$missing" ] && [ "$found_src" = 0 ]; then
  echo "ERROR: no release artifact found for ANY in-scope plugin (missing:$missing)." >&2
  echo "  Nothing was deployed. Looked in: $REL" >&2
  echo "  Check that cargo metadata's target_directory is where cargo build wrote." >&2
  rc=1
fi
if [ -n "$seed_missing" ]; then
  echo "ERROR: could not seed a host binary into a FRESH version dir for:$seed_missing" >&2
  echo "  Each entry is <plugin>/<version>:<launcher>. No $REL/<launcher>$EXT was" >&2
  echo "  built this run, so that version dir holds only its launcher and the" >&2
  echo "  plugin execs NOTHING while looking correctly deployed (dark, not red)." >&2
  echo "  Check that the launcher name matches a workspace bin target, and that" >&2
  echo "  the release build actually wrote to: $REL" >&2
  rc=1
fi
# An artifact not observed to be stripped was deployed NOWHERE. Exiting 0 would
# report "refused to ship it" the same as "shipped everything" (CLAUDE.md §3);
# non-zero also aborts rollout-plugins.sh (set -e) before it prunes or reports
# done. Pinned by scripts/tests/rebuild-ships-stripped-binaries.sh.
if [ -n "$unstripped" ]; then
  echo "ERROR: refused to deploy release artifact(s) not observed to be stripped:$unstripped" >&2
  echo "  [profile.release] strip = true in the workspace Cargo.toml requests it, but" >&2
  echo "  CARGO_PROFILE_RELEASE_STRIP in the environment, or a failing external" >&2
  echo "  \`strip\` (rustc only warns), produces an unstripped binary with exit 0." >&2
  echo "  Rebuild without the override / fix the strip tool, then rerun." >&2
  rc=1
fi
exit "$rc"
