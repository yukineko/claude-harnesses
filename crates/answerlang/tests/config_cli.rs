#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod common;
use common::*;

const DIRECTIVE_KEYS: [&str; 2] = ["AskUserQuestion", "answerlang set --language"];

#[test]
fn inject_on_emits_exact_language_line() {
    let env = Env::new();
    env.write_config("language = \"japanese\"\ninject = true\n");
    let out = env.session_start("s1");
    assert_eq!(out.code, 0);
    let v = out.json().expect("json");
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "SessionStart");
    assert!(out.context().contains(JA_LINE), "stdout: {}", out.stdout);
    let p = env.prompt("s1", "hello there");
    assert_eq!(
        p.json().unwrap()["hookSpecificOutput"]["hookEventName"],
        "UserPromptSubmit"
    );
    assert!(p.context().contains(JA_LINE));
}

#[test]
fn inject_off_prints_nothing() {
    let env = Env::new();
    env.write_config("language = \"japanese\"\ninject = false\n");
    let a = env.session_start("s1");
    let b = env.prompt("s1", "hello there");
    assert!(a.is_empty(), "stdout: {}", a.stdout);
    assert!(b.is_empty(), "stdout: {}", b.stdout);
    assert_eq!(a.code, 0);
    assert_eq!(b.code, 0);
}

#[test]
fn enforce_off_stop_prints_nothing_but_inject_still_works() {
    let env = Env::new();
    env.write_config("language = \"japanese\"\nenforce = false\n");
    let t = env.answer_transcript("t.jsonl", &en(200));
    let out = env.stop("s1", Some(&t), false);
    assert!(out.is_empty(), "stdout: {}", out.stdout);
    assert!(env.session_start("s1").context().contains(JA_LINE));
}

#[test]
fn enforce_on_blocks_english() {
    let env = Env::new();
    env.write_config("language = \"japanese\"\nenforce = true\n");
    let t = env.answer_transcript("t.jsonl", &en(200));
    assert!(env.stop("s1", Some(&t), false).is_block());
}

#[test]
fn master_disabled_silences_all_hooks() {
    let env = Env::new();
    env.write_config("enabled = false\nlanguage = \"japanese\"\n");
    let t = env.answer_transcript("t.jsonl", &en(200));
    for out in [
        env.session_start("s1"),
        env.prompt("s1", "hello there"),
        env.stop("s1", Some(&t), false),
    ] {
        assert_eq!(out.code, 0);
        assert!(out.is_empty(), "stdout: {}", out.stdout);
    }
}

// ---- show / min_share ----

#[test]
fn show_prints_exact_keys() {
    let env = Env::new();
    env.write_config("language = \"japanese\"\n");
    let out = env.run(&["show"], None);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let m = out.show_map();
    let keys: Vec<&str> = m.keys().map(String::as_str).collect();
    let mut want = vec![
        "config_path",
        "config_present",
        "enabled",
        "inject",
        "enforce",
        "language",
        "min_share",
        "effective_language",
        "language_source",
    ];
    want.sort();
    assert_eq!(keys, want, "stdout: {}", out.stdout);
    assert_eq!(m["config_present"], "true");
    assert_eq!(m["language"], "japanese");
    assert_eq!(m["effective_language"], "japanese");
    assert_eq!(m["language_source"], "config");
    assert_eq!(m["min_share"], "0.3");
    assert_eq!(m["enabled"], "true");
}

#[test]
fn show_min_share_is_clamped() {
    let env = Env::new();
    env.write_config("language = \"japanese\"\nmin_share = 0.0\n");
    assert_eq!(env.run(&["show"], None).show_map()["min_share"], "0.05");
    env.write_config("language = \"japanese\"\nmin_share = 1.0\n");
    assert_eq!(env.run(&["show"], None).show_map()["min_share"], "0.95");
}

#[test]
fn show_without_config_reports_absent() {
    let env = Env::new().with_lang("ko_KR.UTF-8");
    let out = env.run(&["show"], None);
    let m = out.show_map();
    assert_eq!(m["config_present"], "false");
    assert_eq!(m["language"], "auto");
    assert!(
        !env.config_path().exists(),
        "show must not create the config"
    );
}

#[test]
fn show_env_source_and_none() {
    let env = Env::new().with_lang("ko_KR.UTF-8");
    env.write_config("language = \"auto\"\n");
    let m = env.run(&["show"], None).show_map();
    // On macOS the stub fails, so env decides.
    assert_eq!(m["effective_language"], "korean");
    assert_eq!(m["language_source"], "env");
    let env2 = Env::new().with_lang("C");
    env2.write_config("language = \"auto\"\n");
    let m2 = env2.run(&["show"], None).show_map();
    assert_eq!(m2["effective_language"], "none");
    assert_eq!(m2["language_source"], "none");
}

