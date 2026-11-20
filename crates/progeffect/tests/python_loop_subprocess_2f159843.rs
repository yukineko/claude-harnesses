//! Backlog 2f159843 (596ef57d R-python-unknown-sink): the asyncio event-loop
//! methods `subprocess_shell` / `subprocess_exec` run a program and have no
//! effect model, so a call of either name (any receiver) is Undetermined.
//! A mention inside a string literal or a comment is not a call and stays
//! Known with no effect.

use progeffect::python::analyze;
use progeffect::Analysis;

#[test]
fn loop_subprocess_calls_are_undetermined() {
    for src in [
        "import asyncio\nasyncio.get_event_loop().subprocess_shell(None, \"rm -rf /usr/lib\")",
        "import asyncio\nloop = asyncio.new_event_loop()\nloop.subprocess_exec(None, \"rm\", \"-rf\", \"/usr/lib\")",
        "import asyncio\nloop = asyncio.new_event_loop()\nloop.subprocess_exec(None, \"ls\")",
        "loop.subprocess_shell(None, \"ls\")",
        "x.subprocess_exec(None, \"ls\")",
    ] {
        assert!(
            matches!(analyze(src, &[]), Analysis::Undetermined(_)),
            "{src}: {:?}",
            analyze(src, &[])
        );
    }
}

#[test]
fn string_and_comment_mentions_stay_known_empty() {
    for src in [
        "print(\"loop.subprocess_shell\")",
        "print(\"loop.subprocess_exec(None, 'ls')\")",
        "# loop.subprocess_shell(None, \"ls\")\nprint(1)",
    ] {
        assert!(
            matches!(analyze(src, &[]), Analysis::Known(ref e) if e.is_empty()),
            "{src}: {:?}",
            analyze(src, &[])
        );
    }
}
