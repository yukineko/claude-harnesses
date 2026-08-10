# blastguard

> 🌐 [English](README.md) ・ **日本語**

**プロジェクトを破壊しかねない操作を実行前に止める PreToolUse ガード**

## 目的

blastguard は Claude Code の **PreToolUse** フックである。エージェントが実行しようと
しているツール呼び出しを stdin から受け取り、純粋関数で **allow / ask / deny** の
三値を判定し、**allow 以外のときに** PreToolUse の JSON を出力して、その操作を実行前に
止める（`deny` は握り潰し、`ask` は人間に判断を渡す）。

判定対象は `Bash` / `Edit` / `Write` / `MultiEdit` / `NotebookEdit` の各ツール。
止めるのは「明らかに破壊的で、取り消しが難しい」操作に限られる。

- **Bash コマンド**: 再帰 `rm`（`rm -rf dir` など）、ワイルドカード `rm`（`rm *`,
  `rm path/*`）、`git clean -fdx` / `-fd`、`git reset --hard`、作業ツリー破棄
  （`git checkout -- .`, `git checkout --force`）、上書きリダイレクト（単一の
  `>`）、ファイル切り詰め / 抹消（`truncate -s0`, `shred`）、ファイルシステム /
  デバイス書き込み（`mkfs.*`, `dd of=/dev/sda`）、再帰的なパーミッション / 所有者
  変更（`chmod -R 777 .`, `chown -R root .`）、`find` 経由の一括削除
  （`find . -delete`, `find . -exec rm …`）、fork bomb。
- **ファイル操作**: 既存ファイルを**空内容で置き換える** Write（＝ファイルの抹消）、
  および **git 内部**（`.git/**`）を上書きする Write は deny。Edit / MultiEdit /
  NotebookEdit は部分編集なので常に allow。

通常作業の邪魔をしないよう、明確に無害な形は通す — 非再帰の `rm file.txt`、追記
（`>>`）、fd リダイレクト（`2>&1`, `>&2`）、`/dev/null` 等への切り詰めリダイレクト
はいずれも allow である。

**ただし「曖昧なものは allow」ではない。** この段落はかつて「設計は意図的に保守的で
あり、曖昧なものはすべて allow に倒す」と書いていたが、それは `Ask` 導入前の二値時代の
記述であり、実挙動と正反対になっていた。解析できない構文を allow に倒す設計こそ
`model.rs` が名指しで廃した fail-open であり（CLAUDE.md 3 の模範例として引用されている）、
現在は `Decision::Ask` に解決する — コマンドについての判定ではなく、判定を推測することの
拒否である。散文が実挙動より安全側に見える話を語るのは CLAUDE.md 4 が禁じる罠なので、
ここに実挙動を書く。

なお `ask` は**人間が答えられる場合にのみ**出力される。headless / agent 主導の
セッションでは答える人がいないため `Decision::hardened` で `deny` へ強化される
（allow へは倒さない）。

介入が実際に何を防いだかは `blastguard retro` で事後検証できる（本 README 末尾の
「事後検証」節を参照）。

さらに、リポジトリの**設定ファイル**に対する編集 / 削除は、形が破壊的に見えても
`.claude/**`、`**/package.json`、`**/*.toml` / `*.yaml` / `*.yml` / `*.lock`、
`.config/**` などは除外（allow）する。**ただし `.claude/settings.json` /
`.claude/settings.local.json` / `.claude/hooks.json` / `.claude/hooks/**` /
`.githooks/**` など、どのゲート・フックが動くか自体を決めるファイルはこの除外の
**対象外**であり、常に deny になる（守護者自身を無効化する経路を塞ぐため、
この一群は設定ファイル除外より優先される）。

## どうして必要か

エージェントによるコーディングでは、`rm -rf`・`git reset --hard`・`git clean -fdx`・
単一 `>` での上書きといった一手が、コミット前の作業や巨大なディレクトリを一瞬で消し
飛ばす。これらは取り返しがつかず、しかもツール呼び出しの中に紛れて流れてくるため、
人間が毎回目視で止めるのは現実的でない。

blastguard はこの「破壊的だが不可逆な少数のパターン」だけを実行前に遮断する安全網に
徹する。判定は純粋関数で決定論的に行われる: **入力が空**、または**対象外のツール**
（＝判定対象が無いと確定できたケース）は黙って allow（exit 0、出力なし）だが、
**検出器内部で panic が起きた場合は判定不能であり、allow ではなく deny に解決する**
（`crates/blastguard/src/main.rs` の `analyse` が `std::panic::catch_unwind` で panic を
捕捉し `Decision::deny(INTERNAL_ERROR_REASON)` を返す）。これは
`Decision::{Allow, Deny, Ask}` の三値設計（`src/model.rs`）を保つための挙動であり、
「判定できなかった」を「安全である」に丸め込まない。プロセス自体（stdin 読み取り・
JSON 出力）がクラッシュしないことは `harness_core::hook::run_hook` が保証するが、
これは判定を持たない外側の backstop であり、上記の deny-on-panic とは別レイヤーである。
広く構えすぎて通常作業を妨げるより、明確に危険なものだけを確実に止めることを優先
している。

