#!/bin/bash
# Runs the 18fe626f per-version prune hold suites (stdlib unittest; no pytest needed).
set -eu
cd "$(dirname "$0")/../.."
python3 -m unittest scripts.test_prune_version_history scripts.test_prune_version_history_extras
