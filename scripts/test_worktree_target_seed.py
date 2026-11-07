#!/usr/bin/env python3
"""Warm-seed a new worktree's `target/` by APFS clonefile (backlog BACKLOG_ID_PENDING).

Spec (independent RED-test writer; the implementer must not edit the asserts
to make them pass — see CLAUDE.md §2 (a)/(b)):

When a worktree is created for work, its `target/` is seeded from a warm
existing target by APFS clonefile (`cp -c -R <warm>/target <new>/target`), and
the mtimes of tracked files whose CONTENT is identical in the warm and the new
worktree are synced from the warm side, so cargo reuses the clone instead of
cold-building. Measured before this spec: 59s cold, ~25s seeded without mtime
sync, ~1s seeded with mtime sync.

SAFETY PROPERTY (the reason this file exists): a file whose content differs
between warm and new must keep its fresh mtime, so cargo rebuilds it. Syncing a
changed file's mtime would make cargo treat changed source as fresh and run
tests against STALE code (fail-open). If the seed cannot be determined — no
warm target, clone fails, blob compare fails, not APFS, new/target already
present — the new worktree is left UNSEEDED (no `target/`, no mtime touched at
all). Never a partial target, never a partial mtime sync.

──────────────────────────────────────────────────────────────────────────────
INTERFACE THE IMPLEMENTER MUST PROVIDE
──────────────────────────────────────────────────────────────────────────────
1. `scripts/seed-worktree-target.py WARM_ROOT NEW_ROOT`
     WARM_ROOT  top-level dir of an existing checkout (main tree or worktree)
                that holds the warm `target/`.
     NEW_ROOT   top-level dir of the freshly created worktree.
   Behaviour:
     * exit 0 whenever it either seeded or cleanly declined to seed (a declined
       seed is not an error: the caller just cold-builds).
     * the LAST line of stdout is one JSON object:
         {"seeded": true|false, "reason": "<text>", ...}
       `seeded` is true only if NEW_ROOT/target was fully cloned AND the mtime
       sync completed.
     * on `seeded: false`, NEW_ROOT/target does not exist (or, if it existed
       before the call, is byte-for-byte untouched) and no file under NEW_ROOT
       had its mtime changed.
     * mtime sync applies ONLY to paths in `git ls-files` of NEW_ROOT whose
       working-tree content hash equals the warm working-tree content hash of
       the same path (compare actual file content, not HEAD blobs, so an
       uncommitted edit in WARM is treated as a difference). The synced mtime
       is the WARM file's mtime. Untracked and ignored files are never touched.
2. `scripts/session-worktree-init.py` (SessionStart hook; prints "A session
   worktree is ready at") seeds the session worktree it creates using the main
   tree (`git rev-parse --show-toplevel` of the starting cwd) as WARM_ROOT.

Targeted creation points: (1) the shared seeding script, (2) the SessionStart
creator. condukt's `crates/condukt/src/worktree.rs::create` is a second
creation point that this file does NOT cover (it is Rust, works in arbitrary
repos where `scripts/` is absent, and needs its own surface).

Run (open defects un-skipped):
    AUDIT_RUN_OPEN=1 python3 scripts/test_worktree_target_seed.py
"""
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
SEEDER = SCRIPTS / "seed-worktree-target.py"
HOOK = SCRIPTS / "session-worktree-init.py"
BACKLOG_ID = "BACKLOG_ID_PENDING"


def open_defect(backlog_id):
    return unittest.skipUnless(
        os.environ.get("AUDIT_RUN_OPEN"),
        f"backlog {backlog_id}: open defect, remove skip when fixed",
    )


def _env(**extra):
    env = dict(os.environ)
    for k in ("CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR", "RUSTC_WRAPPER",
              "CARGO_BUILD_RUSTC_WRAPPER", "CARGO_INCREMENTAL"):
        env.pop(k, None)
    cargo_bin = Path.home() / ".cargo" / "bin"
    env["PATH"] = f"{cargo_bin}{os.pathsep}{env.get('PATH', '')}"
    env.update(
        GIT_CONFIG_GLOBAL=os.devnull,
        GIT_CONFIG_NOSYSTEM="1",
        GIT_AUTHOR_NAME="t",
        GIT_AUTHOR_EMAIL="t@example.invalid",
        GIT_COMMITTER_NAME="t",
        GIT_COMMITTER_EMAIL="t@example.invalid",
    )
    env.update(extra)
    return env