**「入力が不正」はこの黙って allow の側ではない**（2026-08-02 の是正。この節は以前
「入力が空 / 不正 … は黙って allow」と書いていた）。stdin が**非空なのに読めない**、
または**判定対象のツールなのに operand（`tool_input.command` / `file_path`）が読めない**
のは、「判定対象が無いと確定できた」ではなく「**判定できなかった**」である。前者と
後者を同じ出力に写すと、blastguard は自分が「検査した」のか「検査できなかった」のかを
下流から区別不能にする。したがってこの2つは `Decision::Ask`（＝
`src/model.rs:9` の言う *コマンドについての判定ではなく、判定を推測することの拒否*）を
返し、人間が答えられない環境では `hardened()` が deny へ倒す。**空 stdin と対象外の
ツールだけが黙って allow のまま**であり、これは意図された permissive 仕様である
（対象外ツールまで巻き込むと Read / Grep のたびに prompt が出る）。

## 既知の high-frequency deny（backlog ba72dc46 / cd99fa2c の判定記録）

overwatch の continuous-audit 再発トラッカー（`overwatch violations --json`）が
`blastguard:truncating-redirect`（21回発火・4セッション）と
`blastguard:code-interpreter-inline-eval`（17回発火・5セッション）を systemic
（高頻度再発）として検出した。**大半は調査の結果、誤検知ではなく意図通りの
設計と判定し、コードは変更していない**（2026-07-23 判定）。ただし
truncating-redirect については、この判定と並行して**別種の純粋なバグを1件発見し
修正した**（下記参照、v0.2.11）。

- **truncating-redirect**: `crates/blastguard/src/detect.rs:373-379` は
  `/dev/null` / `/dev/stdout` / `/dev/stderr` / 認識済み設定ファイル以外への
  すべての `>` 系切り詰めリダイレクトを deny する。実際の発火ログ
  （`overwatch/violations.jsonl`）を見ると、原因の大半はエージェントが
  scratchpad ディレクトリ配下（例:
  `/tmp/claude-.../scratchpad/rid.txt`）へ出力を書き出そうとした一手である。
  一見「scratchpad なら安全では」と思えるが、同ファイル `:398-413`（D1 コメント）に
  この exact な例外を過去に実装し、`..` を含むパス（`/tmp/../etc/hosts` 等）が
  `exclude::normalize` では解決されないため prefix チェックをすり抜け、**リダイレクト
  ルール全体を無効化するバイパス**になっていたと記録されている。既に一度
  実装して撤去された安全なはずの近道であり、再導入しない。回避策は `>>`
  （追記）・`2>&1` 等の fd 複製・`/dev/null` を使うこと。
  - **v0.2.11 で修正した別件のバグ**: 上記の意図的トレードオフとは別に、
    `$(cmd 2>/dev/null)` のような command substitution 内の redirect が
    誤って deny されるケースを本セッション自身で観測した（この session の
    `> /dev/null)` 発火）。原因はターゲットトークン抽出（`redirect_targets`,
    `:580-589`）が `)` を区切り文字として扱っておらず、command substitution を
    閉じる `)` がターゲット文字列に混入して `/dev/null)` という別物になり、
    `redirect_target_is_safe` の完全一致に失敗していたこと。これは D1 の
    トレードオフとは無関係の純粋なトークナイズ漏れと判断し、`)` を区切り文字に
    追加して修正した（`cargo test -p blastguard` green）。