#[cfg(target_os = "macos")]
#[test]
fn show_macos_source() {
    let env = Env::new().with_stub(Stub::Ja).with_lang("C");
    env.write_config("language = \"auto\"\n");
    let m = env.run(&["show"], None).show_map();
    assert_eq!(m["effective_language"], "japanese");
    assert_eq!(m["language_source"], "macos");
}

#[test]
fn language_tags_map_by_primary_subtag() {
    let env = Env::new();
    env.write_config("language = \"ja-JP\"\n");
    assert_eq!(
        env.run(&["show"], None).show_map()["effective_language"],
        "japanese"
    );
    env.write_config("language = \"en_US.UTF-8\"\n");
    assert_eq!(
        env.run(&["show"], None).show_map()["effective_language"],
        "english"
    );
}

// ---- min_share clamp on Stop ----

#[test]
fn min_share_zero_is_clamped_up_and_still_blocks() {
    let env = Env::new();
    env.write_config("language = \"japanese\"\nmin_share = 0.0\n");
    let t = env.answer_transcript("t.jsonl", &en(200));
    assert!(env.stop("s1", Some(&t), false).is_block());
    // 3% Japanese: only a clamp to 0.05 blocks it (an unclamped 0.0 would pass).
    let text = format!("あいう {}", "q".repeat(97));
    let t2 = env.answer_transcript("t2.jsonl", &text);
    assert!(env.stop("s2", Some(&t2), false).is_block());
}

#[test]
fn min_share_one_is_clamped_down_to_095() {
    let env = Env::new();
    env.write_config("language = \"japanese\"\nmin_share = 1.0\n");
    let text = format!("{} abc", "あ".repeat(97));
    let t = env.answer_transcript("t.jsonl", &text);
    let out = env.stop("s1", Some(&t), false);
    assert!(
        !out.is_block(),
        "97% share must pass at clamped 0.95: {}",
        out.stdout
    );
    // 90% still below the clamped bound.
    let text2 = format!("{} {}", "あ".repeat(90), "q".repeat(10));
    let t2 = env.answer_transcript("t2.jsonl", &text2);
    assert!(env.stop("s2", Some(&t2), false).is_block());
}

#[test]
fn default_min_share_boundary() {
    let env = Env::new();
    env.write_config("language = \"japanese\"\n");
    // 40% Japanese passes the default 0.3
    let ok = format!("{} {}", "あ".repeat(40), "q".repeat(60));
    let t = env.answer_transcript("ok.jsonl", &ok);
    assert!(!env.stop("s1", Some(&t), false).is_block());
    // 20% blocks
    let bad = format!("{} {}", "あ".repeat(20), "q".repeat(80));
    let t2 = env.answer_transcript("bad.jsonl", &bad);
    assert!(env.stop("s2", Some(&t2), false).is_block());
}

// ---- config absent: ask once ----

#[test]
fn config_absent_injects_ask_directive() {
    let env = Env::new().with_lang("ja_JP.UTF-8");
    let a = env.session_start("s1");
    for k in DIRECTIVE_KEYS {
        assert!(a.context().contains(k), "missing {k}: {}", a.stdout);
    }
    assert!(
        a.context().contains("write them in japanese"),
        "stdout: {}",
        a.stdout
    );
    let b = env.prompt("s1", "hello there");
    for k in DIRECTIVE_KEYS {
        assert!(b.context().contains(k), "missing {k}: {}", b.stdout);
    }
}

#[test]
fn config_absent_stop_never_blocks_but_notes() {
    let env = Env::new().with_lang("ja_JP.UTF-8");
    let t = env.answer_transcript("t.jsonl", &en(200));
    let out = env.stop("s1", Some(&t), false);
    assert_eq!(out.code, 0);
    assert!(!out.is_block(), "stdout: {}", out.stdout);
    assert!(out.system_message().is_some(), "stdout: {}", out.stdout);
}

#[test]
fn set_language_removes_directive_and_enables_enforcement() {
    let env = Env::new().with_lang("C");
    let t = env.answer_transcript("t.jsonl", &en(200));
    assert!(env
        .session_start("s1")
        .context()
        .contains("AskUserQuestion"));
    let set = env.run(&["set", "--language", "japanese"], None);
    assert_eq!(set.code, 0, "stderr: {}", set.stderr);
    assert!(env.config_path().exists());
    let a = env.session_start("s1");
    assert!(
        !a.context().contains("AskUserQuestion"),
        "stdout: {}",
        a.stdout
    );
    assert!(a.context().contains(JA_LINE));
    assert!(env.stop("s1", Some(&t), false).is_block());
}

