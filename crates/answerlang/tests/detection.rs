#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod common;
use common::*;

const LATIN_PROMPT: &str = "please fix the failing build in the repository";

#[test]
fn japanese_prompt_determines_japanese_and_skips_macos_stub() {
    let env = Env::new().with_stub(Stub::Ja);
    env.write_config("language = \"auto\"\n");
    let out = env.prompt("s1", "このバグを直してください。お願いします。");
    assert_eq!(out.code, 0);
    assert!(out.context().contains(JA_LINE), "stdout: {}", out.stdout);
    assert_eq!(
        env.stub_calls(),
        0,
        "stub must not run when the prompt determines language"
    );
}

#[test]
fn korean_prompt_determines_korean() {
    let env = Env::new();
    env.write_config("language = \"auto\"\n");
    let out = env.prompt("s1", "이 버그를 고쳐 주세요 감사합니다");
    assert!(
        out.context().contains("write them in korean"),
        "stdout: {}",
        out.stdout
    );
}

#[test]
fn kana_beats_han_in_prompt() {
    let env = Env::new();
    env.write_config("language = \"auto\"\n");
    let out = env.prompt("s1", "修正 修正 修正 修正 修正 修正 してください");
    assert!(
        out.context().contains("write them in japanese"),
        "stdout: {}",
        out.stdout
    );
}

#[test]
fn han_only_prompt_is_chinese() {
    let env = Env::new();
    env.write_config("language = \"auto\"\n");
    let out = env.prompt("s1", "请修复这个错误并且运行测试");
    assert!(
        out.context().contains("write them in chinese"),
        "stdout: {}",
        out.stdout
    );
}

#[test]
fn cyrillic_prompt_is_russian() {
    let env = Env::new();
    env.write_config("language = \"auto\"\n");
    let out = env.prompt("s1", "пожалуйста исправьте эту ошибку");
    assert!(
        out.context().contains("write them in russian"),
        "stdout: {}",
        out.stdout
    );
}

#[cfg(target_os = "macos")]
#[test]
fn latin_prompt_falls_back_to_macos_ui_language() {
    let env = Env::new().with_stub(Stub::Ja).with_lang("C");
    env.write_config("language = \"auto\"\n");
    let out = env.prompt("s1", LATIN_PROMPT);
    assert!(out.context().contains(JA_LINE), "stdout: {}", out.stdout);
    assert_eq!(env.stub_calls(), 1);
}

#[cfg(target_os = "macos")]
#[test]
fn macos_failure_falls_through_to_lang_env() {
    let env = Env::new().with_stub(Stub::Fail).with_lang("ko_KR.UTF-8");
    env.write_config("language = \"auto\"\n");
    let out = env.prompt("s1", LATIN_PROMPT);
    assert!(
        out.context().contains("write them in korean"),
        "stdout: {}",
        out.stdout
    );
}

#[cfg(target_os = "macos")]
#[test]
fn macos_stub_runs_at_most_once_per_session() {
    let env = Env::new().with_stub(Stub::Ja);
    env.write_config("language = \"auto\"\n");
    let a = env.session_start("once");
    assert!(a.context().contains(JA_LINE), "stdout: {}", a.stdout);
    for _ in 0..3 {
        let o = env.prompt("once", LATIN_PROMPT);
        assert!(o.context().contains(JA_LINE), "stdout: {}", o.stdout);
    }
    assert!(
        env.stub_calls() <= 1,
        "stub invoked {} times",
        env.stub_calls()
    );
}

#[cfg(target_os = "macos")]
#[test]
fn macos_undetermined_result_is_also_cached() {
    let env = Env::new().with_stub(Stub::Fail).with_lang("C");
    env.write_config("language = \"auto\"\n");
    env.session_start("once");
    for _ in 0..3 {
        env.prompt("once", LATIN_PROMPT);
    }
    assert!(
        env.stub_calls() <= 1,
        "stub invoked {} times",
        env.stub_calls()
    );
}

#[test]
fn lang_env_ko_determines_korean_when_stub_fails() {
    let env = Env::new().with_stub(Stub::Fail).with_lang("ko_KR.UTF-8");
    env.write_config("language = \"auto\"\n");
    let out = env.prompt("s1", LATIN_PROMPT);
    assert!(
        out.context().contains("write them in korean"),
        "stdout: {}",
        out.stdout
    );
}

#[test]
fn lc_all_wins_over_lang() {
    let mut env = Env::new().with_lang("ko_KR.UTF-8");
    env.lc_all = Some("ja_JP.UTF-8".to_string());
    env.write_config("language = \"auto\"\n");
    let out = env.prompt("s1", LATIN_PROMPT);
    // On macOS the stub (fail) is skipped and env decides.
    assert!(out.context().contains(JA_LINE), "stdout: {}", out.stdout);
}

#[test]
fn nothing_determined_injects_no_language_line() {
    let env = Env::new().with_stub(Stub::Fail).with_lang("C");
    env.write_config("language = \"auto\"\n");
    let out = env.prompt("s1", LATIN_PROMPT);
    assert_eq!(out.code, 0);
    assert!(
        !out.context().contains("write them in"),
        "stdout: {}",
        out.stdout
    );
    assert!(
        out.is_empty(),
        "nothing to inject must mean empty stdout: {}",
        out.stdout
    );
}

#[test]
fn nothing_determined_stop_does_not_block() {
    let env = Env::new().with_stub(Stub::Fail).with_lang("C");
    env.write_config("language = \"auto\"\n");
    env.prompt("s1", LATIN_PROMPT);
    let t = env.answer_transcript("t.jsonl", &en(200));
    let out = env.stop("s1", Some(&t), false);
    assert_eq!(out.code, 0);
    assert!(!out.is_block(), "stdout: {}", out.stdout);
    let msg = out.system_message().unwrap_or_default();
    assert!(
        msg.to_lowercase().contains("undetermined"),
        "systemMessage: {msg:?}"
    );
}

#[test]
fn stop_uses_recorded_prompt_language_not_transcript() {
    // Prompt was Japanese; the English answer must be blocked even though the
    // transcript is mostly English (including feedback-looking user text).
    let env = Env::new().with_lang("C");
    env.write_config("language = \"auto\"\n");
    env.prompt("s1", "このバグを直してください。お願いします。");
    let t = env.transcript(
        "t.jsonl",
        &[user(&en(100)), assistant(&en(200)), user(&en(100))],
    );
    let out = env.stop("s1", Some(&t), false);
    assert!(out.is_block(), "stdout: {}", out.stdout);
    assert!(out.reason().to_lowercase().contains("japanese"));
}