- **code-interpreter-inline-eval**: `:948-1009`, `:1572-1576` は
  `python`/`perl`/`ruby`/`node`/`php`/`lua`（バージョン付きも含む）が
  `-c`/`-e`/`-r`/`-p`/`--eval`/`--print` 等の inline-eval フラグと共に呼ばれた
  場合を無条件に deny する。ペイロードの中身（読み取り専用か破壊的か）は見ない。
  これは意図的: inline のコード片が実際に安全かどうかを構文的に判定するには
  ペイロードの意味解析が要るが、`os.system`/`subprocess`/`exec`/難読化された
  呼び出し等でいくらでも迂回できるため、「見た目安全」ヒューリスティックは
  上記 truncating-redirect の D1 と同種の脆弱な近道になる。**スクリプトファイル
  経由の実行は禁止されていない** — `is_inline_eval_flag` は `-c` 等のフラグにのみ
  マッチし、`python3 script.py`（フラグ無しのスクリプト実行）は対象外
  （`:986-987` のコメント参照）。複雑な処理が必要なら Write ツールでスクリプトを
  書き出してから引数無しで実行すればよく、コードも可視化される。
  - **v0.2.12 で追記した具体例**: この判定の妥当性を、本セッション自身で観測した
    実発火例で裏付ける。次の `python3 -c` 呼び出しは、transcript ファイルを開いて
    `json.loads` でパースし、マッチした行番号を `print` するだけの read-only スクリプト
    だったが、`is_inline_eval_flag`（`:1007-1020`）はペイロードの中身を見ないため
    無条件に deny された:
    ```
    python3 -c "
    import json
    lines = open('....jsonl').readlines()
    for i, l in enumerate(lines):
        if 'inline-eval flag can run' in l:
            ...
            print(i, tool_use_id)
    "
    ```
    一見「read-only スクリプトまで一律ブロックする過検知」に見えるが、構文だけから
    「本当に read-only か」を判定する信頼できる方法は無い —
    `import os; os.system(...)` や `eval(...)`、難読化された文字列結合を同じ `-c`
    引数内に混在させれば、静的な字面判定は容易に迂回できる。したがってこの一手も
    truncating-redirect の D1 と同種の「意図的な false positive の許容」と判断し、
    コードは変更しない。回避策は本セッションでも実際に採った通り、`jq`/`grep` 等の
    専用ツールを使うか、Write でスクリプトファイルを書き出して引数無しで実行すること。

## ライブラリとしての再利用

`src/lib.rs` は同じ検出ロジックを他クレートへも公開している（純粋関数・I/O なし）:
specguard の forge は LLM が生成した `test_cmd` を `sh -c` に渡す前に
`detect::detect` で検証し、condukt のスケジューラは段階的な
`classify::classify`（risk / reversibility 判定）を使って、上流の LLM が
誤ラベル付けした場合でも deploy・`git push`・release のような対外的で不可逆な
アクションを GATED ゲートへ強制的に通す。

コマンド分類とは別に、**diff の内容そのもの**から意味的リスクを見る2つのモジュールも
公開している。いずれも純粋・決定論・I/O なしで、command 分類が拾えない「無害な
コマンドで危険な差分を書く」ケースを埋める:

- **`diffrisk`**（`src/diffrisk.rs`）— 変更されたファイルパスがセンシティブ領域
  （auth/認証・payment/決済・PII 等、`DEFAULT_SENSITIVE_GLOBS` に列挙）に触れているか、
  および公開 API シグネチャ（`pub fn`/`pub struct` 等）を変更しているかを、diff の
  unified diff テキストから直接判定する（`classify_diff` / `changes_public_symbol`）。
  結果は command classification と同じ `crate::classify::RiskAssessment` に載るので、
  呼び出し側は2つの軸を別々に扱わず1本の risk として扱える。
- **`callgraph`**（`src/callgraph.rs`）— 変更された symbol を diff から抽出し
  （`changed_symbol_names`）、その symbol を実際に参照している呼び出し箇所を
  ソースコーパス全体から字句レベルで列挙する（`enumerate_callers`）。パーサ・正規表現・
  外部 API 不使用の純粋な文字列トークンスキャンで、どんな入力に対しても panic しない
  （壊れた/中途半端なソースに対する fail-soft floor）。

**condukt 側の呼び出し経路**: `crates/condukt/src/diffrisk_record.rs` が
`blastguard::diffrisk::{classify_diff, classify_diff_with_callers, SensitiveConfig}` と
`blastguard::callgraph::{changed_symbol_names, enumerate_callers}` を呼び出し、
コンパイル済みの caller コーパスと突き合わせて `(needs_review, high_risk)` 相当の
判定を1つの `ViolationSource::Blastguard` イベントとして `overwatch` へ記録する
（`crates/condukt/src/schedule.rs` / `gate_exec.rs` / `review_brief.rs` もこの経路を
消費し、diff の blast-radius をスケジューリング・レビューブリーフ生成に反映する）。

## どう使うか

