# reviewgate の verdict 経路 監査（read-only、逐語引用つき）

- **対象**: `crates/reviewgate/`（`main.rs` / `review.rs` / `git.rs` / `config.rs` / `install.rs` /
  `model.rs` / `state.rs` — src 直下 7 ファイル・計 2073 行）
- **監査日**: 2026-08-04
- **監査時点**: `171062fc`（worktree `…-audit-reviewgate-verdict-paths-r2`）
- **対象バージョン**: reviewgate 0.1.18（`crates/reviewgate/Cargo.toml:4`）
- **性質**: **read-only**。`crates/reviewgate/` 配下のコード・テストは 1 行も変更していない。
  ここに記す P（permissive）項目は**すべて未是正**である。「見つけたが直していない」ことを明記する。
- **位置づけ**: `docs/audit-blastguard-verdict-paths.md` / `docs/audit-budgetguard-verdict-paths.md` と
  同じフォーマット（P/R/D の 3 分類・逐語引用・実測・棄却候補にも同じ立証責任）に従う、
  compass charter DoD9（各 gate crate の verdict 経路の per-gate 監査）の 1 本。
- **本稿は再監査（r2）である**。前回草稿 `ebd32400` を独立検証者が REFUTED した 4 点は §6 で
  逐一訂正した（未列挙サイト 1 件の追加・giveup の箇所数と警告有無の誤り・その誤りに依存した
  対比記述・行番号 3 箇所）。**前回の数字は 1 つも継承せず、すべて `grep -n` と実測で取り直した。**

分類:

- **P（permissive）** — 判定不能・subprocess 失敗・IO 失敗・空集合を「問題なし（許可）」へ潰している。
  **本監査では是正しない**（read-only）。
- **R（restrictive）** — 判定不能を制限側へ解決している。見た目が permissive でも**変更してはならない**。
- **D（deliberate）** — 意図的に許可へ解決する**宣言済みの仕様**（bounded escape hatch 等）。
  permissive とは別物。「許可するか」ではなく「許可を*決めた*か」で分ける。

---

## 0. 方法と、verdict 産出・消費サイトの全列挙

`reviewgate` は bin-only crate（`lib.rs` を持たず外部から `crate::` をリンクできない）ため、
まず手読みで**判定を産む関数と、それを消費する call site を全列挙**し、そのうえで疑わしい経路は
**リリースビルドしたバイナリ（`target/release/reviewgate`）をブラックボックスで実際に走らせて観測**した
（生成物を実行することはコード変更ではない）。判断ではなく観測に落とすため、実測は 8 本（§0.2）。

### 0.1 産出サイト / 消費サイト（全列挙）

| # | 産出サイト | 型 | 三値か | 分類 |
|---|---|---|---|---|
| A1 | `git::changed_files` (`crates/reviewgate/src/git.rs:33-59`) | `ChangeScan{NotRepo,Failed,Files}` | **是**（3値） | R（§2） |
| A2 | `git::collect` (`crates/reviewgate/src/git.rs:65-80`) | `bool`（成功/失敗） | 2値だが失敗が上流で `Failed` に写る | R（§2） |
| A3 | `harness_core::git_probe::probe_repo` の消費 (`crates/reviewgate/src/git.rs:34-42`) | `RepoProbe` 3値 | 是 | R（§2） |
| A4 | **`git::diff_text` (`crates/reviewgate/src/git.rs:99-133`)** | `DiffText{text,truncated}` | **否**（失敗を表す channel が無い） | **P1 / P2**（§1） |
| A5 | `git::run_diff` (`crates/reviewgate/src/git.rs:135-143`) | `()`（戻り値なし） | 否 | **P1 の実体**（§1） |
| A6 | `git::truncate_on_boundary` (`crates/reviewgate/src/git.rs:145-162`) | `DiffText` | — | R（§2） |
| A7 | `review::build_set` / `reviewable_files` (`crates/reviewgate/src/review.rs:57-83`) | `Option<GlobSet>` → `Vec<String>` | 否 | **P6**（§1）＋ R（§2） |
| A8 | `review::run_reviewer` (`crates/reviewgate/src/review.rs:464-509`) | `harness_core::verdict::Verdict` 3値 | 是（ただし到達しない穴あり） | **P3 / P4**（§1）＋ R（§2） |
| A9 | `review::classify` (`crates/reviewgate/src/review.rs:511-521`) | `Verdict` | 否（`Undetermined` を返す枝が無い） | **P3 / P4**（§1） |
| A10 | `review::evaluate` (`crates/reviewgate/src/review.rs:96-176`) | `Decision{Allow,Block}` | 2値 | D×4 ＋ **P1**（§1,§3） |
| A11 | `review::decide_subprocess` (`crates/reviewgate/src/review.rs:181-260`) | `Decision` | 2値 | R ＋ **P5**（§1,§2） |
| A12 | `review::decide_truncated` (`crates/reviewgate/src/review.rs:279-305`) | `Decision` | 2値 | R（§2） |
| A13 | `review::decide_scan_failed` (`crates/reviewgate/src/review.rs:317-341`) | `Decision` | 2値 | R（§2） |
| A14 | `config::Config::load` (`crates/reviewgate/src/config.rs:188-273`) | `Config` | **否**（読めた/読めなかったが同型） | **P7**（§1） |
| A15 | `config::Config::disabled_env` (`crates/reviewgate/src/config.rs:276-280`) | `bool` | 否 | R（§2） |
| A16 | `state::load`（`harness_core::gate::state:54-59`）の消費 (`crates/reviewgate/src/main.rs:169`) | `SessionState` | 否 | R（§2） |
| A17 | `main::review_command` の panic barrier (`crates/reviewgate/src/main.rs:126-128`) | `run_guarded` | 是（fail-closed） | R（§2） |
| A18 | `HookInput::parse` → `interactive` (`crates/reviewgate/src/main.rs:120-125`, `133`) | `bool` | 否 | **P8**（§1） |

| # | 消費サイト | 何に変換されるか | 備考 |
|---|---|---|---|
| B1 | `main::review_run` の `match decision` (`crates/reviewgate/src/main.rs:172-237`) | `Decision::Block` → stdout に `{"decision":"block"}` ＋ exit 0 / `Allow` → exit 0 | **Claude Code へ渡る唯一の判定** |
| B2 | main::emit_violation（実体は 248-264 行） | overwatch violation store へ 1 event | **Decision::Block の分岐からしか呼ばれない**＝ allow は fleet 統計に一切残らない: `crates/reviewgate/src/main.rs:219`「emit_violation(&root, &session, tag);」 |
| B3 | main::log_event（実体は 266-276 行。allow 側は 191 行、block 側は 218 行） | state_dir 直下の log.jsonl に 1 行（verdict タグ＋mode） | repo 内に機械的消費者は無い（grep -rn log.jsonl crates/ で reviewgate/harness-core 以外に reviewgate 由来の読み手なし）。人間が読む観測ログ: `crates/reviewgate/src/main.rs:275`「harness_core::gate::run::append_jsonl(&cfg.state_dir, &entry);」 |
| B4 | `main::status` の `match git::changed_files` (`crates/reviewgate/src/main.rs:296-323`) | 人間向け stdout | 「ゲートが設定されているか」を人間が判断する面＝**判定を持つ側**（§4-5 の実測を参照） |
| B5 | overwatch violation stream の下流 | overwatch violations CLI / benchkit::auditsample（`crates/benchkit/src/auditsample.rs:253`「ViolationSource::Reviewgate => "reviewgate",」。同ファイル 264 行は自身を the heart of the real audit source と呼び、gates が通してしまった miss を violation stream から検出する） | B2 が出ない経路は、この下流からも**永久に見えない** |

