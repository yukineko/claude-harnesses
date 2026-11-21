//! session-master — one master per git common dir; master-only operations
//! (push / rollout / main merge / relay adjudication) are denied to members
//! and queued for the master, which processes the queue serially.
//!
//! See `README.ja.md` and the SPEC it cites. Exit codes for the CLI:
//! 0 = ok (or `role` master), 1 = refused (member / not in progress / `role`
//! member), 2 = cannot determine (stderr carries the reason) or usage error.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod config;
mod detect;
mod gate;
mod group;
mod hooks;
mod queue;
mod relay;
mod session;
mod state;

use queue::Reply;
use std::path::PathBuf;

const NAME: &str = "session-master";

struct Args {
    sub: String,
    positional: Vec<String>,
    session: Option<String>,
    cwd: Option<PathBuf>,
    name: Option<String>,
    kind: Option<String>,
    command: Option<String>,
    note: Option<String>,
    outcome: Option<String>,
    result: Option<String>,
    ack: bool,
}

fn parse_args(raw: Vec<String>) -> Result<Args, String> {
    let mut it = raw.into_iter();
    let sub = it.next().ok_or_else(usage)?;
    let mut a = Args {
        sub,
        positional: Vec::new(),
        session: None,
        cwd: None,
        name: None,
        kind: None,
        command: None,
        note: None,
        outcome: None,
        result: None,
        ack: false,
    };
    while let Some(x) = it.next() {
        let mut val = |flag: &str| it.next().ok_or_else(|| format!("{flag} に値が無い"));
        match x.as_str() {
            "--session" => a.session = Some(val("--session")?),
            "--cwd" => a.cwd = Some(PathBuf::from(val("--cwd")?)),
            "--name" => a.name = Some(val("--name")?),
            "--kind" => a.kind = Some(val("--kind")?),
            "--command" => a.command = Some(val("--command")?),
            "--note" => a.note = Some(val("--note")?),
            "--outcome" => a.outcome = Some(val("--outcome")?),
            "--result" => a.result = Some(val("--result")?),
            "--ack" => a.ack = true,
            s if s.starts_with("--") => return Err(format!("未知の option {s}")),
            _ => a.positional.push(x),
        }
    }
    Ok(a)
}

fn usage() -> String {
    "usage: session-master <register [--name N]|heartbeat|leave|name <NAME>|role|status|\
     request --kind K --command C [--note T]|next|complete <ID> --outcome ok|failed [--result T]|\
     results [--ack]|gate|hook <session-start|prompt|stop|session-end>|--version> \
     [--session ID] [--cwd DIR]"
        .to_string()
}

fn session_of(a: &Args) -> Result<String, String> {
    if let Some(s) = &a.session {
        if !s.is_empty() {
            return Ok(s.clone());
        }
    }
    match std::env::var("CLAUDE_CODE_SESSION_ID") {
        Ok(s) if !s.is_empty() => Ok(s),
        _ => Err("--session も CLAUDE_CODE_SESSION_ID も無い".to_string()),
    }
}

fn cwd_of(a: &Args) -> Result<PathBuf, String> {
    match &a.cwd {
        Some(c) => Ok(c.clone()),
        None => std::env::current_dir().map_err(|e| format!("cwd を決められない ({e})")),
    }
}

/// `role` prints `undetermined` on stdout when it cannot decide.
fn finish(r: Result<Reply, String>, undetermined_stdout: Option<&str>) -> ! {
    match r {
        Ok(reply) => {
            if let Some(s) = reply.stdout {
                println!("{s}");
            }
            if let Some(e) = reply.stderr {
                eprintln!("{e}");
            }
            std::process::exit(reply.code)
        }
        Err(e) => {
            if let Some(stdout) = undetermined_stdout {
                println!("{stdout}");
            }
            eprintln!("{NAME}: 判定不能: {e}");
            std::process::exit(2)
        }
    }
}