def _git(cwd, *args):
    r = subprocess.run(["git", *args], cwd=cwd, env=_env(),
                       capture_output=True, text=True)
    if r.returncode != 0:
        raise AssertionError(f"git {args} failed: {r.stderr}")
    return r.stdout.strip()


def _cargo_build(root):
    """Return (compiling_crate_names, stderr)."""
    r = subprocess.run(["cargo", "build", "--offline"], cwd=root, env=_env(),
                       capture_output=True, text=True, timeout=600)
    if r.returncode != 0:
        raise AssertionError(f"cargo build failed in {root}:\n{r.stderr}")
    compiling = [ln.split()[1] for ln in r.stderr.splitlines()
                 if ln.strip().startswith("Compiling ")]
    return compiling, r.stderr


def _run_bin(root):
    r = subprocess.run([str(Path(root) / "target" / "debug" / "app")],
                       capture_output=True, text=True, timeout=60)
    return r.stdout.strip()


def _seed(warm, new):
    r = subprocess.run([sys.executable, str(SEEDER), str(warm), str(new)],
                       env=_env(), capture_output=True, text=True, timeout=600)
    last = (r.stdout.strip().splitlines() or ["<no stdout>"])[-1]
    try:
        verdict = json.loads(last)
    except ValueError:
        verdict = None
    return r, verdict


def _mtimes(root, rels):
    return {p: os.stat(Path(root) / p).st_mtime_ns for p in rels}


APP_MAIN = 'fn main() {{ println!("{}", lib1::word()); }}\n'
LIB1 = 'pub fn word() -> &\'static str {{ "{}" }}\n'


class Fixture:
    """A two-crate cargo workspace in a git repo, built once (the warm tree)."""

    def __init__(self, tc):
        self.tmp = tempfile.TemporaryDirectory()
        tc.addCleanup(self._cleanup)
        self.base = Path(os.path.realpath(self.tmp.name))
        self.warm = self.base / "repo"
        self.warm.mkdir()
        w = self.warm
        _git(w, "init", "-q", "-b", "main")
        (w / "Cargo.toml").write_text(
            '[workspace]\nmembers = ["app", "lib1"]\nresolver = "2"\n')
        for c in ("app", "lib1"):
            (w / c / "src").mkdir(parents=True)
        (w / "app" / "Cargo.toml").write_text(
            '[package]\nname = "app"\nversion = "0.1.0"\nedition = "2021"\n'
            '[dependencies]\nlib1 = { path = "../lib1" }\n')
        (w / "lib1" / "Cargo.toml").write_text(
            '[package]\nname = "lib1"\nversion = "0.1.0"\nedition = "2021"\n')
        (w / "app" / "src" / "main.rs").write_text(APP_MAIN.format("{}"))
        (w / "lib1" / "src" / "lib.rs").write_text(LIB1.format("warm"))
        (w / ".gitignore").write_text("/target\n*.secret\n")
        _git(w, "add", "-A")
        _git(w, "commit", "-q", "-m", "seed")
        _cargo_build(w)  # generates Cargo.lock (untracked) + warm target
        assert (w / "target" / "debug" / "app").exists()
        # Make freshly checked-out files deterministically newer than the
        # warm dep-info, so an unsynced seed is observably stale.
        time.sleep(1.2)

    def _cleanup(self):
        for p in self.base.rglob("*"):
            try:
                os.chmod(p, 0o755)
            except OSError:
                pass
        self.tmp.cleanup()

    def add_worktree(self, name, rev="HEAD", branch=None):
        path = self.base / "wts" / name
        path.parent.mkdir(exist_ok=True)
        _git(self.warm, "worktree", "add", "-q", "-b", branch or name,
             str(path), rev)
        return path

    def commit_change(self, rel, content, branch):
        """Commit `content` at `rel` on a new branch, without touching warm."""
        tmpwt = self.add_worktree(f"edit-{branch}", branch=branch)
        (tmpwt / rel).write_text(content)
        _git(tmpwt, "commit", "-q", "-am", "change")
        _git(self.warm, "worktree", "remove", "--force", str(tmpwt))
        return branch


# ─────────────────────────────── RED: seed works ─────────────────────────────

