//! v1.1 canon §8 (amended 4322e061): each remote command has an execution timeout
//! (default 60 s, clamp 5..=600). CONTRACT.md §12.9: config key `remote_command_timeout_secs`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(unix)]
mod common;
use common::*;
use std::time::Instant;

/// Wrap the fake ssh: the W15 command sleeps 40 s, everything else is delegated.
fn hang_w15(env: &mut Env) {
    let real = env.vars.get("HOSTAUDIT_SSH").cloned().unwrap();
    let wrap = env.fakes.join("ssh-hang");
    let body = format!(
        "#!/bin/sh\ncmd=\"\"\nfor a in \"$@\"; do cmd=$a; done\nif [ \"$cmd\" = '{W15}' ]; then exec sleep 40; fi\nexec '{real}' \"$@\"\n"
    );
    write_exec(&wrap, &body);
    env.set("HOSTAUDIT_SSH", wrap.to_str().unwrap());
}

fn run_with_timeout_cfg(cfg: &str) -> (Out, std::time::Duration) {
    let mut env = Env::new();
    env.eol_ubuntu_supported();
    env.one_selected_host(&clean_host());
    // Written after `select` so only `audit` depends on the new key.
    write(&env.hostaudit_dir().join("config.toml"), cfg);
    hang_w15(&mut env);
    let t = Instant::now();
    let o = env.run(&["audit", "--json"]);
    (o, t.elapsed())
}

#[test]
fn hung_command_times_out_into_undetermined_for_its_rule_only() {
    let (o, el) = run_with_timeout_cfg("remote_command_timeout_secs = 5\n");
    let fs = o.findings();
    let s2 = finding(&fs, "web1", "S2");
    assert_eq!(s2["verdict"], "undetermined", "{s2}");
    assert!(
        s2["reason"].as_str().unwrap_or("").contains("時間切れ"),
        "reason must say 時間切れ: {s2}"
    );
    assert_eq!(o.code, 1, "{}", o.dbg());
    // Other rules are unaffected by one hung command.
    assert_eq!(verdict(&fs, "web1", "S1"), "clean");
    assert_eq!(verdict(&fs, "web1", "B1"), "clean");
    // It gave up around the limit instead of waiting for the 40 s sleep.
    assert!(
        el.as_secs() < 30,
        "audit took {el:?}; the hung command was not cut off"
    );
}

#[test]
fn timeout_below_the_floor_is_clamped_to_five_seconds_not_zero() {
    // 0 must clamp UP to 5: if it were honoured, every command would time out instantly.
    let (o, el) = run_with_timeout_cfg("remote_command_timeout_secs = 0\n");
    let fs = o.findings();
    assert_eq!(verdict(&fs, "web1", "S2"), "undetermined");
    assert_eq!(
        verdict(&fs, "web1", "S1"),
        "clean",
        "fast commands must still succeed: {}",
        o.dbg()
    );
    assert_eq!(verdict(&fs, "web1", "B1"), "clean");
    assert!(el.as_secs() < 30, "{el:?}");
}