### 0.2 実測（すべて本監査で自分で観測したもの。前回草稿の数値は継承していない）

| Probe | 何を注入したか | 観測結果 | 対応項目 |
|---|---|---|---|
| A | `PATH` 先頭の fake `git` が、`diff_text()` の内容取得コマンド（引数に `--` を含むもの）**だけ**を exit 1 にする | exit 0・stdout/stderr **完全に無出力**・log `"verdict":"empty-diff"`。対照（実 git）は同じ木で `{"decision":"block"}` | P1 |
| I | 同上だが `ls-files … --` **だけ**を失敗させる（tracked diff は成功＝**部分取得**） | reviewer が受け取った prompt に `untracked.rs` の内容（`NEVER REVIEWED MARKER`）が **0 回**。対照（実 git）は 1 回。verdict `"clean"` | P2 |
| B | reviewer が実所見を**非 UTF-8** の stdout に書いて exit 0 | verdict `"clean"`（所見は消える） | P3 |
| F | reviewer が stdin を読まず・何も出力せず exit 0 | verdict `"clean"` | P4 |
| C2 | reviewer が毎回実所見を返す状態で 3 stop（`max_attempts=2`、毎回 diff を変える） | 3 回目: exit 0・stdout 空・**stderr 空**・log `"verdict":"giveup"`・violation store には `blocked-review` **2 件のみ**（giveup の event は無い） | **P5** |
| D | 対照: spawn できない reviewer で 3 stop | 3 回目: **stderr に WARNING**・log `"verdict":"reviewer-error-giveup"` | §3 の D（loud giveup） |
| E | 対照: inject mode で 3 stop | 3 回目: stderr 空・log `"verdict":"giveup"`（**C2 と同一タグ**） | §3・P5 |
| H | `include` の glob を 1 個だけ壊す (H2) / 全部壊す (H3) | H1 対照=block、**H2=無診断で `no-reviewable-changes`**、H3=全ファイル対象で block | P6・R |
| J | `reviewgate.toml` に TOML 構文エラーを 1 個入れる | 対照 J1=block、**J2=無診断で `no-reviewable-changes`**、`reviewgate status` は壊れた config を `config: …/reviewgate.toml` と**採用済みのように表示** | P7・§4 |

---

## 1. P — 見つかった permissive 経路（8 件。**すべて未是正**）

### P1. `diff_text()` の取得失敗が「空 diff ＝ 変更なし」に潰れ、**無診断で** allow する

`git.rs` は `changed_files()` について、失敗を `clean` に写さないことを module doc で宣言し、
実際に `collect()` はそれを守っている:

```rust
// crates/reviewgate/src/git.rs:65-80  collect()
fn collect(root: &Path, args: &[&str], out: &mut Vec<String>) -> bool {
    match Command::new("git").current_dir(root).args(args).output() {
        Ok(o) if o.status.success() => {
            ...
            true
        }
        // Spawn error OR non-zero exit: the sub-command did not complete
        // successfully, so its (empty) output must not be trusted as "clean".
        _ => false,
    }
}
```

**同じファイルの下半分**、実際の diff 本文を取ってくる側は、同じ失敗を一切伝播しない:

```rust
// crates/reviewgate/src/git.rs:135-143  run_diff()
fn run_diff(root: &Path, base: &[&str], files: &[String], out: &mut String) {
    let mut args: Vec<&str> = base.to_vec();
    args.extend(files.iter().map(String::as_str));
    if let Ok(o) = Command::new("git").current_dir(root).args(&args).output() {
        if o.status.success() {
            out.push_str(&String::from_utf8_lossy(&o.stdout));
        }
    }
}
```

`Err`（spawn 失敗）にも `Ok(o) if !o.status.success()`（非ゼロ終了）にも **else が無い**。
未追跡ファイルの列挙も同型（`if let Ok(o)` に else 無し、`if o.status.success()` に else 無し）:

```rust
// crates/reviewgate/src/git.rs:109-118  diff_text() 内の untracked 列挙
    if let Ok(o) = Command::new("git").current_dir(root).args(&args).output() {
        if o.status.success() {
            for line in String::from_utf8_lossy(&o.stdout).lines() {
                let line = line.trim();
                if !line.is_empty() {
                    others.push(line.to_string());
                }
            }
        }
    }
```

```rust
// crates/reviewgate/src/git.rs:119-130  未追跡ファイル本体の読み込み
    for f in others {
        s.push_str(&format!("\n=== new file: {f} ===\n"));
        if let Ok(content) = std::fs::read_to_string(root.join(&f)) {
            s.push_str(&content);
            if !s.ends_with('\n') {
                s.push('\n');
            }
        }
        if s.len() > max_bytes {
            break;
        }
    }
```

**消費側にはこの欠落を検知する手段が無い。** `DiffText` が運ぶのは `truncated: bool` だけで、
これは「サイズ超過で末尾を切った」しか表現できない:

```rust
// crates/reviewgate/src/review.rs:119-125  evaluate()
    let crate::git::DiffText {
        text: diff,
        truncated,
    } = crate::git::diff_text(root, &files, cfg.max_diff_bytes);
    if diff.trim().is_empty() {
        return allow("empty-diff", st);
    }
```

`changed_files()` が「2 ファイル変更あり」と**確定した直後**の別の git 呼び出しが落ちると、
`diff` は空文字列・`truncated` は `false` になり、**「本当に変更が無い」場合と同じ `"empty-diff"`
allow に合流する**。

#### 実測（Probe A）

`PATH` 先頭に、`changed_files()` の走査コマンドは実 git に委譲し、`diff_text()` の内容取得コマンド
（引数に `--` 区切りを含むもの）だけを exit 1 にする fake `git` を挿し、実 repo（tracked `a.rs` の
編集 1 件 ＋ untracked `untracked.rs` の新規 1 件）で `reviewgate review` を実行した:

```
=== BASELINE (real git, no shim): expect a block ===
{"decision":"block","reason":"🔍 reviewgate: 完了前に、自分の変更をコードレビューしてください (round 1/2).\n\nレビュー対象 (2 files):\n  a.rs\n  untracked.rs\n …"}
BASELINE EXIT=0

=== INJECTED (fake git fails only the content fetches) ===
INJECTED EXIT=0
（stdout / stderr ともに完全に無出力）

=== fakegit.log ===
FAKEGIT-CALL: rev-parse --is-inside-work-tree
FAKEGIT-CALL: diff --name-only
FAKEGIT-CALL: diff --cached --name-only
FAKEGIT-CALL: ls-files --others --exclude-standard
FAKEGIT-CALL: diff -- a.rs untracked.rs
FAKEGIT: injecting failure for: diff -- a.rs untracked.rs
FAKEGIT-CALL: diff --cached -- a.rs untracked.rs
FAKEGIT: injecting failure for: diff --cached -- a.rs untracked.rs
FAKEGIT-CALL: ls-files --others --exclude-standard -- a.rs untracked.rs
FAKEGIT: injecting failure for: ls-files --others --exclude-standard -- a.rs untracked.rs

=== state log.jsonl ===
{"attempt":1,"files":["a.rs","untracked.rs"],…,"verdict":"blocked-inject"}   ← baseline
{"attempt":0,"files":[],…,"verdict":"empty-diff"}                            ← injected
```

