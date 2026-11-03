"""Fake Blender for the sitecraft-capabilities spec tests (no real Blender).

Invoked by a per-test `/bin/sh` wrapper (`exec <python3> -B fake_blender.py "$@"`)
that the test selects through `SITECRAFT_BLENDER` (or puts on `PATH` as
`blender`). Behaviour, in order:

1. `FAKE_BLENDER_LOG` set   -> append one JSON line `{"argv": [...], "cwd": "..."}`
   (argv excludes the program itself).
2. `FAKE_BLENDER_MARKER` set -> create that file (proves "blender was spawned").
3. `FAKE_BLENDER_SLEEP` set -> sleep that many seconds (timeout tests).
4. `FAKE_BLENDER_EXIT` set  -> exit with that code (nothing else runs).
5. Otherwise find `--python <script>` (missing -> exit 3) and run the script
   like Blender does: `sys.argv` is `["blender", *argv]` (so the script reads
   its own arguments after `--`), and `import bpy` resolves to the fake in
   `fake_bpy/` next to this file. An exception in the script -> exit 1;
   otherwise exit 0 (or the script's own `sys.exit` code).
"""

import json
import os
import runpy
import sys
import time
import traceback

argv = sys.argv[1:]

log = os.environ.get("FAKE_BLENDER_LOG")
if log:
    with open(log, "a", encoding="utf-8") as f:
        f.write(json.dumps({"argv": argv, "cwd": os.getcwd()}) + "\n")

marker = os.environ.get("FAKE_BLENDER_MARKER")
if marker:
    open(marker, "a", encoding="utf-8").close()

sleep = os.environ.get("FAKE_BLENDER_SLEEP")
if sleep:
    time.sleep(float(sleep))

code = os.environ.get("FAKE_BLENDER_EXIT")
if code:
    sys.exit(int(code))

if "--python" not in argv or argv.index("--python") + 1 >= len(argv):
    sys.stderr.write("fake blender: no --python <script> in argv %r\n" % (argv,))
    sys.exit(3)

script = argv[argv.index("--python") + 1]
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "fake_bpy"))
sys.argv = ["blender"] + argv
try:
    runpy.run_path(script, run_name="__main__")
except SystemExit:
    raise
except BaseException:  # noqa: BLE001 - report every script failure as exit 1
    traceback.print_exc()
    sys.exit(1)
sys.exit(0)
