#!/usr/bin/env python3
"""backlog 9d1f3885: check-fail-open flags jev client.rs:137 as an
`undetermined-arm-empty-fallback`, a false positive.

The arm does not erase the undetermined value into an empty one: it FORWARDS
`why` into `self.give_up(...)`. The block-arm rule reads the multi-line call's
`None` positional argument as the block's empty tail value. Measured at triage
(main e1aca620): `python3 scripts/check-fail-open.py --all` still prints
`crates/jev/src/client.rs:137: [undetermined-arm-empty-fallback] (advisory)`.

Fixture below is the shape of crates/jev/src/client.rs:133-146 at e1aca620.
A control asserts the rule still flags a genuinely empty arm, so the expected
"no hit" is not satisfied by a rule that flags nothing.

RED repro: python3 -m pytest --runxfail scripts/test_backlog_9d1f3885.py
"""
from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_SPEC = importlib.util.spec_from_file_location(
    "check_fail_open", _HERE / "check-fail-open.py"
)
fo = importlib.util.module_from_spec(_SPEC)
exec(  # noqa: S102 -- same source-text load as test_check_fail_open.py (05726f9f)
    compile(_SPEC.loader.get_source(_SPEC.name), _SPEC.origin, "exec", dont_inherit=True),
    fo.__dict__,
)

RULE = "undetermined-arm-empty-fallback"

FORWARDING = """\
fn call(&self) -> Outcome {
    let RawHttp { status, body } = match http.require() {
        Required::Determined(h) => h,
        Required::Blocked(why) => {
            return self.give_up(
                req,
                started,
                None,
                "undetermined",
                redact::redact(why.as_str(), secret),
            )
        }
    };
    status
}
"""

EMPTY = """\
fn call(&self) -> Option<u8> {
    let h = match http.require() {
        Required::Determined(h) => h,
        Required::Blocked(_why) => {
            return None
        }
    };
    Some(h)
}
"""


def hits(src: str) -> list[tuple[int, str, str]]:
    return [h for h in fo.scan_rust(src.splitlines(keepends=True)) if h[2] == RULE]


class JevClient137(unittest.TestCase):
    def test_control_genuinely_empty_arm_is_flagged(self) -> None:
        self.assertTrue(hits(EMPTY), "control: the rule must flag `=> { return None }`")

    @unittest.expectedFailure
    def test_forwarding_why_into_a_multiline_call_is_not_flagged(self) -> None:
        self.assertEqual(
            hits(FORWARDING),
            [],
            "an arm forwarding `why` into give_up(..., None, ...) is not an empty fallback",
        )


if __name__ == "__main__":
    unittest.main()