**実在する 2 ファイルの変更が、警告 1 行も無しに未レビューで通過した。**

> **前回草稿の対比記述の訂正**: 前回は「giveup 群はいずれも警告を出すのに `empty-diff` だけが
> 無警告なのが対照的」と書いていたが、これは事実ではない（§3 のとおり giveup 5 箇所のうち
> **2 箇所は無警告**）。正しい対比は「**判定不能を表現できた 3 経路
> （`git-scan-failed` / `reviewer-unavailable` / `diff-truncated`）は block ＋ 警告 ＋ 専用タグを持つのに、
> `diff_text()` の失敗だけは判定不能を表現する型を持たないため、`empty-diff` という
> *正常系の allow タグ*に合流する**」である。

### P2. **部分取得**でも失敗が伝わらず、欠落したファイルを含む変更が「レビュー済み」として certify される

P1 の同じ穴は、diff が空にならない場合により静かに効く。3 つの取得コマンドのうち 1 つだけが落ちると、
diff は**非空だが不完全**になり、`empty-diff` の分岐すら通らずに hash が記録される
（`crates/reviewgate/src/review.rs:145` `let hash = hash_diff(&diff);` → `Decision` の `last_hash` として保存され、
以後 `crates/reviewgate/src/review.rs:148-150` の `already-reviewed` がその**部分 diff** を「レビュー済み」と証明する）。

#### 実測（Probe I）

`ls-files --others --exclude-standard -- <files>` **だけ**を失敗させ、subprocess mode で
「渡された prompt をファイルに保存してから LGTM を返す」reviewer を使い、reviewer が実際に
何を受け取ったかを観測した（未追跡ファイルには `NEVER REVIEWED MARKER` を仕込んだ）:

```
=== injected: only the untracked-content fetch fails ===
EXIT=0 / stdout: [] / stderr: []
--- log.jsonl ---
{"attempt":0,"files":[],…,"verdict":"clean"}
--- did the reviewer ever see the untracked file's content? ---
0                       ← grep -c 'NEVER REVIEWED MARKER' prompt.txt
--- what the prompt DID contain (diff section) ---
--- diff ---
diff --git a/a.rs b/a.rs
…
+pub fn a() -> u8 { 2 }

=== control: same tree, real git ===
1                       ← 実 git なら marker は prompt に入る
```

`files`（レビュー対象の一覧）は `changed_files()` 由来なので**2 ファイル**を主張し、
実際に reviewer へ渡った diff は**1 ファイル分**だった。ブロック時の reason 文面も同じ `files` を使う
（subprocess_reason 経由で file_list(files) に渡る:
`crates/reviewgate/src/review.rs:250`「let reason = subprocess_reason(&files, findings, attempts, cfg.max_attempts);」）ため、
**「2 files をレビューした」と表示しながら 1 file しか見せていない**状態が起こりうる
（Probe A の baseline が `レビュー対象 (2 files)` を出力しているのが同じ経路の証拠）。

### P3. reviewer の stdout が読めない（非 UTF-8）と、`Undetermined` ではなく `Clean` に潰れる

`run_reviewer` の doc は `Undetermined` の原因を**列挙**しているが、その列挙に
「stdout の読み取り自体が失敗した」が入っていない:

```rust
// crates/reviewgate/src/review.rs:459-463
/// Returns a [`Verdict`]: `Clean` (ran, nothing to report), `Violation`
/// (findings), or `Undetermined` (the reviewer could not run to a conclusion —
/// spawn failure, non-zero exit with no output, timeout, wait error). An
/// `Undetermined` is **not** a clean review and must resolve to the blocking
/// side downstream — never Allow.
```

```rust
// crates/reviewgate/src/review.rs:491-501
        Ok(Some(status)) => {
            let mut out = String::new();
            if let Some(mut so) = child.stdout.take() {
                use std::io::Read;
                let _ = so.read_to_string(&mut out);
            }
            if !status.success() && out.trim().is_empty() {
                return Verdict::undetermined(format!("exit {:?}", status.code()));
            }
            classify(&out)
        }
```

`read_to_string` の `Result` は `let _ = …` で捨てられる。reviewer が **exit 0** かつ stdout が
有効な UTF-8 でないとき、`out` は空のまま、ガード `!status.success() && out.trim().is_empty()` は
第 1 項が偽で成立せず、`classify("")` に落ちる:

```rust
// crates/reviewgate/src/review.rs:511-521
fn classify(out: &str) -> Verdict {
    let t = out.trim();
    if t.is_empty() {
        return Verdict::from_findings(vec![]);
    }
```

`Verdict::from_findings` 自身の doc がこの使い方を名指しで禁じている:

```rust
// crates/harness-core/src/verdict.rs:218-226
    /// The sanctioned Clean-minting path for a check that **cannot itself be
    /// undetermined** (a pure, in-memory check that always runs). …
    /// A check that *can* fail to run must not call this with an empty list to
    /// stand in for "could not check" — that is the exact fail-open this module
    /// exists to prevent.
```

`classify(&out)` の `out` は fallible な subprocess IO の結果であり、その fallibility は
呼び出し前に握りつぶされている。**契約の外側で呼ばれている。**

#### 実測（Probe B）

実所見（`- high: real bug in a.rs:2 …`）を非 UTF-8 バイト列と一緒に stdout へ書いて exit 0 する
reviewer を `reviewer_cmd` に設定し、実バイナリで 1 stop:

```
=== PROBE B: non-UTF-8 reviewer stdout, exit 0, real findings ===
PROBE-B EXIT=0 / stdout: [] / stderr: []
{"attempt":0,"files":[],"mode":"subprocess",…,"verdict":"clean"}
```

**所見は消え、`"clean"` として allow された。**

### P4. 「exit 0 かつ stdout が空」＝ `Clean`（空集合 fail-open）

`classify("")` に到達する経路は P3 だけではない。**prompt の配送失敗**も同じ場所へ落ちる:

```rust
// crates/reviewgate/src/review.rs:476-487
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Verdict::undetermined(format!("spawn: {e}")),
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(prompt.as_bytes());
        // drop closes stdin so the reviewer sees EOF
    }
```

`child.stdin.take()` が `None` のときも、`write_all` が `Err`（EPIPE 等）のときも else が無い。
reviewer は diff を**受け取らないまま**走り、何も出力せず exit 0 すれば `Clean` になる。
さらに `.stderr(Stdio::null())`（`crates/reviewgate/src/review.rs:478`）により、reviewer 自身が吐いた診断も捨てられる。

**D の主張とその棄却**: `crates/reviewgate/src/review.rs:457` は `Output that is empty or starts with "LGTM" = clean.` と
**宣言している**ので、一見 D（宣言済み仕様）に見える。しかし同じ関数が reviewer へ渡す prompt は、
clean の合図を**空ではなく `LGTM` という明示的なトークン**と定めている:

```rust
// crates/reviewgate/src/review.rs:468-469（run_reviewer が組み立てる prompt）
         実在し根拠のある問題だけを、深刻度(high/med/low)付きで簡潔に箇条書きしてください。\
         該当ファイルと行が分かるよう示すこと。問題が無ければ `LGTM` とだけ出力してください。\n\n\
```