プラグインとして導入すれば、追加の起動操作は不要。slash command は持たず、
**フックとして自動配線**される。`hooks/hooks.json` が PreToolUse に登録されており、
`Bash|Edit|Write|MultiEdit|NotebookEdit` にマッチした呼び出しのたびに
`${CLAUDE_PLUGIN_ROOT}/bin/blastguard` が起動する。

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash|Edit|Write|MultiEdit|NotebookEdit",
        "hooks": [
          { "type": "command", "command": "${CLAUDE_PLUGIN_ROOT}/bin/blastguard", "timeout": 10 }
        ]
      }
    ]
  }
}
```

`bin/blastguard` はホストの OS / アーキテクチャに合う `blastguard-<os>-<arch>` を
exec する POSIX-sh ランチャである。**該当プラットフォーム向けの同梱ビルドが無い場合は
stderr に警告を出しつつ exit 0 する**（`crates/blastguard/bin/blastguard` 参照） —
これは analyser 内部の「判定不能は deny」とは異なるレイヤーの、既知の fail-open
ギャップである。バイナリが存在しない環境では検出器そのものが起動しないため、判定を
下す主体が居ない。この経路は起動前のプラットフォーム欠落のみに限られ、起動後の
判定不能（内部 panic 等）は上記の通り deny に解決される。API キーは不要で、**1 フック
+ 同梱バイナリだけで完結する subscription-native** な構成である。

CLI 表面は最小限で、stdin を触る前に `--version` / `-V` と `--help` / `-h` のみ
短絡する。

ビルド / テスト:

```sh
cargo build --release -p blastguard   # -> target/release/blastguard
make bins                             # 各プラットフォーム向け同梱バイナリを更新
cargo test -p blastguard              # ユニット + 統合テスト
```

## 事後検証（`blastguard retro`）

ゲート自身のログは「ゲートが何を言ったか」しか答えない。ゲートが割に合っているかを
決めるのは次の問いである — **止めた操作は本当に起きなかったのか、それとも人間が
承認して結局実行されたのか**。承認は violation store に痕跡を残さないので、この失敗
モードは従来まったく見えなかった。

`blastguard retro` は Claude Code の transcript 内の PreToolUse 判定と、同じ
`toolUseID` の `tool_result` を突き合わせてこれに答える。

```sh
blastguard retro                              # cwd から対象プロジェクトを推定
blastguard retro --project /path/to/repo
blastguard retro --dir ~/.claude/projects/-path-to-repo
```

介入ごとに三値の outcome が付く — `executed-anyway`（人間が yes と答えた＝その回
ゲートは何も阻止していない）／ `not-executed` ／ `unknown`。**`tool_result` が無い
ことを阻止として数えない**: 途中で放棄された turn や切り詰められた transcript が、
そのままゲートの見かけ上の価値を水増しするため。

PreToolUse JSON を出さず非ゼロ終了で止める script ゲート（`guard-maintree-bash.py`
等）も解析対象に含む。母集団を blastguard 専用にすると「止めたゲート」の比較が
成立しないため。

transcript を1件も読めなかった場合は空の表ではなく `UNDETERMINED` を出して **exit 2**
にする。「測れなかった」が「問題なし」として描画されるのは、このクレート自体が消す
ために存在する fail-open そのものである。

**主張しないこと**: 承認は「その回このゲートが何も阻止しなかった」ことだけを立て、
止めたのが誤りだったことは立てない。また、言い換えて再実行された迂回は検出できない
ので、阻止件数は**上限**である。この2つの但し書きは数字と同じ出力に必ず同梱される。

### `retro` で 0.2.44→0.2.49 の効果は測れない（測り方も併記する）

`retro` は**履歴**を読む — transcript に既に書かれた判定である。コードを変えたあとに
`retro` を再実行しても、変更について何も測っていない。判定アームを変えた効果を見るには、
**記録されたコマンドを新しいバイナリに通し直す**必要がある。

その際 `--list` の出力を入力に使ってはならない。`--list` は command の空白列を squeeze
するが、**改行はこの resolver にとって segment 区切り**である。`BIN=x` 改行 `cd "$SB"` が
`BIN=x cd "$SB"` になると、それは同一 segment の前置代入という**別の構文**になり、
定義上ちがう答えになる。生のコマンドを transcript から `toolUseID` で取り直すこと
（`tool_use` ブロックは hook の attachment より**前**に書かれるので、単一の前方走査では
join できない。ファイルごとに2パスする）。

**実測**（測定日 2026-08-07、測定点 `e306331c`、手順は上記のとおり再判定）:

| | 件数 |
|---|---|
| 記録されていた `unresolvable-command-word` 介入（生コマンドが取れたもの） | 115 |
| 0.2.49 で解決した（この ask を出さなくなった） | **49**（うち 45 は Allow、4 は別の restrictive な ask へ） |
| 依然として同じ理由で ask | 66 |
| **Deny へ移った** | **0** |

Deny が 0 件であることは弱い保証ではあるが事実である — このコーパスに、展開の陰に
隠れた破壊的コマンドは1件も無かった。

事前見積りは 70 だったが、それは「`&&`/`||` より前に literal 代入がある」という
**粗い正規表現**の値であり、出荷した resolver はそれより厳しい。差 21 の内訳:
`export`/`declare` 経由 10（0.2.48 の第二著者監査が塞いだ stale-literal 経路なので
**正しく拒否している**）、`"$D/bin/tool"` のように参照へテキストが貼り付いた head 9
（残る機会。backlog `2fb05132`）、前置代入 1、条件付き 1。
**見積りの数字を実測として転記しない** — この節が置かれている理由がそれである。
