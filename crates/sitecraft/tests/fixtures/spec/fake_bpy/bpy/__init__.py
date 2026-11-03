"""Fake `bpy` for the sitecraft-capabilities spec tests (loaded by fake_blender.py).

Every attribute path (`bpy.ops.wm.read_factory_settings`, `bpy.data.objects`,
`bpy.context.scene.render.filepath = ...`, ...) exists; calling one records it
and returns `{'FINISHED'}`. Attribute assignment works like on a plain object.
Two operators have real effects:

* `bpy.ops.export_scene.gltf(filepath=P, ...)` writes a GLB to `P` (with
  `.glb` appended when `P` has no `.glb` suffix and `export_format` is absent
  or `"GLB"`, as Blender's exporter does). What it writes is chosen by
  `FAKE_BPY_GLB`: `valid` (default; a well-formed glTF 2 binary whose header
  length equals the file size), `empty` (0 bytes), `bad-magic`, or `none`
  (writes nothing).
* `bpy.ops.render.render(write_still=True, ...)` copies the file named by
  `FAKE_BPY_PNG` (if set) to `bpy.context.scene.render.filepath`.

`FAKE_BPY_LOG` set -> one JSON line per call `{"op": "bpy.ops....", "kwargs": {...}}`.
"""

import json
import os
import shutil
import struct


def _log(op, kwargs):
    p = os.environ.get("FAKE_BPY_LOG")
    if p:
        safe = {k: (v if isinstance(v, (str, int, float, bool)) or v is None else repr(v)) for k, v in kwargs.items()}
        with open(p, "a", encoding="utf-8") as f:
            f.write(json.dumps({"op": op, "kwargs": safe}) + "\n")


class _Any:
    def __init__(self, path):
        object.__setattr__(self, "_path", path)

    def __getattr__(self, name):
        if name.startswith("__"):
            raise AttributeError(name)
        v = _Any(self._path + "." + name)
        object.__setattr__(self, name, v)
        return v

    def __call__(self, *args, **kwargs):
        _log(self._path, kwargs)
        return {"FINISHED"}

    def __getitem__(self, key):
        return _Any("%s[%r]" % (self._path, key))

    def __iter__(self):
        return iter(())

    def __len__(self):
        return 0

    def __bool__(self):
        return True


def _glb_bytes():
    body = json.dumps({"asset": {"version": "2.0"}, "scenes": [{"nodes": []}], "scene": 0}).encode()
    body += b" " * ((4 - len(body) % 4) % 4)
    total = 12 + 8 + len(body)
    return b"glTF" + struct.pack("<II", 2, total) + struct.pack("<II", len(body), 0x4E4F534A) + body


def _gltf(filepath="", export_format="GLB", **kwargs):
    kw = dict(kwargs, filepath=filepath, export_format=export_format)
    _log("bpy.ops.export_scene.gltf", kw)
    path = filepath
    if export_format in (None, "GLB") and not path.lower().endswith(".glb"):
        path = path + ".glb"
    mode = os.environ.get("FAKE_BPY_GLB", "valid")
    if mode == "none":
        return {"FINISHED"}
    d = os.path.dirname(path)
    if d:
        os.makedirs(d, exist_ok=True)
    with open(path, "wb") as f:
        if mode == "valid":
            f.write(_glb_bytes())
        elif mode == "bad-magic":
            f.write(b"gLTX" + _glb_bytes()[4:])
        # "empty": zero bytes
    return {"FINISHED"}


def _render(**kwargs):
    _log("bpy.ops.render.render", kwargs)
    src = os.environ.get("FAKE_BPY_PNG")
    dst = context.scene.render.filepath
    if src and isinstance(dst, str) and dst:
        shutil.copyfile(src, dst)
    return {"FINISHED"}


ops = _Any("bpy.ops")
data = _Any("bpy.data")
context = _Any("bpy.context")
types = _Any("bpy.types")
props = _Any("bpy.props")
utils = _Any("bpy.utils")
app = _Any("bpy.app")
app.version = (4, 2, 0)
app.version_string = "4.2.0 (fake)"
app.background = True
ops.export_scene.gltf = _gltf
ops.render.render = _render
context.scene.render.filepath = ""