つまり**空出力は契約上の clean 合図ではない**。加えて同じ関数の隣接分岐（`crates/reviewgate/src/review.rs:497-499`）は
「exit≠0 かつ空出力」を信用せず `Undetermined` にしており、**crate 自身が空出力を
「情報が無い」と扱う判断を既に持っている**。CLAUDE.md 第3節の「エラー時に空の集合を返さない。
空集合は下流で『検査対象なし ＝ 合格』と読まれる」に真っ向から該当するため、**D ではなく P** と分類する。

#### 実測（Probe F）

stdin を読まず、何も出力せず exit 0 する reviewer（`#!/bin/bash` / `exit 0`）:

```
=== PROBE F: silent reviewer (no stdin read, no stdout, exit 0) ===
EXIT=0 / stdout: [] / stderr: []
{"attempt":0,"files":[],"mode":"subprocess",…,"verdict":"clean"}
```

**diff を一度も見ていない reviewer が `"clean"` を成立させた。**

### P5. `Verdict::Violation` の giveup が、**無診断・共有タグ・violation event 無し**で allow する

> **本項は前回草稿（`ebd32400`）に P/R/D いずれとしても列挙されていなかったサイトである。**
> 独立検証者の指摘 1（未列挙の重大サイト）への対応。

```rust
// crates/reviewgate/src/review.rs:240-249  decide_subprocess()
        Verdict::Violation(r) => {
            let findings = r.as_str();
            let attempts = prior_attempts + 1;
            if attempts > cfg.max_attempts {
                return Decision::Allow {
                    tag: "giveup",
                    attempts: 0,
                    last_hash: String::new(),
                };
            }
```

このサイトの入力は **`Undetermined` ではなく `Violation`** — すなわち「独立レビュアーが実際に
所見を報告した」という **KNOWN な違反**である。それを:

1. **警告なしで** allow する（直前に `eprintln!` が無い。`grep -n 'giveup\|eprintln!'` の結果は §3 の表）。
2. **タグ `"giveup"` は inject mode の giveup（`crates/reviewgate/src/review.rs:157`）と同一の文字列リテラル**であり、
   crate 自身が確立した「giveup は *distinct tag* で clean と区別する」という規約
   （`crates/reviewgate/src/review.rs:22-23`「then give up / loudly with a distinct tag so the turn is never permanently trapped.」、
   `crates/reviewgate/src/review.rs:312-313`「then give up *loudly* with a distinct tag so a persistently / broken git can never trap the turn.」）から外れている。
3. `emit_violation` は `Decision::Block` の分岐からしか呼ばれない（`crates/reviewgate/src/main.rs:219`）ので、
   **overwatch violation stream に何も残らない**。§0.1 B5 のとおり、この stream は
   `benchkit::auditsample` が「gates が通してしまった miss」を検出する *real audit source* であり、
   **gate が実際に通した違反が、その検出器から構造的に見えない**。

#### 実測（Probe C2 と、対照の Probe D / E）

毎回同じ実所見を返す reviewer で 3 stop（`max_attempts=2`、`already-reviewed` を避けるため毎回 diff を変更）:

```
--- round: file value 2 --- EXIT=0
stdout: [{"decision":"block","reason":"🔍 reviewgate: 独立レビュアーが変更に問題を指摘しました (round 1/2).]
stderr: []
--- round: file value 3 --- EXIT=0
stdout: [{"decision":"block","reason":"🔍 reviewgate: 独立レビュアーが変更に問題を指摘しました (round 2/2).]
stderr: []
--- round: file value 4 --- EXIT=0
stdout: []
stderr: []                                   ← 警告ゼロ
--- log.jsonl ---
{"attempt":1,…,"verdict":"blocked-review"}
{"attempt":2,…,"verdict":"blocked-review"}
{"attempt":0,…,"verdict":"giveup"}           ← 所見ありの stop を allow
--- overwatch violations ---
{"source":"reviewgate","signature":"reviewgate:blocked-review",…}
{"source":"reviewgate","signature":"reviewgate:blocked-review",…}
                                             ← giveup の event は存在しない
```

**対照 Probe D**（spawn できない reviewer ＝ `Undetermined` 側の giveup、同じ回数・同じ cap）:

```
--- round 3 --- EXIT=0
stderr: [reviewgate: WARNING reviewer still unavailable after 2 attempt(s) (spawn: No such file
         or directory (os error 2)) — allowing the stop UNREVIEWED. Fix reviewer_cmd …]
{"attempt":0,…,"verdict":"reviewer-error-giveup"}
```

**対照 Probe E**（inject mode の giveup）: `stderr: []`、`"verdict":"giveup"` — **C2 と同一タグ**。

#### 分類の根拠（D ではなく P とする理由）と、その反証への応答

**D 主張**: `crates/reviewgate/src/config.rs:45-46` は
`/// After this many consecutive review rounds in one session (the diff kept changing), give up and`
`/// allow the stop so the agent isn't trapped.` と bounded escape を宣言しており、
allow 自体は「決めた許可」である。

**棄却の理由（同じ立証責任で示す）**:

- 宣言文が根拠にしているのは **"the diff kept changing"**、つまり *inject mode の収束不全*である。
  subprocess mode で reviewer が**同じ所見を出し続けている**状況は「diff が変わり続けた」ではなく
  「**違反が解消されていない**」であり、宣言の射程に入っていない。
- 他の 3 経路（`Undetermined` / truncated / scan-failed）は、**より情報の少ない**状態
  （判定不能）であるにもかかわらず、いずれも警告＋専用タグを持つ。**最も情報が確かな
  「違反あり」だけが無診断**という非対称は、設計として宣言された跡が無い（doc も test も無い）。
  `reviewer_error_gives_up_after_max_attempts_but_never_traps`（`crates/reviewgate/src/review.rs:642-657`）等、
  他 3 経路の giveup には固定テストがあるが、**Violation giveup を固定するテストは存在しない**
  （`grep -rn 'tag, "giveup"' crates/reviewgate/` は 0 件）。
- したがって「bounded に allow すること」自体は D に近いが、**その allow が
  下流のどの channel からも観測できない**点（stderr 無し・共有タグ・violation event 無し）は
  宣言済み仕様ではなく、CLAUDE.md 第1節「沈黙は許容される degrade ではない」に該当する。
  **本監査は P と分類する**（是正の最小形は「警告を出す」「専用タグにする」であって、
  allow をやめることではない — §7）。

### P6. `include` glob の**部分的な**パース失敗が、レビュー範囲を無診断で縮める

```rust
// crates/reviewgate/src/review.rs:70-83
fn build_set(globs: &[String]) -> Option<globset::GlobSet> {
    let mut b = GlobSetBuilder::new();
    let mut any = false;
    for g in globs {
        if let Ok(glob) = Glob::new(g) {
            b.add(glob);
            any = true;
        }
    }
    if !any {
        return None;
    }
    b.build().ok()
}
```

`Glob::new(g)` が `Err` のとき、そのパターンは**黙って捨てられる**（診断なし）。
`include` の一部だけが壊れている場合、集合は「残った有効パターン」だけになり、
**operator が意図したファイルが無言でレビュー対象から外れる**。
（全部壊れて `None` になる場合は `unwrap_or(true)` で全ファイル対象＝ restrictive。§2 参照。
 **部分失敗のときだけ permissive に倒れる**という、grep では見えない非対称。）