class SeedReuse(unittest.TestCase):
    @open_defect(BACKLOG_ID)
    def test_same_commit_seed_builds_with_zero_compiling(self):
        f = Fixture(self)
        new = f.add_worktree("same")
        # Lockfile is untracked in this fixture; carry it so resolution is equal.
        shutil.copy2(f.warm / "Cargo.lock", new / "Cargo.lock")
        r, v = _seed(f.warm, new)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIsNotNone(v, f"last stdout line is not JSON: {r.stdout!r}")
        self.assertTrue(v.get("seeded"), v)
        self.assertTrue((new / "target" / "debug" / "app").exists())
        compiling, err = _cargo_build(new)
        self.assertEqual(compiling, [],
                         f"seeded worktree rebuilt workspace crates:\n{err}")
        self.assertEqual(_run_bin(new), "warm")

    @open_defect(BACKLOG_ID)
    def test_identical_tracked_files_get_the_warm_mtime(self):
        f = Fixture(self)
        new = f.add_worktree("mt")
        rel = "lib1/src/lib.rs"
        r, v = _seed(f.warm, new)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertTrue(v and v.get("seeded"), v)
        self.assertEqual(os.stat(new / rel).st_mtime_ns,
                         os.stat(f.warm / rel).st_mtime_ns)

    @open_defect(BACKLOG_ID)
    def test_session_worktree_hook_seeds_from_main_tree(self):
        f = Fixture(self)
        home = f.base / "home"
        home.mkdir()
        out = subprocess.run(
            (sys.executable, str(HOOK)),
            input=json.dumps({"cwd": str(f.warm), "session_id": "seedab12-0"}),
            capture_output=True, text=True, env=_env(HOME=str(home)), timeout=600,
        )
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertIn("A session worktree is ready at", out.stdout)
        wt = f.base / ".repo-worktrees" / "session-seedab12"
        self.assertTrue(wt.is_dir(), out.stdout)
        self.assertTrue((wt / "target" / "debug" / "app").exists(),
                        f"session worktree has no seeded target:\n{out.stdout}")
        shutil.copy2(f.warm / "Cargo.lock", wt / "Cargo.lock")
        compiling, err = _cargo_build(wt)
        self.assertEqual(compiling, [], err)


# ──────────────────────────── SAFETY controls ────────────────────────────────

class SeedSafety(unittest.TestCase):
    @open_defect(BACKLOG_ID)
    def test_changed_committed_source_is_rebuilt_and_binary_is_new(self):
        f = Fixture(self)
        f.commit_change("lib1/src/lib.rs", LIB1.format("changed"), "chg")
        new = f.add_worktree("chgwt", rev="chg", branch="chgwt")
        shutil.copy2(f.warm / "Cargo.lock", new / "Cargo.lock")
        before = os.stat(new / "lib1/src/lib.rs").st_mtime_ns
        r, v = _seed(f.warm, new)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertTrue(v and v.get("seeded"), v)
        # The differing file keeps its fresh mtime ...
        self.assertEqual(os.stat(new / "lib1/src/lib.rs").st_mtime_ns, before)
        compiling, err = _cargo_build(new)
        self.assertIn("lib1", compiling, err)
        # ... and the observable behaviour is the NEW code, not the warm one.
        self.assertEqual(_run_bin(new), "changed")

    @open_defect(BACKLOG_ID)
    def test_uncommitted_warm_edit_counts_as_a_difference(self):
        f = Fixture(self)
        # Warm working tree diverges from HEAD (edited, rebuilt); new worktree
        # is HEAD. Content differs -> no sync -> rebuild -> HEAD behaviour.
        (f.warm / "lib1/src/lib.rs").write_text(LIB1.format("dirtywarm"))
        _cargo_build(f.warm)
        time.sleep(1.2)
        new = f.add_worktree("dirty")
        shutil.copy2(f.warm / "Cargo.lock", new / "Cargo.lock")
        before = os.stat(new / "lib1/src/lib.rs").st_mtime_ns
        r, v = _seed(f.warm, new)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertTrue(v and v.get("seeded"), v)
        self.assertEqual(os.stat(new / "lib1/src/lib.rs").st_mtime_ns, before)
        compiling, err = _cargo_build(new)
        self.assertIn("lib1", compiling, err)
        self.assertEqual(_run_bin(new), "warm")

    @open_defect(BACKLOG_ID)
    def test_untracked_and_ignored_files_are_not_mtime_touched(self):
        f = Fixture(self)
        new = f.add_worktree("untr")
        for root in (f.warm, new):
            (root / "notes.txt").write_text("same\n")      # untracked
            (root / "key.secret").write_text("same\n")     # ignored
        rels = ["notes.txt", "key.secret"]
        before = _mtimes(new, rels)
        r, v = _seed(f.warm, new)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertTrue(v and v.get("seeded"), v)
        self.assertEqual(_mtimes(new, rels), before)


