//! Regression guard for the fail-closed Stop-gate panic barrier.
//!
//! Since `harness_core::gate::run_guarded` fails CLOSED on a panic (a crashed
//! gate body now emits a `decision:block` instead of exiting 0 = allow — see
//! `src/gate/run.rs`), the operator ESCAPE HATCHES must be evaluated *before*
//! any panic-prone verification. If the panic-prone `evaluate()` ran first and
//! panicked, control would never reach the skip-marker / disabled checks, and a
//! deterministically-crashing gate would be **unescapable** (it would block
//! every stop, and the operator's `.<gate>-skip` marker or `enabled = false`
//! toggle would be dead code behind the crash).
//!
//! The two hatches that MUST survive a panicking verifier are the `disabled`
//! toggles (`Config::disabled_env()` and the config `!cfg.enabled` check — a
//! persistent "turn this gate off") and the one-shot
//! `consume_skip(&root, ".<gate>-skip")` marker (an operator's "let this one stop
//! through NOW"). Both are panic-free and, in every gate today, run ahead of
//! `evaluate()`. This test pins that ordering so a future refactor that moves
//! verification (or any panic-prone logic) ahead of an escape is caught here
//! rather than shipping a gate that can crash into a block no operator can clear.
//!
//! (The `give-up` / `max_attempts` hatch legitimately sits *after* `evaluate()`;
//! it is not an escape from a *crash* — the crash case is bounded instead by the
//! `stop_hook_active` `BoundedAllow` in `run_guarded`, so it is intentionally
//! excluded from this ordering check.)

use std::path::PathBuf;

struct Gate {
    /// Crate dir under `crates/`.
    crate_name: &'static str,
    /// The Stop-hook body function whose ordering we pin.
    body_fn: &'static str,
    /// The operator skip-marker filename this gate consumes.
    skip_marker: &'static str,
}

const GATES: &[Gate] = &[
    Gate {
        crate_name: "donegate",
        body_fn: "fn gate_run(",
        skip_marker: ".donegate-skip",
    },
    Gate {
        crate_name: "reviewgate",
        body_fn: "fn review_run(",
        skip_marker: ".reviewgate-skip",
    },
    Gate {
        crate_name: "tdd",
        body_fn: "fn gate_run(",
        skip_marker: ".tdd-skip",
    },
    Gate {
        crate_name: "propguard",
        body_fn: "fn check_run(",
        skip_marker: ".propguard-skip",
    },
];

/// The Stop-hook body's source text: from its `fn` header to the start of the
/// next top-level `fn` (so a same-named `evaluate` call in a *later* function —
/// e.g. tdd's `derive` path — is not mistaken for the body's).
fn body_src(main_rs: &str, body_fn: &str) -> String {
    let start = main_rs
        .find(body_fn)
        .unwrap_or_else(|| panic!("body fn `{body_fn}` not found"));
    let rest = &main_rs[start..];
    // Skip the header line, then find the next top-level `\nfn ` boundary.
    let after_header = rest.find('\n').map(|i| i + 1).unwrap_or(0);
    let end = rest[after_header..]
        .find("\nfn ")
        .map(|i| after_header + i)
        .unwrap_or(rest.len());
    rest[..end].to_string()
}

/// Does `body` place every escape needle before the `::evaluate(` anchor?
/// Extracted so both the real-source guard and the synthetic teeth-check below
/// exercise the exact same comparison.
fn escapes_precede_evaluate(body: &str, skip_marker: &str) -> Result<(), String> {
    let eval_pos = body.find("::evaluate(").ok_or("no ::evaluate( anchor")?;
    let skip_needle = format!("consume_skip(&root, \"{skip_marker}\")");
    for needle in [
        "Config::disabled_env()",
        "!cfg.enabled",
        skip_needle.as_str(),
    ] {
        let pos = body
            .find(needle)
            .ok_or_else(|| format!("missing escape `{needle}`"))?;
        if pos >= eval_pos {
            return Err(format!(
                "escape `{needle}` at {pos} is not before evaluate at {eval_pos}"
            ));
        }
    }
    Ok(())
}

#[test]
fn ordering_check_has_teeth_on_a_violating_body() {
    // A body where verification runs BEFORE the skip marker must be REJECTED —
    // proving the guard below can actually go red (a green-only test is useless).
    let bad = "fn gate_run() {\n    if Config::disabled_env() {}\n    if !cfg.enabled {}\n    \
               let v = gate::evaluate(&cfg, &root);\n    consume_skip(&root, \".donegate-skip\");\n}";
    assert!(
        escapes_precede_evaluate(bad, ".donegate-skip").is_err(),
        "a post-evaluate skip marker must be flagged"
    );
    // The corrected order (skip before evaluate) must pass.
    let good = "fn gate_run() {\n    if Config::disabled_env() {}\n    if !cfg.enabled {}\n    \
                consume_skip(&root, \".donegate-skip\");\n    let v = gate::evaluate(&cfg, &root);\n}";
    assert!(escapes_precede_evaluate(good, ".donegate-skip").is_ok());
}

#[test]
fn operator_escapes_precede_panic_prone_verification() {
    let crates_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("harness-core is under crates/")
        .to_path_buf();

    for g in GATES {
        let main_rs_path = crates_dir.join(g.crate_name).join("src/main.rs");
        let main_rs = std::fs::read_to_string(&main_rs_path)
            .unwrap_or_else(|e| panic!("read {}: {e}", main_rs_path.display()));
        let body = body_src(&main_rs, g.body_fn);

        // All four gates dispatch verification through a `::evaluate(` call
        // (gate::evaluate / review::evaluate); a panic can only originate at/after
        // it, since config load, state load and the escape checks above it are all
        // fail-soft. Every operator escape (disabled env/config, skip marker) must
        // sit before that anchor — else it is unreachable when evaluate panics and
        // the fail-closed barrier blocks (a crashing gate an operator can't clear).
        if let Err(why) = escapes_precede_evaluate(&body, g.skip_marker) {
            panic!(
                "{} ({}): {why}. Because the Stop-gate panic barrier fails CLOSED \
                 (block), a post-evaluate escape is unreachable on a panic — move \
                 the escape back above `::evaluate(`.",
                g.crate_name, g.body_fn
            );
        }
    }
}