fn cli(a: Args) -> Result<Reply, String> {
    let need = |a: &Args| -> Result<(String, PathBuf), String> { Ok((session_of(a)?, cwd_of(a)?)) };
    match a.sub.as_str() {
        "register" | "heartbeat" => {
            let (s, c) = need(&a)?;
            session::register(&s, &c, a.name.clone())
        }
        "leave" => {
            let (s, c) = need(&a)?;
            session::leave(&s, &c)
        }
        "name" => {
            let (s, c) = need(&a)?;
            match a.positional.first() {
                Some(n) => session::name(&s, &c, n),
                None => Ok(Reply::refuse(2, "usage: session-master name <NAME>".into())),
            }
        }
        "role" => {
            let (s, c) = need(&a)?;
            session::role(&s, &c)
        }
        "status" => session::status(&cwd_of(&a)?),
        "request" => {
            let (s, c) = need(&a)?;
            match (&a.kind, &a.command) {
                (Some(k), Some(cmd)) => queue::request(&s, &c, k, cmd, a.note.clone()),
                _ => Ok(Reply::refuse(
                    2,
                    "request には --kind と --command が要る".into(),
                )),
            }
        }
        "next" => {
            let (s, c) = need(&a)?;
            queue::next(&s, &c)
        }
        "complete" => {
            let (s, c) = need(&a)?;
            match (a.positional.first(), &a.outcome) {
                (Some(id), Some(o)) => queue::complete(&s, &c, id, o, a.result.clone()),
                _ => Ok(Reply::refuse(
                    2,
                    "usage: session-master complete <ID> --outcome ok|failed [--result TEXT]"
                        .into(),
                )),
            }
        }
        "results" => {
            let (s, c) = need(&a)?;
            queue::results(&s, &c, a.ack)
        }
        other => Ok(Reply::refuse(
            2,
            format!("未知の subcommand {other:?}\n{}", usage()),
        )),
    }
}

/// session-start / prompt / session-end: no `run_hook` (which would turn a
/// crash into a silent exit 0). A panic is reported on stderr, exit 1.
fn guarded_hook(name: &str, body: impl FnOnce() -> Result<Option<String>, String>) -> ! {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)) {
        Ok(Ok(out)) => {
            if let Some(s) = out {
                println!("{s}");
            }
            std::process::exit(0)
        }
        Ok(Err(e)) => {
            eprintln!("{NAME} hook {name}: {e}");
            std::process::exit(1)
        }
        Err(_) => {
            eprintln!("{NAME} hook {name}: panic した（hook は実行されていない）");
            std::process::exit(1)
        }
    }
}

fn read_stdin() -> Result<String, String> {
    harness_core::hook::try_read_stdin().map_err(|e| format!("stdin を読めない ({e})"))
}

fn hook(which: &str) -> ! {
    match which {
        "session-start" => guarded_hook(which, || Ok(Some(hooks::session_start(&read_stdin()?)))),
        "prompt" => guarded_hook(which, || Ok(Some(hooks::prompt(&read_stdin()?)))),
        "session-end" => guarded_hook(which, || {
            hooks::session_end(&read_stdin()?)?;
            Ok(None)
        }),
        "stop" => {
            let raw = harness_core::gate::run::read_stop_payload_guarded(
                NAME,
                false,
                harness_core::hook::try_read_stdin(),
            );
            let payload = hooks::parse_payload(&raw);
            // Unknown `stop_hook_active` resolves to `false` (= "first stop"),
            // under which a panic blocks: the restrictive side.
            let active = match &payload {
                Ok(v) => matches!(
                    v.get("stop_hook_active"),
                    Some(serde_json::Value::Bool(true))
                ),
                Err(_) => false,
            };
            harness_core::gate::run::run_guarded(NAME, false, active, || {
                let line = match payload {
                    Ok(v) => hooks::stop(&v),
                    Err(e) => Some(
                        serde_json::json!({"systemMessage": format!("session-master stop: {e}（判定不能）。役割と queue は未確認。")})
                            .to_string(),
                    ),
                };
                if let Some(l) = line {
                    println!("{l}");
                }
                std::process::exit(0)
            })
        }
        other => {
            eprintln!("{NAME}: 未知の hook {other:?}");
            std::process::exit(1)
        }
    }
}

fn main() {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    match raw.first().map(String::as_str) {
        Some("--version") | Some("-V") => {
            println!("{NAME} {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        Some("gate") => harness_core::hook::run_pretooluse_guarded(NAME, gate::run),
        Some("hook") => match raw.get(1) {
            Some(w) => hook(w),
            None => {
                eprintln!("{NAME}: hook の種類が無い");
                std::process::exit(1)
            }
        },
        _ => {}
    }
    match parse_args(raw) {
        Ok(a) => {
            let undet = (a.sub == "role").then_some("undetermined");
            finish(cli(a), undet)
        }
        Err(e) => {
            eprintln!("{NAME}: {e}\n{}", usage());
            std::process::exit(2)
        }
    }
}
