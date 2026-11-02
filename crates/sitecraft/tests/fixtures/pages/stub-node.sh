#!/bin/sh
# Stand-in for `node` in crates/sitecraft/tests/check_pages.rs (via SITECRAFT_NODE).
# It ignores the script it is given and replays a fixed result:
#   STUB_RECORD_DIR  - if set, record cwd / argv / whether argv[1] exists, and touch `spawned`
#   STUB_STDERR      - text written to stderr
#   STUB_STDOUT_FILE - fixture file copied verbatim to stdout (unset = empty stdout)
#   STUB_EXIT        - exit status (default 0)
if [ -n "$STUB_RECORD_DIR" ]; then
  mkdir -p "$STUB_RECORD_DIR"
  pwd -P > "$STUB_RECORD_DIR/cwd"
  printf '%s' "$#" > "$STUB_RECORD_DIR/argc"
  printf '%s' "$1" > "$STUB_RECORD_DIR/script"
  if [ -f "$1" ]; then printf 'yes' > "$STUB_RECORD_DIR/script_exists"; fi
  printf '%s' "$2" > "$STUB_RECORD_DIR/input"
  : > "$STUB_RECORD_DIR/spawned"
fi
if [ -n "$STUB_STDERR" ]; then printf '%s\n' "$STUB_STDERR" >&2; fi
if [ -n "$STUB_STDOUT_FILE" ]; then cat "$STUB_STDOUT_FILE"; fi
exit "${STUB_EXIT:-0}"