# ─────────────────────── fail-closed: unseeded, untouched ────────────────────

TRACKED = ["Cargo.toml", "app/Cargo.toml", "lib1/Cargo.toml",
           "app/src/main.rs", "lib1/src/lib.rs", ".gitignore"]


class SeedDeclines(unittest.TestCase):
    def _assert_declined_untouched(self, r, v, new, before):
        self.assertEqual(r.returncode, 0,
                         f"a declined seed must not be an error: {r.stderr}")
        self.assertIsNotNone(v, f"last stdout line is not JSON: {r.stdout!r}")
        self.assertIs(v.get("seeded"), False, v)
        self.assertEqual(_mtimes(new, TRACKED), before,
                         "declined seed touched mtimes")

    @open_defect(BACKLOG_ID)
    def test_no_warm_target_leaves_worktree_unseeded(self):
        f = Fixture(self)
        shutil.rmtree(f.warm / "target")
        new = f.add_worktree("cold")
        before = _mtimes(new, TRACKED)
        r, v = _seed(f.warm, new)
        self._assert_declined_untouched(r, v, new, before)
        self.assertFalse((new / "target").exists())

    @open_defect(BACKLOG_ID)
    def test_clone_failure_leaves_no_partial_target_and_no_mtime_sync(self):
        f = Fixture(self)
        bad = f.warm / "target" / "debug" / "unreadable"
        bad.write_text("x")
        os.chmod(bad, 0)
        new = f.add_worktree("clonefail")
        before = _mtimes(new, TRACKED)
        r, v = _seed(f.warm, new)
        self._assert_declined_untouched(r, v, new, before)
        self.assertFalse((new / "target").exists(),
                         "partial target left behind after a failed clone")

    @open_defect(BACKLOG_ID)
    def test_warm_not_a_git_checkout_cannot_compare_so_declines(self):
        f = Fixture(self)
        plain = f.base / "plain"
        plain.mkdir()
        shutil.copytree(f.warm / "target", plain / "target", symlinks=True)
        for rel in TRACKED:
            (plain / rel).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(f.warm / rel, plain / rel)
        new = f.add_worktree("nogit")
        before = _mtimes(new, TRACKED)
        r, v = _seed(plain, new)
        self._assert_declined_untouched(r, v, new, before)
        self.assertFalse((new / "target").exists())

    @open_defect(BACKLOG_ID)
    def test_existing_new_target_is_left_untouched(self):
        f = Fixture(self)
        new = f.add_worktree("hastarget")
        (new / "target").mkdir()
        (new / "target" / "mine").write_text("keep\n")
        before = _mtimes(new, TRACKED)
        r, v = _seed(f.warm, new)
        self._assert_declined_untouched(r, v, new, before)
        self.assertEqual(sorted(p.name for p in (new / "target").iterdir()),
                         ["mine"])
        self.assertEqual((new / "target" / "mine").read_text(), "keep\n")


# ────────── existing behaviour (passes at HEAD; must keep passing) ───────────

class HookWithoutWarmTarget(unittest.TestCase):
    def test_hook_still_creates_worktree_when_main_has_no_target(self):
        f = Fixture(self)
        shutil.rmtree(f.warm / "target")
        home = f.base / "home"
        home.mkdir()
        out = subprocess.run(
            (sys.executable, str(HOOK)),
            input=json.dumps({"cwd": str(f.warm), "session_id": "coldab12-0"}),
            capture_output=True, text=True, env=_env(HOME=str(home)), timeout=600,
        )
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertIn("A session worktree is ready at", out.stdout)
        wt = f.base / ".repo-worktrees" / "session-coldab12"
        self.assertTrue(wt.is_dir(), out.stdout)
        self.assertFalse((wt / "target").exists())


if __name__ == "__main__":
    unittest.main()
