//! CLI / hook contract tests for playbook. Each drives the real `playbook`
//! binary in a fresh temp HOME (so `~/.playbook` and `~/.claude` resolve
//! inside the sandbox) and asserts the specific observable outcome. Where the
//! README once described different behaviour, the test asserts the OBSERVED
//! behaviour and says so in its comment.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

/// A throwaway HOME plus a project directory `<home>/alphaproj`.
struct Sb {
    home: PathBuf,
    proj: PathBuf,
}

impl Sb {
    fn new(tag: &str) -> Sb {
        let raw = std::env::temp_dir().join(format!(
            "playbook-claims-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&raw).unwrap();
        // Canonicalize: the binary's current_dir() reports the resolved path
        // (/private/var on macOS) and the project-store hash is over that string.
        let home = raw.canonicalize().unwrap();
        let proj = home.join("alphaproj");
        std::fs::create_dir_all(&proj).unwrap();
        Sb { home, proj }
    }

    fn other_dir(&self) -> PathBuf {
        let q = self.home.join("betaproj");
        std::fs::create_dir_all(&q).unwrap();
        q
    }

    fn store(&self) -> PathBuf {
        self.home.join(".playbook").join("store")
    }

    fn run_in(&self, cwd: &Path, args: &[&str], stdin: Option<&str>, env: &[(&str, &str)]) -> Out {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_playbook"));
        cmd.args(args)
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env_remove("PLAYBOOK_DISABLE")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().expect("spawn playbook");
        if let Some(mut si) = child.stdin.take() {
            let _ = si.write_all(stdin.unwrap_or("").as_bytes());
        }
        let o = child.wait_with_output().expect("wait playbook");
        Out {
            code: o.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        }
    }

    fn run(&self, args: &[&str]) -> Out {
        self.run_in(&self.proj, args, None, &[])
    }

    fn inject_in(&self, cwd: &Path, prompt: &str, env: &[(&str, &str)]) -> Out {
        let payload = serde_json::json!({ "cwd": cwd, "prompt": prompt }).to_string();
        self.run_in(cwd, &["inject"], Some(&payload), env)
    }

    fn inject(&self, prompt: &str) -> Out {
        self.inject_in(&self.proj, prompt, &[])
    }

    fn add(&self, title: &str, trigger: &str, tags: &str, body: &str, extra: &[&str]) -> Out {
        let mut args = vec![
            "add",
            "--title",
            title,
            "--trigger",
            trigger,
            "--tags",
            tags,
            "--body",
            body,
        ];
        args.extend_from_slice(extra);
        let o = self.run(&args);
        assert_eq!(o.code, 0, "add failed: {}", o.stderr);
        o
    }

    fn write_project_config(&self, toml: &str) {
        std::fs::write(self.proj.join("playbook.toml"), toml).unwrap();
    }

    /// Project store dir as reported by `status` (the binary is the authority).
    fn project_store_dir(&self) -> PathBuf {
        let st = self.run(&["status"]);
        let line = st
            .stdout
            .lines()
            .find(|l| l.starts_with("project store:"))
            .expect("status prints project store");
        PathBuf::from(line["project store:".len()..].trim())
    }

    /// Score shown by `search` for `slug`, plus whether its line is ✓-marked.
    fn search_score(&self, query: &str, slug: &str) -> (i64, bool) {
        let o = self.run(&["search", query]);
        assert_eq!(o.code, 0, "search failed: {}", o.stderr);
        let mut found = None;
        for l in o.stdout.lines() {
            let checked = l.starts_with('✓');
            let toks: Vec<&str> = l.split_whitespace().filter(|t| *t != "✓").collect();
            if toks.len() >= 2 && toks[1] == slug {
                if let Ok(score) = toks[0].parse::<i64>() {
                    found = Some((score, checked));
                }
            }
        }
        found.unwrap_or_else(|| panic!("no search line for {slug}: {}", o.stdout))
    }
}

impl Drop for Sb {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn read_json(p: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn commands(settings: &serde_json::Value, event: &str) -> Vec<String> {
    settings["hooks"][event]
        .as_array()
        .map(|groups| {
            groups
                .iter()
                .flat_map(|g| g["hooks"].as_array().cloned().unwrap_or_default())
                .filter_map(|h| h["command"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------- claims

#[test]
fn claim_deterministic() {
    let sb = Sb::new("det");
    sb.add("det note", "zebraxyz", "", "DETBODYMARK", &[]);
    let a = sb.inject("tell me about zebraxyz");
    let b = sb.inject("tell me about zebraxyz");
    assert_eq!(a.code, 0);
    assert!(
        a.stdout.contains("DETBODYMARK"),
        "trigger-only match must inject: {:?}",
        a.stdout
    );
    assert_eq!(a.stdout, b.stdout, "same store+prompt => identical stdout");
}

#[test]
fn claim_hooks_json_wiring() {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("hooks/hooks.json");
    let v = read_json(&p);
    let groups = v["hooks"]["UserPromptSubmit"].as_array().expect("event");
    let cmds: Vec<&str> = groups
        .iter()
        .flat_map(|g| g["hooks"].as_array().unwrap())
        .map(|h| h["command"].as_str().unwrap())
        .collect();
    assert!(
        cmds.contains(&"${CLAUDE_PLUGIN_ROOT}/bin/playbook inject"),
        "got {cmds:?}"
    );
}

#[test]
fn claim_install_backs_up() {
    let sb = Sb::new("install");
    let claude = sb.home.join(".claude");
    std::fs::create_dir_all(&claude).unwrap();
    let settings = claude.join("settings.json");
    let original = r#"{"hooks":{"UserPromptSubmit":[{"hooks":[{"type":"command","command":"ctxrot guard"}]}]},"model":"x"}"#;
    std::fs::write(&settings, original).unwrap();

    let o = sb.run(&["install"]);
    assert_eq!(o.code, 0, "install failed: {}", o.stderr);

    let cmds = commands(&read_json(&settings), "UserPromptSubmit");
    assert!(
        cmds.iter()
            .any(|c| c.contains("playbook") && c.ends_with(" inject")),
        "no playbook inject hook: {cmds:?}"
    );
    assert!(cmds.iter().any(|c| c == "ctxrot guard"), "foreign kept");

    let baks: Vec<PathBuf> = std::fs::read_dir(&claude)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("settings.json.bak-")
        })
        .collect();
    assert_eq!(baks.len(), 1, "exactly one backup expected: {baks:?}");
    assert_eq!(std::fs::read_to_string(&baks[0]).unwrap(), original);
}

#[test]
fn claim_uninstall_and_dry_run() {
    let sb = Sb::new("uninstall");
    let claude = sb.home.join(".claude");
    std::fs::create_dir_all(&claude).unwrap();
    let settings = claude.join("settings.json");
    std::fs::write(
        &settings,
        r#"{"hooks":{"UserPromptSubmit":[{"hooks":[{"type":"command","command":"ctxrot guard"}]}]}}"#,
    )
    .unwrap();

    // install --dry-run: exit 0, file untouched.
    let before = std::fs::read(&settings).unwrap();
    let d = sb.run(&["install", "--dry-run"]);
    assert_eq!(d.code, 0, "{}", d.stderr);
    assert_eq!(std::fs::read(&settings).unwrap(), before);

    assert_eq!(sb.run(&["install"]).code, 0);
    assert!(commands(&read_json(&settings), "UserPromptSubmit")
        .iter()
        .any(|c| c.contains("playbook")));

    // uninstall --dry-run: exit 0, file untouched.
    let before = std::fs::read(&settings).unwrap();
    let d = sb.run(&["uninstall", "--dry-run"]);
    assert_eq!(d.code, 0, "{}", d.stderr);
    assert_eq!(std::fs::read(&settings).unwrap(), before);

    // uninstall: playbook group gone, foreign group kept.
    let u = sb.run(&["uninstall"]);
    assert_eq!(u.code, 0, "{}", u.stderr);
    let cmds = commands(&read_json(&settings), "UserPromptSubmit");
    assert!(
        !cmds.iter().any(|c| c.contains("playbook")),
        "playbook hook still present: {cmds:?}"
    );
    assert_eq!(cmds, vec!["ctxrot guard".to_string()]);
}

#[test]
fn inject_never_suggests_flow() {
    let sb = Sb::new("flow");
    for p in [
        "condukt で回して",
        "let's run this via /flow",
        "本番 struct を変更",
        "production change but can't test",
        "trait refactor but test impossible",
        "hello",
    ] {
        let o = sb.inject(p);
        assert_eq!(o.code, 0, "prompt {p:?}: {}", o.stderr);
        assert!(
            !o.stdout.contains("→ /flow"),
            "prompt {p:?} got a /flow suggestion: {:?}",
            o.stdout
        );
        assert!(
            !o.stdout.contains("複雑な問題"),
            "prompt {p:?} got a complexity banner: {:?}",
            o.stdout
        );
    }
}

#[test]
fn notes_still_injected_without_flow_line() {
    let sb = Sb::new("flownotes");
    sb.add("fn note", "zebraxyz", "", "FLOWNOTEBODY", &[]);
    let o = sb.inject("condukt で回して zebraxyz");
    assert_eq!(o.code, 0);
    assert!(o.stdout.contains("FLOWNOTEBODY"), "{:?}", o.stdout);
    assert!(!o.stdout.contains("→ /flow"), "{:?}", o.stdout);
}

#[test]
fn claim_visible_stores() {
    let sb = Sb::new("stores");
    sb.add("proj note", "zebraxyz", "", "PROJBODY", &[]);
    sb.add("glob note", "zebraxyz", "", "GLOBBODY", &["--global"]);

    // On-disk layout: <store>/alphaproj-<8 hex>/ and <store>/_global/.
    let mut names: Vec<String> = std::fs::read_dir(sb.store())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names.len(), 2, "{names:?}");
    assert_eq!(names[0], "_global");
    let suffix = names[1].strip_prefix("alphaproj-").expect("basename-hash");
    assert_eq!(suffix.len(), 8);
    assert!(suffix.chars().all(|c| c.is_ascii_hexdigit()));
    assert!(sb.store().join(&names[1]).join("proj-note.md").exists());
    assert!(sb.store().join("_global").join("glob-note.md").exists());

    let here = sb.inject("zebraxyz");
    assert!(here.stdout.contains("PROJBODY") && here.stdout.contains("GLOBBODY"));

    let q = sb.other_dir();
    let there = sb.inject_in(&q, "zebraxyz", &[]);
    assert!(there.stdout.contains("GLOBBODY"), "{:?}", there.stdout);
    assert!(!there.stdout.contains("PROJBODY"), "{:?}", there.stdout);
}

#[test]
fn claim_score_weights() {
    let sb = Sb::new("score");
    sb.add("tn one", "alphatrig", "", "qq", &[]);
    sb.add("tn two", "", "betatag", "qq", &[]);
    sb.add("gammatitle", "", "", "qq", &[]);
    sb.add("bodysix", "", "", "w1aa w2aa w3aa w4aa w5aa w6aa", &[]);
    sb.add("bodytwo", "", "", "w1aa w2aa", &[]);
    let q = "alphatrig betatag gammatitle w1aa w2aa w3aa w4aa w5aa w6aa";
    assert_eq!(sb.search_score(q, "tn-one").0, 5, "trigger x5");
    assert_eq!(sb.search_score(q, "tn-two").0, 3, "tag x3");
    assert_eq!(sb.search_score(q, "gammatitle").0, 2, "title word x2");
    assert_eq!(
        sb.search_score(q, "bodysix").0,
        4,
        "body overlap capped at 4"
    );
    assert_eq!(
        sb.search_score(q, "bodytwo").0,
        2,
        "body overlap uncapped below cap"
    );

    // CJK is tokenized per char: a Japanese title matches a Japanese prompt
    // (3 chars x2 = 6 >= min_score) and the note is injected.
    let sj = Sb::new("scorejp");
    sj.add("日本語", "", "", "JPBODY", &[]);
    assert_eq!(sj.search_score("日本語を確認", "日本語").0, 6);
    assert!(sj.inject("日本語を確認").stdout.contains("JPBODY"));
}

/// OBSERVED (README claims a Japanese trigger matches Japanese prompts):
/// a multi-character Japanese trigger scores 0, because the prompt is
/// tokenized per char but the trigger is compared whole. A single-char
/// Japanese trigger does score 5.
#[test]
fn claim_score_weights_japanese_trigger_observed() {
    let sb = Sb::new("scorejptrig");
    sb.add("jt multi", "日本語", "", "qq", &[]);
    sb.add("jt single", "日", "", "qq", &[]);
    assert_eq!(sb.search_score("日本語", "jt-multi").0, 0);
    assert_eq!(sb.search_score("日本語", "jt-single").0, 5);
}

#[test]
fn claim_selection_order_budget() {
    // always first (slug of the scored note sorts BEFORE the always note's).
    let sb = Sb::new("sel-order");
    sb.add("aaa scored", "zebraxyz", "", "SCOREDBODY", &[]);
    sb.add("zzz always", "", "", "ALWAYSBODY", &["--always"]);
    let o = sb.inject("zebraxyz");
    let a = o.stdout.find("ALWAYSBODY").expect("always injected");
    let s = o.stdout.find("SCOREDBODY").expect("scored injected");
    assert!(a < s, "always notes first: {:?}", o.stdout);

    // min_score: a title-only hit (2) is below the default 5 and not injected.
    let sb = Sb::new("sel-min");
    sb.add("gammatitle", "", "", "LOWBODY", &[]);
    assert!(!sb.inject("gammatitle").stdout.contains("LOWBODY"));
    sb.add("trigger one", "gammatitle", "", "HIGHBODY", &[]);
    let o = sb.inject("gammatitle");
    assert!(o.stdout.contains("HIGHBODY"));
    assert!(!o.stdout.contains("LOWBODY"));

    // top_k: only the 2 best scored notes.
    let sb = Sb::new("sel-topk");
    sb.write_project_config("top_k = 2\n");
    sb.add("s1", "kwone,kwtwo,kwthree", "", "BODY1", &[]);
    sb.add("s2", "kwone,kwtwo", "", "BODY2", &[]);
    sb.add("s3", "kwone", "", "BODY3", &[]);
    sb.add("s4", "kwone", "", "BODY4", &[]);
    let o = sb.inject("kwone kwtwo kwthree");
    assert!(o.stdout.contains("BODY1") && o.stdout.contains("BODY2"));
    assert!(!o.stdout.contains("BODY3") && !o.stdout.contains("BODY4"));
    assert_eq!(o.stdout.matches('●').count(), 2);

    // max_chars: a scored note that would overflow the budget is dropped.
    let big = |c: char| c.to_string().repeat(150);
    let sb = Sb::new("sel-budget");
    sb.write_project_config("max_chars = 200\n");
    sb.add("big a", "kwone,kwtwo", "", &big('A'), &[]);
    sb.add("big b", "kwone", "", &big('B'), &[]);
    let o = sb.inject("kwone kwtwo");
    assert!(o.stdout.contains(&big('A')));
    assert!(!o.stdout.contains(&big('B')), "overflowing note dropped");
    // control: with the default budget both fit.
    std::fs::remove_file(sb.proj.join("playbook.toml")).unwrap();
    let o = sb.inject("kwone kwtwo");
    assert!(o.stdout.contains(&big('A')) && o.stdout.contains(&big('B')));
}

/// OBSERVED (README: `max_chars` = "hard cap on injected characters"): it is not
/// a hard cap. `always` notes are injected in full regardless of the budget
/// (and the overrun is shed from scored notes), and the first scored note is
/// admitted even if it alone exceeds `max_chars`.
#[test]
fn claim_selection_order_budget_max_chars_not_hard_cap_observed() {
    let sb = Sb::new("sel-cap");
    sb.write_project_config("max_chars = 200\n");
    let long = "L".repeat(400);
    sb.add("alw long", "", "", &long, &["--always"]);
    sb.add("scored small", "zebraxyz", "", "SMALLBODY", &[]);
    let o = sb.inject("zebraxyz");
    assert!(o.stdout.contains(&long), "always note injected in full");
    assert!(o.stdout.chars().count() > 200, "output exceeds max_chars");
    assert!(
        !o.stdout.contains("SMALLBODY"),
        "scored note shed once always exhausted the budget"
    );

    // A second always note is also injected in full, past max_chars.
    let sb = Sb::new("sel-cap-two");
    sb.write_project_config("max_chars = 200\n");
    let long2 = "M".repeat(400);
    sb.add("alw one", "", "", &long, &["--always"]);
    sb.add("alw two", "", "", &long2, &["--always"]);
    let o = sb.inject("hello");
    assert!(o.stdout.contains(&long) && o.stdout.contains(&long2));
    assert!(o.stdout.chars().count() > 800);

    let sb = Sb::new("sel-cap-first");
    sb.write_project_config("max_chars = 200\n");
    sb.add("lone", "zebraxyz", "", &long, &[]);
    let o = sb.inject("zebraxyz");
    assert!(o.stdout.contains(&long), "first scored note always fits");
    assert!(o.stdout.chars().count() > 200);
}

#[test]
fn claim_nothing_relevant_nothing_injected() {
    let sb = Sb::new("nothing");
    sb.add("rel note", "zebraxyz", "", "RELBODY", &[]);
    let hit = sb.inject("zebraxyz");
    assert_eq!(hit.code, 0);
    assert!(hit.stdout.contains("RELBODY"));
    let miss = sb.inject("good morning friend");
    assert_eq!(miss.code, 0);
    assert_eq!(miss.stdout, "");
}

#[test]
fn claim_add_global_and_stdin() {
    let sb = Sb::new("addglobal");
    let g = sb.run(&["add", "--global", "--title", "T one", "--body", "GBODY"]);
    assert_eq!(g.code, 0, "{}", g.stderr);
    let gfile = sb.store().join("_global").join("t-one.md");
    assert!(gfile.exists(), "global note under _global: {}", g.stdout);
    assert!(std::fs::read_to_string(&gfile).unwrap().contains("GBODY"));

    let s = sb.run_in(&sb.proj, &["add", "--title", "T two"], Some("BODYX"), &[]);
    assert_eq!(s.code, 0, "{}", s.stderr);
    let pfile = sb.project_store_dir().join("t-two.md");
    let text = std::fs::read_to_string(&pfile).expect("project note written");
    assert!(text.trim_end().ends_with("BODYX"), "{text:?}");
}

#[test]
fn claim_list_shows_notes() {
    let sb = Sb::new("list");
    sb.add("Hello List Note", "", "", "body", &[]);
    let o = sb.run(&["list"]);
    assert_eq!(o.code, 0);
    assert!(
        o.stdout
            .lines()
            .any(|l| l.contains("hello-list-note") && l.contains("Hello List Note")),
        "{:?}",
        o.stdout
    );
}

#[test]
fn claim_search_check_mark() {
    let sb = Sb::new("search");
    sb.add("hit note", "zebraxyz", "", "qq", &[]);
    sb.add("low note", "", "", "qq", &[]);
    sb.add("title only", "", "", "qq", &[]);
    sb.add("always note", "", "", "qq", &["--always"]);
    let (s, c) = sb.search_score("zebraxyz", "hit-note");
    assert_eq!((s, c), (5, true));
    // unrelated non-always: unchecked, score 0
    assert_eq!(sb.search_score("zebraxyz", "low-note"), (0, false));
    // below min_score (title word only = 2): shown with score but unchecked
    assert_eq!(sb.search_score("zebraxyz title", "title-only"), (2, false));
    // always notes would be injected: checked
    assert!(sb.search_score("zebraxyz", "always-note").1);
}

#[test]
fn claim_rm_removes() {
    let sb = Sb::new("rm");
    sb.add("Doomed Note", "", "", "body", &[]);
    let file = sb.project_store_dir().join("doomed-note.md");
    assert!(file.exists());
    let o = sb.run(&["rm", "doomed-note"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(!file.exists());
    assert!(!sb.run(&["list"]).stdout.contains("doomed-note"));
}

#[test]
fn claim_status_output() {
    let sb = Sb::new("status");
    sb.add("one", "", "", "b1", &[]);
    sb.add("two", "", "", "b2", &["--global"]);
    let o = sb.run(&["status"]);
    assert_eq!(o.code, 0);
    let out = &o.stdout;
    assert!(out.contains("top_k:          3"), "{out}");
    assert!(out.contains("min_score:      5"), "{out}");
    assert!(out.contains("max_chars:      1500"), "{out}");
    let proj = format!("project store:  {}", sb.project_store_dir().display());
    assert!(out.contains(&proj), "{out}");
    let glob = format!("global store:   {}", sb.store().join("_global").display());
    assert!(out.contains(&glob), "{out}");
    assert!(out.contains("visible notes:  2"), "{out}");
}

#[test]
fn claim_init_creates() {
    let sb = Sb::new("init");
    let o = sb.run(&["init"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(sb.store().join("_global").is_dir());
    let pd = sb.project_store_dir();
    assert!(pd.is_dir());
    let notes: Vec<_> = std::fs::read_dir(&pd).unwrap().flatten().collect();
    assert_eq!(notes.len(), 1, "exactly one sample note");
    assert!(sb
        .run(&["list"])
        .stdout
        .contains("example-keep-diffs-scoped"));
}

#[test]
fn claim_note_format() {
    let sb = Sb::new("format");
    // A note written by `add` starts with the +++ fence.
    sb.add("Made By Add", "", "", "ADDBODY", &[]);
    let pd = sb.project_store_dir();
    let written = std::fs::read_to_string(pd.join("made-by-add.md")).unwrap();
    assert!(written.starts_with("+++\n"), "{written:?}");
    assert!(written.contains("title = \"Made By Add\""));

    // A hand-written note with title/tags/triggers/always is loaded.
    std::fs::write(
        pd.join("hand.md"),
        "+++\ntitle = \"Hand Written\"\ntags = [\"data\"]\ntriggers = [\"pandas\"]\nalways = true\n+++\n\nHANDBODY\n",
    )
    .unwrap();
    let l = sb.run(&["list"]);
    assert!(
        l.stdout.lines().any(|x| x.contains("hand")
            && x.contains("Hand Written")
            && x.contains("*always")
            && x.contains("pandas")),
        "{}",
        l.stdout
    );
    // always=true injects regardless of relevance.
    assert!(sb.inject("good morning friend").stdout.contains("HANDBODY"));
}

#[test]
fn claim_inject_stdin_json() {
    let sb = Sb::new("stdinjson");
    sb.add("sj note", "zebraxyz", "", "SJBODY", &[]);
    let payload = format!(
        r#"{{"cwd":{},"prompt":"zebraxyz"}}"#,
        serde_json::to_string(sb.proj.to_str().unwrap()).unwrap()
    );
    let o = sb.run_in(&sb.proj, &["inject"], Some(&payload), &[]);
    assert_eq!(o.code, 0);
    assert!(o.stdout.contains("SJBODY"), "{:?}", o.stdout);
}

#[test]
fn claim_example_toml() {
    let ex = Path::new(env!("CARGO_MANIFEST_DIR")).join("playbook.example.toml");
    assert!(ex.exists());
    let sb = Sb::new("example");
    std::fs::copy(&ex, sb.proj.join("playbook.toml")).unwrap();
    let o = sb.run(&["status"]);
    assert_eq!(o.code, 0);
    let out = &o.stdout;
    assert!(out.contains(&sb.proj.join("playbook.toml").display().to_string()));
    for want in [
        "enabled:        true",
        "top_k:          3",
        "min_score:      5",
        "max_chars:      1500",
        "include_global: true",
    ] {
        assert!(out.contains(want), "missing {want:?} in {out}");
    }
}

#[test]
fn claim_config_precedence() {
    let sb = Sb::new("prec");
    let home_cfg = sb.home.join(".playbook");
    std::fs::create_dir_all(&home_cfg).unwrap();
    std::fs::write(home_cfg.join("config.toml"), "max_chars = 900\n").unwrap();

    // Home file only: it is used.
    let o = sb.run(&["status"]);
    assert!(o.stdout.contains("max_chars:      900"), "{}", o.stdout);

    // Project file wins outright, no merging: home's max_chars is NOT applied.
    sb.write_project_config("top_k = 7\n");
    let o = sb.run(&["status"]);
    assert!(o.stdout.contains("top_k:          7"), "{}", o.stdout);
    assert!(o.stdout.contains("max_chars:      1500"), "{}", o.stdout);
}

#[test]
fn claim_config_defaults() {
    let sb = Sb::new("defaults");
    let o = sb.run(&["status"]);
    for want in [
        "enabled:        true",
        "top_k:          3",
        "min_score:      5",
        "max_chars:      1500",
        "include_global: true",
    ] {
        assert!(o.stdout.contains(want), "missing {want:?} in {}", o.stdout);
    }
    let want_global = format!(
        "global store:   {}",
        sb.home.join(".playbook/store/_global").display()
    );
    assert!(o.stdout.contains(&want_global), "{}", o.stdout);

    // include_global=true by default: a global note is injected.
    sb.add("gl", "zebraxyz", "", "GLBODY", &["--global"]);
    assert!(sb.inject("zebraxyz").stdout.contains("GLBODY"));
    // include_global=false: it is not.
    sb.write_project_config("include_global = false\n");
    assert!(!sb.inject("zebraxyz").stdout.contains("GLBODY"));
    std::fs::remove_file(sb.proj.join("playbook.toml")).unwrap();

    // always bypasses min_score (score 0 here, still injected).
    sb.add("alw", "", "", "ALWBODY", &["--always"]);
    assert!(sb.inject("good morning friend").stdout.contains("ALWBODY"));
}

#[test]
fn claim_kill_switch() {
    let sb = Sb::new("kill");
    sb.add("alw", "", "", "KILLBODY", &["--always"]);
    for v in ["1", "x"] {
        let o = sb.inject_in(&sb.proj, "hello", &[("PLAYBOOK_DISABLE", v)]);
        assert_eq!(o.code, 0);
        assert_eq!(o.stdout, "", "PLAYBOOK_DISABLE={v} must disable");
    }
    for v in ["0", ""] {
        let o = sb.inject_in(&sb.proj, "hello", &[("PLAYBOOK_DISABLE", v)]);
        assert!(
            o.stdout.contains("KILLBODY"),
            "PLAYBOOK_DISABLE={v:?} must not disable: {:?}",
            o.stdout
        );
    }
}

#[test]
fn claim_top_k_excludes_always() {
    let sb = Sb::new("topk-always");
    sb.write_project_config("top_k = 1\n");
    sb.add("zzz always", "", "", "ALWAYSBODY", &["--always"]);
    sb.add("s best", "kwone,kwtwo", "", "BESTBODY", &[]);
    sb.add("s next", "kwone", "", "NEXTBODY", &[]);
    let o = sb.inject("kwone kwtwo");
    assert_eq!(o.code, 0);
    assert!(o.stdout.contains("ALWAYSBODY"), "{:?}", o.stdout);
    assert!(o.stdout.contains("BESTBODY"), "{:?}", o.stdout);
    assert!(!o.stdout.contains("NEXTBODY"), "{:?}", o.stdout);
    assert_eq!(o.stdout.matches('●').count(), 2);
}