#### 実測（Probe H）

```
=== H1: control — a valid include list that covers a.rs ===   include = ["**/*.rs", "**/*.md"]
EXIT=0 blocked=yes stderr=[]
=== H2: the SAME intent, but the .rs pattern is malformed ===  include = ["**/*.rs{", "**/*.md"]
EXIT=0 blocked=no  stderr=[]        ← a.rs の変更が無診断でレビュー対象から消える
=== H3: EVERY include pattern malformed (build_set -> None) === include = ["**/*.rs{"]
EXIT=0 blocked=yes stderr=[]        ← None は全マッチ＝ restrictive
--- log.jsonl ---
{"attempt":1,"files":["a.rs"],…,"verdict":"blocked-inject"}          ← H1
{"attempt":0,"files":[],…,"verdict":"no-reviewable-changes"}         ← H2
{"attempt":1,"files":["a.rs","reviewgate.toml"],…,"verdict":"blocked-inject"}  ← H3
```

### P7. `Config::load` の read/parse 失敗が「そう設定されていた」に写る（＋ `status` が誤報する）

```rust
// crates/reviewgate/src/config.rs:214-216
        if let Some(path) = chosen {
            if let Ok(text) = std::fs::read_to_string(&path) {
                if let Ok(fc) = toml::from_str::<FileConfig>(&text) {
```

いずれの `Err` にも else が無く、`Config::default()` がそのまま返る。
`Config` は**二値の形**（`crates/blastguard/src/model.rs:5`「Three answers, not two.」が名指しする構造）で、
「そう設定されていた」と「読めなかった／解釈できなかった」が同じ型の同じ値に写る。

**budgetguard との差（誇張しないために明記する）**: budgetguard は既定値そのものが「全許可」
（上限 0.0）だったのでゲートが**丸ごと無効化**された。reviewgate の既定は `enabled: true` で
armed のままなので、**丸ごと無効にはならない**。失われるのは**operator が上乗せした厳格さ**である
（`include` の拡張・`mode = "subprocess"` の独立レビュアー・`min_changed_files` の引き下げ等）。
それでも「測れなかったものを測れたことにしている」構造は同一であり、budgetguard 監査が
同じ構造を P1/P2 として是正した先例に合わせて **P** と分類する
（設定ファイル**不在**は KNOWN な答えなので D。§3）。

#### 実測（Probe J）

```
=== J1: valid config that deliberately widens include to .md ===   include = ["**/*.md"]
EXIT=0 blocked=yes stderr=[]
=== J2: the SAME config with one TOML syntax error ===             include = ["**/*.md"   ← 閉じ括弧欠落
EXIT=0 blocked=no  stderr=[]        ← DESIGN.md の変更が無診断で未レビュー通過
=== J3: what does 'reviewgate status' report about the broken config? ===
config:        …/reviewgate.toml    ← 壊れた config を「採用中」と表示する
enabled:       true
mode:          inject               ← 実際には 1 項目も適用されていない
changed:       2 file(s), 0 reviewable
(below min_changed_files=1 — stop would be allowed)
```

`status` の config 行は `Config::project_path(&root).exists()`（`crates/reviewgate/src/main.rs:281`）で決まり、
**パースできたかどうかを見ていない**。operator が「設定は効いている」と読む面で誤報する
（§4-5 に doc/報告と実挙動の食い違いとして再掲）。

### P8. hook payload が parse 不能だと `interactive` と誤判定し、**block が block として届かない**

```rust
// crates/reviewgate/src/main.rs:119-125
fn review_command() -> ! {
    let raw = read_stdin();
    let hook = HookInput::parse(&raw);
    let interactive = hook.is_none();
```

`HookInput::parse` は空文字列・非 JSON・（`MAX_STDIN_BYTES` = 10 MiB 超過による切り詰め等で）
壊れた JSON に対して `None` を返す（`crates/harness-core/src/hook.rs:96-102`）。
`None` ＝「hook ではなく手動 CLI 実行」と解釈され、`Decision::Block` は
stdout の `{"decision":"block"}` ではなく **stderr ＋ exit 1** になる（`crates/reviewgate/src/main.rs:220-228`）。

#### 実測（Probe G）

```
=== PROBE G: unparseable hook payload + a blocking decision ===
EXIT=1
stdout: []                                    ← decision JSON が出ない
stderr: [🔍 reviewgate: 完了前に、自分の変更をコードレビューしてください (round 1/2) …]
=== control: the SAME tree with a well-formed payload ===
EXIT=0
stdout: [{"decision":"block","reason":"🔍 reviewgate: 完了前に…"}]
```

**観測できたこと**: 同じ木・同じ判定なのに、payload が壊れているだけで
「block の宣言（stdout JSON）」が消え exit 1 になる。これは同ファイルの docstring と矛盾する
（`crates/reviewgate/src/main.rs:111-112`「Always exits 0 toward Claude (the `decision` field, not the exit code, is what
blocks a stop). Returns exit 1 only in manual CLI mode.」— §4-1）。

**観測できていないこと**: Claude Code 本体が Stop hook の exit 1 をどう扱うか。本監査に
Claude Code 本体を制御して観測する手段は無い。**判定不能なので棄却せず P として残す**
（CLAUDE.md 第3節・第6節。「到達しないはず」は予測であって観測ではない）。
なお `HookInput::parse` / `read_stdin` は `harness-core` の共有コードで、他 gate も同じ経路を使うため、
是正はこの crate 単独では閉じない可能性がある。

---

## 2. R — 判定不能を制限側へ解決している経路（**変更してはならない**）

見た目が permissive（`unwrap_or(true)` / `unwrap_or(false)` / `.ok()` / `_ => false`）でも、
帰結を追うとゲートを**強める**側に倒れているもの。grep ベースの一括修正で誤って「直される」のを防ぐ。

