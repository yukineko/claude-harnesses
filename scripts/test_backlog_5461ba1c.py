"""Backlog 5461ba1c repro: an orphan cache dir (cache dir with no crate under
crates/) must not be answered with the rollout remedy, which cannot work for a
plugin that has no crate. Audit-written; see the backlog item.
"""
import tempfile
import unittest

from scripts import test_check_plugin_rollout as T


class OrphanCacheRemedy(T._FixtureCase):
    @unittest.skip("backlog 5461ba1c: open defect, remove skip when fixed")
    def test_orphan_cache_dir_is_not_told_to_rollout(self):
        with tempfile.TemporaryDirectory() as tmp:
            rc, _out, err = self.run_main(tmp, cached_versions={T.GHOST: ["0.1.0"]})
            self.assertEqual(rc, T.cpr.RC_ROLLOUT)  # control: it is a failure
            self.assertIn("cached but no current version known from crates/", err)
            self.assertNotIn(
                "scripts/rollout-plugins.sh --plugin", err,
                "the orphan-cache finding is folded into ROLLOUT DRIFT and the "
                "Fix line points at a rollout, which cannot work without a crate",
            )


if __name__ == "__main__":
    unittest.main()