#[test]
fn set_round_trips_through_show() {
    let env = Env::new();
    let set = env.run(
        &[
            "set",
            "--language",
            "Japanese",
            "--inject",
            "off",
            "--enforce",
            "off",
            "--enabled",
            "off",
        ],
        None,
    );
    assert_eq!(set.code, 0, "stderr: {}", set.stderr);
    let m = env.run(&["show"], None).show_map();
    assert_eq!(m["language"], "japanese");
    assert_eq!(m["inject"], "false");
    assert_eq!(m["enforce"], "false");
    assert_eq!(m["enabled"], "false");
    assert_eq!(m["config_present"], "true");
    let set2 = env.run(
        &[
            "set",
            "--inject",
            "on",
            "--enforce",
            "on",
            "--enabled",
            "on",
        ],
        None,
    );
    assert_eq!(set2.code, 0);
    let m2 = env.run(&["show"], None).show_map();
    assert_eq!(m2["inject"], "true");
    assert_eq!(m2["enforce"], "true");
    assert_eq!(m2["enabled"], "true");
    assert_eq!(m2["language"], "japanese");
}

#[test]
fn set_without_options_exits_2() {
    let env = Env::new();
    let out = env.run(&["set"], None);
    assert_eq!(out.code, 2);
    assert!(!out.stderr.trim().is_empty(), "usage expected on stderr");
}

#[test]
fn set_invalid_on_off_fails_and_leaves_file_unchanged() {
    let env = Env::new();
    assert_eq!(env.run(&["set", "--language", "japanese"], None).code, 0);
    let before = env.read_config().expect("config written");
    for args in [
        vec!["set", "--inject", "maybe"],
        vec!["set", "--enforce", "2"],
        vec!["set", "--enabled", "yes please"],
        vec!["set", "--language", "korean", "--inject", "bogus"],
    ] {
        let out = env.run(&args, None);
        assert_ne!(out.code, 0, "args {args:?} should fail");
        assert_eq!(
            env.read_config().unwrap(),
            before,
            "file changed for {args:?}"
        );
    }
}

#[test]
fn set_merges_keys_not_given() {
    let env = Env::new();
    assert_eq!(env.run(&["set", "--language", "japanese"], None).code, 0);
    assert_eq!(env.run(&["set", "--inject", "off"], None).code, 0);
    let m = env.run(&["show"], None).show_map();
    assert_eq!(m["language"], "japanese");
    assert_eq!(m["inject"], "false");
    assert_eq!(env.run(&["set", "--language", "auto"], None).code, 0);
    let m2 = env.run(&["show"], None).show_map();
    assert_eq!(m2["language"], "auto");
    assert_eq!(m2["inject"], "false");
}

#[test]
fn set_preserves_hand_written_min_share() {
    let env = Env::new();
    env.write_config("min_share = 0.6\nlanguage = \"auto\"\n");
    assert_eq!(env.run(&["set", "--language", "japanese"], None).code, 0);
    assert_eq!(env.run(&["show"], None).show_map()["min_share"], "0.6");
}

// ---- unparseable config ----

#[test]
fn unparseable_config_warns_and_uses_defaults_with_enforcement() {
    let env = Env::new().with_lang("ja_JP.UTF-8");
    env.write_config("this is = = not [[[ toml\n");
    let t = env.answer_transcript("t.jsonl", &en(200));
    let out = env.stop("s1", Some(&t), false);
    assert!(!out.stderr.trim().is_empty(), "stderr must carry a message");
    assert!(
        out.is_block(),
        "defaults apply, enforcement active: {}",
        out.stdout
    );
    let p = env.prompt("s1", "hello there");
    assert!(
        !p.context().contains("AskUserQuestion"),
        "unparseable still counts as present"
    );
    assert!(p.context().contains(JA_LINE));
}

// ---- unknown explicit language ----

#[test]
fn unknown_explicit_language_injects_verbatim_and_does_not_block() {
    let env = Env::new();
    env.write_config("language = \"klingon\"\n");
    let a = env.session_start("s1");
    assert!(
        a.context().contains("write them in klingon"),
        "stdout: {}",
        a.stdout
    );
    let t = env.answer_transcript("t.jsonl", &en(200));
    let out = env.stop("s1", Some(&t), false);
    assert!(!out.is_block(), "stdout: {}", out.stdout);
    let msg = out.system_message().unwrap_or_default().to_lowercase();
    assert!(msg.contains("unavailable"), "systemMessage: {msg:?}");
}