| 位置 | 逐語 | なぜ restrictive か |
|---|---|---|
| `crates/reviewgate/src/git.rs:41` | `RepoProbe::Undetermined => return ChangeScan::Failed,` | 「git が答えられない（かつ `.git` は在る）」を no-scope ではなく undetermined に写す。`harness_core::git_probe` の三値を正しく消費 |
| `crates/reviewgate/src/git.rs:46-55` | `if !ok { return ChangeScan::Failed; }` | 3 つの走査コマンドのいずれか 1 つでも失敗したら undetermined。**終了ステータスを判定に使っている** |
| `crates/reviewgate/src/git.rs:78` | `_ => false,`（`collect`） | spawn 失敗・非ゼロ終了の空 stdout を「clean」と読まない |
| `crates/reviewgate/src/git.rs:127-129` | `if s.len() > max_bytes { break; }` | 打ち切りは `truncate_on_boundary` で `truncated: true` になり block へ（§3 D の bounded 経路） |
| `crates/reviewgate/src/review.rs:63` |「.unwrap_or(true)」＝ include 側の inc.as_ref().map(...).unwrap_or(true) | include 集合が構築できない＝**全ファイルをレビュー対象にする**方向（Probe H3 で実測） |
| `crates/reviewgate/src/review.rs:64` |「.unwrap_or(false)」＝ exclude 側の !exc.as_ref().map(...).unwrap_or(false) | exclude が構築できない＝**何も除外しない**方向 |
| `crates/reviewgate/src/review.rs:105-109` / `128-132` | `if now() - st.last_ts > cfg.reset_after_secs { 0 } else { st.attempts }` | idle 後に attempts を 0 に戻す＝ giveup までの猶予が**増える**＝ block 側 |
| `crates/reviewgate/src/review.rs:141-143` | `if truncated { return decide_truncated(...); }` | **hash short-circuit より前**に置かれている。`already-reviewed` が切り詰め diff を certify するのを防ぐ（コメント `crates/reviewgate/src/review.rs:139-140` が理由を明記） |
| `crates/reviewgate/src/review.rs:201-234` | `Verdict::Undetermined(r) => …` → `Decision::Block` | reviewer crash / timeout / 非ゼロ終了かつ空出力を clean と誤認しない。**P3/P4 はこの分岐に*到達しない*ケースがあることを指摘している**（分岐自体は正しい） |
| `crates/reviewgate/src/review.rs:227-233` / `295-304` / `332-340` | `last_hash: String::new(),` | 判定不能・未レビュー末尾・undetermined change set に hash を**記録しない**＝ `already-reviewed` に certify させない |
| `crates/reviewgate/src/review.rs:279-305` | `decide_truncated` → `Decision::Block`（giveup まで） | 切り詰めた末尾を未レビューのまま通さない |
| `crates/reviewgate/src/review.rs:317-341` | `decide_scan_failed` → `Decision::Block`（giveup まで） | `ChangeScan::Failed` を block へ |
| `crates/reviewgate/src/config.rs:193-204` | project `reviewgate.toml` が untrusted なら**無視して** home/defaults にフォールバック（`eprintln!` あり） | `reviewer_cmd` が Stop hook から subprocess 実行されるため、untrusted な repo 同梱値の実行は RCE。**警告を出す**点でも silent ではない |
| `crates/reviewgate/src/config.rs:260-271` | sanitize floor 4 件（`max_attempts==0→1` 等） | 0 を放置するとゲートが無意味な極端側（毎回即 giveup 等）に振れる。**floor はゲートを弱めていない** |
| `crates/reviewgate/src/config.rs:276-280` | `.map(\|v\| !v.is_empty() && v != "0").unwrap_or(false)` | 未設定・読み取り失敗で `false`＝「無効化**されていない**」＝ armed のまま |
| `crates/reviewgate/src/main.rs:126-128` | `harness_core::gate::run::run_guarded("reviewgate", …)` | panic は **fail closed（block）**、`stop_hook_active` のときだけ bounded に allow（`crates/harness-core/src/gate/run.rs:85-99`） |
| `crates/reviewgate/src/main.rs:169` |「let prior = state::load(&cfg.state_dir, &session);」＝ 実体は `crates/harness-core/src/gate/state.rs:54-59` の read_to_string(...).ok().and_then(...).unwrap_or_default() | state が壊れている＝ attempts:0 / last_hash 空＝「まだレビューしていない」＝ block 側 |

---

## 3. D — 意図的に許可へ解決する宣言済み仕様（permissive ではない）

### 3.1 giveup サイトは **5 箇所**（4 箇所ではない）。警告があるのは **3 箇所だけ**

機械的な確認（本監査で実行。前回草稿の「4 箇所」「いずれも警告あり」はこれと一致しない）:

```
$ grep -n 'giveup\|eprintln!' crates/reviewgate/src/review.rs
157:                    tag: "giveup",
205:                eprintln!(
213:                    tag: "reviewer-error-giveup",
218:            eprintln!(
245:                    tag: "giveup",
282:        eprintln!(
290:            tag: "truncated-giveup",
320:        eprintln!(
327:            tag: "git-scan-failed-giveup",
（以下 538 行目以降は #[cfg(test)] 内）
```

| # | giveup サイト | tag | **直前の警告** | 発生条件 | 分類 |
|---|---|---|---|---|---|
| G1 | `crates/reviewgate/src/review.rs:155-161` | `"giveup"` | **無し** | inject mode で `attempts > max_attempts` | **D**（§3.2） |
| G2 | `crates/reviewgate/src/review.rs:204-217` | `"reviewer-error-giveup"` | **有り**（`:205`） | `Verdict::Undetermined` | D |
| G3 | `crates/reviewgate/src/review.rs:243-249` | `"giveup"`（G1 と同一） | **無し** | `Verdict::Violation`（所見あり） | **P5**（§1） |
| G4 | `crates/reviewgate/src/review.rs:281-294` | `"truncated-giveup"` | **有り**（`:282`） | diff 切り詰め | D |
| G5 | `crates/reviewgate/src/review.rs:319-331` | `"git-scan-failed-giveup"` | **有り**（`:320`） | `ChangeScan::Failed` | D |

- `:218` の `eprintln!` は **giveup ではなく block 経路**の警告（`Undetermined` で block するとき）。
  giveup 直前の警告と混同しないこと。
- hook モードでは `crates/reviewgate/src/main.rs:192-194` の `println!("reviewgate: allow ({tag})")` は
  `if interactive` の内側なので出ない。**G1/G3 は hook モードで完全に無音**（Probe C2/E で実測）。
- G1 と G3 は**同じ文字列リテラル `"giveup"`**。JSONL 行には別フィールド `"mode"` があるので
  ログ上は mode で区別しうるが、crate 自身が掲げる「giveup は *distinct tag*」という規約
  （`crates/reviewgate/src/review.rs:22-23`, `312-313`）は満たしていない。overwatch の signature は
  `reviewgate:<tag>` 形式だが、**giveup は violation event を出さない**ので signature 衝突は起きない
  （＝下流から見えないことの裏返し）。

### 3.2 D 一覧

| 位置 | 挙動 | 宣言箇所 / 固定テスト |
|---|---|---|
| git repo でない | `ChangeScan::NotRepo` → `allow("no-git", st)`（`crates/reviewgate/src/review.rs:98-99`） | `crates/reviewgate/src/git.rs:1-10` module doc。`non_repo_dir_is_notrepo`（`crates/reviewgate/src/git.rs:229-234`）、`genuine_non_repo_with_real_git_still_allows` / `unspawnable_git_without_a_dot_git_still_allows`（`tests/git_probe_wiring.rs:141,168`） |
| レビュー対象が `min_changed_files` 未満 | `allow("no-reviewable-changes", st)`（`crates/reviewgate/src/review.rs:115-117`） | operator が `include`/`exclude`/`min_changed_files` で制御する宣言済みスコープ |
| 同一 diff hash の再 stop | `allow("already-reviewed", st)`（`crates/reviewgate/src/review.rs:148-150`） | `crates/reviewgate/src/review.rs:9-11` module doc（convergence）。無限 block を防ぐ核 |
| **G1** inject giveup | bounded に allow（無音・タグ `"giveup"`） | `crates/reviewgate/src/config.rs:45-46`「give up and allow the stop so the agent isn't trapped」。**inject mode では gate 自身が verdict を持たない**（レビューするのは agent 自身）ので、*捨てられた既知の違反は存在しない* — この点が G3 と決定的に異なる |
| **G2 / G4 / G5** | bounded に allow ＋ 警告 ＋ 専用タグ | 各 `decide_*` の doc / 分岐コメント（`crates/reviewgate/src/review.rs:178-180` と `188-200`, `270-278`, `307-316`）。`reviewer_error_gives_up_after_max_attempts_but_never_traps`（`:642`）/ `truncated_diff_gives_up_after_max_attempts_but_never_traps`（`:726`）/ `failed_git_scan_gives_up_after_max_attempts_but_never_traps`（`:782`）が固定 |
| `.reviewgate-skip` | 消費されたら allow（`crates/reviewgate/src/main.rs:157-167`、`eprintln!` あり） | `harness_core::gate::run::consume_skip`。**marker ファイルの物理的作成という operator の明示的行為**が前提。読めなくても `"(no reason given)"` で消費するが、`p.exists()` が偽なら消費しない（restrictive 側） |
| `REVIEWGATE_DISABLE=1` | 即 allow / exit 0（`crates/reviewgate/src/main.rs:137-143`） | `Config::disabled_env`。panic guard の**外側**ではなく `review_run` 内だが、config 読み込みより前に評価され常に到達可能 |
| config `enabled = false` | 即 allow / exit 0（`crates/reviewgate/src/main.rs:146-152`） | operator の明示的意思 |
| 設定ファイルが**存在しない** | `Config::default()`（armed） | `crates/reviewgate/src/config.rs:1-6`「Safe by default: … Installing the hook can never *trap* a turn on its own.」**不在は判定不能ではなく KNOWN な答え**。budgetguard 監査 §1 の carve-out と同じ線引き（**存在するのに読めない/解釈できない**場合だけが P7） |
| `classify` の `LGTM` 前方一致 | `Clean`（`crates/reviewgate/src/review.rs:516-519`） | `crates/reviewgate/src/review.rs:457`「Output that is empty or starts with "LGTM" = clean.」＋ prompt（469 行）で reviewer に指示済み。classify_lgtm_is_clean（571 行）が固定 |
| stdout が空 ＝ `Clean` | — | **D 主張を §1 P4 で棄却した**（prompt は `LGTM` を要求しており、空は契約上の clean 合図ではない） |

---

## 4. 散文・報告と実挙動の食い違い（CLAUDE.md 第4節）

本監査は read-only なので**是正していない**。検出のみを記録する。

1. **`crates/reviewgate/src/main.rs:111-112`**:
   `/// The Stop hook. Always exits 0 toward Claude (the `decision` field, not the exit code, is`
   `/// what blocks a stop). Returns exit 1 only in manual CLI mode.`
   — **Probe G で反証**。hook 実行でも payload が parse 不能なら exit 1 になり、`decision` フィールドは
   出力されない。「manual CLI mode でのみ」は偽。
2. **`crates/reviewgate/src/main.rs:12-14`**:
   `//!   * a *harness* error (bad config, no git, our own bug) → exit 0, allow the`
   `//!     stop. We must never trap a turn because reviewgate itself broke.`
   — 同じファイルの `crates/reviewgate/src/main.rs:113-118` が「panic は hook モードで **fail CLOSED**（block を出す）」と
   説明しており、**同一ファイル内で自己矛盾**している（budgetguard 監査 §5 と同型）。
   加えて「our own bug → allow」は、`ChangeScan::Failed` が block する現在の設計とも整合しない。
   `never trap a turn` は CLAUDE.md 第1節が「verdict 経路の docstring に書いた時点で赤信号」と
   名指しする語であり、この crate では `crates/reviewgate/src/review.rs:198`, `278`, `315` にも現れる
   （そこでは bounded giveup の正当化に使われており、G2/G4/G5 は実際に警告付き＝宣言どおり。
   一方 G1/G3 は同じ語の傘の下にありながら無音である点が、この語の危うさを示す実例になっている）。
3. **`crates/reviewgate/src/git.rs:94-98`**（`diff_text` の doc）:
   `/// … the`
   `/// returned `truncated` flag lets the caller refuse to silently allow a stop`
   `/// whose tail was dropped.`
   — `truncated` が覆うのは**サイズ超過による欠落だけ**であり、取得コマンド失敗による欠落
   （P1/P2）は同じ「silently allow」を招くのに flag に現れない。doc は「caller は refuse できる」と
   読ませるが、refuse する材料が渡っていない。
4. **`crates/reviewgate/src/review.rs:459-463`**（`run_reviewer` の doc）: `Undetermined` の原因を
   `spawn failure, non-zero exit with no output, timeout, wait error` と**列挙**しているが、
   実際には stdout 読み取り失敗（P3）・prompt 配送失敗（P4）が抜けており、それらは `Clean` に落ちる。
5. **`reviewgate status` の `config:` 行**（`crates/reviewgate/src/main.rs:281-287`）: パース成否を見ずに
   `exists()` だけで「採用中の config」を表示する。Probe J3 のとおり、**1 項目も適用されていない
   壊れた TOML を「採用中」と報告**する。operator はこの出力で「設定は効いている」と読む。

---

## 5. 検討したが確証を得られなかった／棄却した候補（棄却にも同じ立証責任）

CLAUDE.md 第6節に従い、**「経路を辿れなかった」を「経路が無い」と書かない**。
棄却できていないものは UNVERIFIED として残す。

| 候補 | 状態 | 根拠 |
|---|---|---|
| `classify` の `first.to_ascii_lowercase().starts_with("lgtm")`（`crates/reviewgate/src/review.rs:517`）が、"lgtm" で始まる**実所見**を Clean と誤分類しうる | **RECORDED, not asserted** | 前方一致であり `lgtm, but: high severity …` のような出力は Clean になる。prompt（`:469`）は「問題が無ければ `LGTM` **とだけ**」と指示しているので契約違反の出力ではあるが、LLM reviewer が前置きに "LGTM overall, but…" と書く実務的確率は無視できない。**実測していないので P に格上げしない**。是正するなら完全一致にすべき、という指摘のみ記録 |
| `hash_diff` の `DefaultHasher`（`crates/reviewgate/src/review.rs:85-89`）の衝突・std 更新による不安定性 | **棄却（restrictive 方向）** | 衝突すれば `already-reviewed` で誤 allow だが 64bit SipHash の偶発衝突は無視可能。std 更新でハッシュが変われば**過去の hash と一致しなくなる**＝もう一度 block する側に倒れる。`hash_is_stable_and_distinct`（`:582`）が同一プロセス内の安定性を固定 |
| `state::save` / `append_jsonl` の書き込み失敗（`crates/reviewgate/src/main.rs:181-190`, `209-217`, `275`） | **棄却（restrictive 方向）** | `last_hash` を保存できなければ次の stop で `already-reviewed` が成立せず**再度 block**する。attempts を保存できなければ giveup までの猶予が増える |
| `crates/reviewgate/src/main.rs:279`「std::env::current_dir().unwrap_or_else(」＝ 失敗時に Path::new(".") へ落ちる | **判定経路ではない（が §4-5 で別の欠陥あり）** | 下流消費者を列挙した: status は --json を持たず、grep -rn 'reviewgate status' に機械的消費者は無い（唯一の消費者は端末の人間）。ただし**人間が「ゲートは armed か」を判断する面**なので免責は狭い。`current_dir` 失敗そのものより、§4-5 の config 誤報のほうが実害が大きい |
| `crates/reviewgate/src/install.rs:17-22` `dirs::home_dir().unwrap_or_else(\|\| PathBuf::from("."))` / `current_exe().ok()…unwrap_or_else(\|\| "reviewgate")` | **設置経路。本監査のスコープ外として記録** | home 解決に失敗すると `./.claude/settings.json` に書いて `Installed Stop hook` と**成功を報告**する（ゲートが設置されない fleet 規模の fail-open）。budgetguard 監査 §7・backlog `1e783882` と**同一クラス**。verdict 経路ではないので P に含めない |
| P8（`interactive` 誤判定）の**下流**、すなわち Claude Code が Stop hook の exit 1 をどう扱うか | **UNVERIFIED（棄却しない）** | 本監査に Claude Code 本体を観測する手段が無い。観測できたのは「hook 実行でも block JSON が出ず exit 1 になる」ことと、それが docstring と矛盾すること（§4-1）まで。**判定不能を「問題なし」に写さない**ため P8 は開いたまま残す |
| P1/P2 の**現場での発生頻度** | **UNVERIFIED** | 「git は普通失敗しない」は予測であって観測ではない。頻度は測っていない。ただし `changed_files` 側は同じ失敗を fail-closed 扱いすると既に決めており（`crates/reviewgate/src/git.rs:16-21`）、**同一 crate 内で頻度評価が矛盾している**ことは指摘できる |

---

## 6. 前回草稿（`ebd32400`）からの訂正 — 独立検証者の指摘 4 点への対応

| # | 指摘 | 本稿での対応 |
|---|---|---|
| 1 | `crates/reviewgate/src/review.rs:240-249` の **Violation-giveup** が P/R/D のどこにも列挙されていない | **§1 P5 として独立に列挙**し、逐語引用・実測（Probe C2、対照 D/E）・D 主張の明示的棄却つきで **P** に分類した |
| 2 | D 表の「giveup は 4 箇所、いずれも直前に無条件 `eprintln!` 警告」が事実と違う | **§3.1 で `grep -n` の生出力を貼り、giveup は 5 箇所・警告があるのは 3 箇所（G2/G4/G5）だけ**であることを表で示した。`:218` は giveup ではなく block 経路の警告である点も明記。G1/G3 が hook モードで無音であることは Probe C2/E で実測 |
| 3 | 上記の誤りに依存していた P1 の対比記述（「empty-diff だけが無警告」） | **§1 P1 末尾に訂正ブロックを置き**、正しい対比（「判定不能を*表現できた* 3 経路は block＋警告＋専用タグ、`diff_text` の失敗だけは表現する型が無く正常系 allow タグに合流する」）へ差し替えた |
| 4 | 行番号の誤り 3 件 | (a) `ls-files --others` は `crates/reviewgate/src/git.rs:119-126` ではなく **`crates/reviewgate/src/git.rs:109-118`**（`if let Ok(o)` が 109、対応する閉じ括弧が 118。ファイル内容の読み込みは別に `crates/reviewgate/src/git.rs:119-130`）。(b) `Ok(Some(status))` アームは `crates/reviewgate/src/review.rs:490-500` ではなく **`crates/reviewgate/src/review.rs:491-501`**。(c) `evaluate()` の `DiffText` destructure は**4 行のまま**逐語引用した（§1 P1、`crates/reviewgate/src/review.rs:119-125`）。本稿の行番号は全件 `sed -n` で再確認済み |

加えて、前回草稿が「未確証」として表に留めていた 2 件（config フォールバックによる厳格さの後退／
`interactive` 誤判定）は、**実測（Probe J / Probe G）を行って P7・P8 に格上げ**した。
前回 P1・P2 の中身（`diff_text` の握りつぶし、reviewer stdout の read 失敗）は正しかったので維持し、
本稿ではそれぞれ P1（＋新たに部分取得の P2）・P3 として、**自分で取り直した実測**とともに記載している。

---

## 7. 結論

**reviewgate の verdict 経路監査: permissive 8 件を検出（すべて未是正）／ restrictive 17 件を
「変更してはならない」として明記／ deliberate 11 件を仕様として分離／散文・報告の食い違い 5 件を検出。**

| ID | 一言 | 実測 | 重大度の目安 |
|---|---|---|---|
| P1 | `diff_text()` の失敗 → 空 diff → 無診断 allow | Probe A | 高（実在の変更が丸ごと未レビュー通過） |
| P2 | 同上の**部分**失敗 → 欠落したまま「レビュー済み」certify | Probe I | 高（欠落が hash で確定される） |
| P3 | reviewer stdout の read 失敗（非 UTF-8）→ `Clean` | Probe B | 中（所見が消える） |
| P4 | exit 0 かつ空 stdout → `Clean`（空集合 fail-open、stderr は `/dev/null`） | Probe F | 中 |
| P5 | `Violation` の giveup が無音・共有タグ・violation event 無し | Probe C2（対照 D/E） | 中（既知の違反が下流から不可視） |
| P6 | `include` glob の部分パース失敗がレビュー範囲を無診断で縮める | Probe H | 低〜中 |
| P7 | config の read/parse 失敗が「そう設定されていた」に写る＋`status` が誤報 | Probe J | 低〜中 |
| P8 | payload parse 不能 → `interactive` 誤判定 → block が届かない | Probe G（下流は UNVERIFIED） | 未確定 |

最も一般化する所見は、この crate に固有ではない:

> **同じファイル・同じ関数群の中で、片方の経路だけが三値化から取り残される。**
> `changed_files()` は `ChangeScan{NotRepo, Failed, Files}` という三値を持ち、subprocess の
> 終了ステータスを判定に使い、失敗を block へ倒している。その **数十行下**の `diff_text()` は、
> 同じ `git` を同じ `Command` で呼びながら、失敗を表現する型を一切持たない。
> `run_reviewer` も同様で、spawn/timeout/非ゼロ終了は `Undetermined` に写すのに、
> **stdout を読む段**だけが `let _ =` で握りつぶされている。
> これは `docs/audit-blastguard-verdict-paths.md` が名指しした **mirror gap**（直近で塞いだ穴の
> 双子を必ず監査する）と同型であり、「三値型を導入した」ことは
> **その型が全経路に届いた**ことを意味しない。

是正の方向性（実装は本タスクの範囲外。read-only）:

- **P1/P2**: `run_diff` と untracked 取得を成功フラグ付きに変え、`DiffText` に「取得コマンドが失敗した」
  を表す channel を足し、`evaluate()` から `decide_scan_failed` と同型の bounded fail-closed 経路へ回す。
- **P3/P4**: `read_to_string` の `Err` を `Verdict::undetermined("stdout not valid utf-8")` に写し、
  `write_all` の `Err`／`stdin.take() == None` も同様に写す。「exit 0 かつ空出力」は
  `Undetermined`（prompt が要求する `LGTM` が来ていない）に倒す。
- **P5**: giveup 直前に G2/G4/G5 と同形の `eprintln!` 警告を出し、タグを `"review-giveup"` 等の
  専用値にし、`emit_violation` を allow 側の giveup でも呼ぶか、少なくとも別 signature で記録する。
  **allow をやめる必要は無い**（bounded escape 自体は D）。
- **P6/P7**: 壊れた glob／壊れた config を**黙って捨てない**（警告 ＋ `status` に「parse 失敗」を表示）。
- **P8**: hook 実行かどうかを payload の parse 成否で推定せず、別の signal（stdin が piped か等）で
  決める。harness-core 側の共有経路なので他 gate と併せて設計する必要がある。

DoD9 の per-gate 監査に reviewgate を 1 本追加できる状態になったが、**本監査は是正を伴わない**ため、
「fixed」側には計上しない — **発見のみが記録された状態**である。
