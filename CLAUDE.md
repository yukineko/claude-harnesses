# claude-harnesses — リポジトリ指針

`yukineko` の Claude Code ハーネス一家を単一ソースで管理する Cargo ワークスペース・モノレポ。
各 `crates/<name>/` は Rust クレートかつ Claude Code プラグイン（`.claude-plugin/plugin.json` +
`hooks/` + 同梱 `bin/` + `skills/`）。共通基盤は `crates/harness-core`（ビルド時ライブラリ。各
プラグインバイナリに静的に焼き込まれる）。

## 最上位の方針 — 失敗は検出であり、価値である（**他のすべてに優先する**）

このリポジトリのゲート群が存在する理由そのもの。**迷ったらこの節に従う。**

### 1. fail は罰ではない。検出であり、価値がある

テストが赤い・ゲートがブロックする・チェックが非0で終わる — これらは**成果**であって事故ではない。
問題を出荷前に見つけたということは、ゲートが仕事をしたということ。**赤を「邪魔」と捉えた瞬間に、
このリポジトリの投資はすべて無駄になる。**

- ゲートが自分の変更を止めたら、まずゲートが正しい可能性を疑う。ゲートの側を疑うのはその後。

**「ターンを壊すな」を判定の正当化に使ってはならない（撤去する *方針*。実体は一部のみ移行済み）。**
このリポジトリには「never break the turn ＝ ターンを壊すな」という**不変条件**があり、
判定を持つコードにも適用されていた。結果として Stop ゲートの panic barrier は panic を「許可」に
写しており、**panic やエラーを「許可」に写す実装がこの一文で正当化されていた** — 当時のコードは
`docs/article-llm-fail-open.md:48` に「swallow it and exit 0 (allow the stop; a hook must never break the user's turn)」として引用が残る。

**移行済みの実体**: 4 ゲート（donegate / reviewgate / propguard / tdd の各 `main.rs`）が通る
panic barrier `crates/harness-core/src/gate/run.rs:36` は `d6db4670` で fail-closed へ移行した。
gate 本体が panic した＝判定不能は block に解決する（「**fail closed**: block the stop and surface the crash」）。
連続 2 回目の panic だけが `stop_hook_active` により bounded に allow へ落ちる。

**未移行の実体（この文書のこの版を書いた時点で生きている。方針だけ先行している）**:
`docs/stop-gate-latency.md:41` が**同じ規範を MUST として再主張している**（「A gate that errors internally allows the stop (exit 0)」）。
移行は backlog `13dba04c`。**この節と `docs/stop-gate-latency.md` は現在矛盾している** —
移行完了までは、両者の食い違いを承知の上で新規コードにこの節を適用する。
（`crates/harness-core/src/hook.rs:218` の `run_hook` も常に exit 0 だが、これは判定を持たない
observability hook 専用の入口であり、下の carve-out の側に属する。）したがって:

- **判定を持つコード（ゲート・チェック・verdict を返すもの）に「ターンを壊すな」は適用されない。**
  ブロックはターンを壊す行為ではない。判定不能なら 3. に従って制限側へ倒す。それだけ。
- 残るのは**判定を持たないコード**に対する、遥かに狭い要求だけ:
  **「判定しないものは、失敗しても判定したふりをしない」**。ただし**「判定を持つ」は返り値の型ではなく
  消費のされ方で決まる** — 下流（人間・スクリプト）が沈黙・空・既定値を「問題なし」と読める出力は、
  型に関わらず判定を持つ。**沈黙は許容される degrade ではない**（`3b1eb24`: statusline の空表示は
  「余裕あり」と読まれる fail-open だった。だから statusline は判定を持つ側であり、
  `unknown` と明示する）。判定を持たないのは、消費者が存在しないか、欠落が UI 上で明示的に
  unknown と表示される場合だけ。**この分類は自己申告で免責を得る道具になりうる**ので、
  免責を主張するモジュールは下流消費者を列挙すること。その出力が別の判定の入力になった時点で免責は失効する。
- **この語を verdict 経路の docstring・コメント・コミットメッセージに書いた時点で赤信号**とみなす。
  レビューで見つけたら、その実装は fail-open を疑って読むこと。

### 2. 判断は予測にすぎない。fail はテストで決着させる

**判断（judgment）は予測であって事実ではない。** 「たぶん大丈夫」「おそらくこの経路は通らない」
「意図としてはこう」は、どれも**未検証の予測**である。**事実とは観測されたもの**だけを指す。
できうる限り事実と観測をベースにし、判断で代用しない。

fail が出たとき、**判断で片付けない**。次の順で必ず事実に落とす:

1. **その挙動を突いたテストが既にあるか確認する。**
2. **無ければ作る。** 「明らかだから不要」は判断＝予測なので理由にならない。
3. **実行する。** 結果を読む。
4. **テストが通れば、それは事実として認定し ok とする。** 通らなければ ok にしない。
   判断で ok にするのは禁止（3. の「判定不能」と同じ扱い）。

**疑わしいときも同じ手順**を踏む。「疑わしいが多分平気」は最も高くつく判断である。

#### テストが書けない問題は、判断で進めず **ask する**

上の手順で唯一の例外は「**テストが書けない**」ケースであり、その出口は**判断ではなく人間への質問**である。
「テストできないから仕方なく自分の判断で ok にした」は**この規範の最大の抜け穴**なので、明示的に塞ぐ:

- テストが書けない・書いても意味のある観測にならない・環境的に実行できない — **そう判明した時点で
  作業を止め、`AskUserQuestion` で人間に投げる**（何が測れないのか、なぜ測れないのかを添えて）。
- 「テスト不能なので判断で通した」は**禁止**。判断は予測であり、予測で ok を出すのは 4. の隠蔽に接続する。
- このゲートは **auto で自答してはならない**（`condukt policy answer` に掛けるなら escalate 相当。
  自律モードでも人間に残す停止として扱う）。測れないという事実こそ人間が知るべき情報である。

#### この規範が成立するための 2 条件（どちらも必須）

「テストが通れば事実」は無条件には成り立たない。**何も検証していないテストは常に通る**からである
（実例・**現在も未修正**: `crates/condukt/src/verify.rs:1326` の `checks_verdict` は空スライスに
`true` を返し、`:2871` の `assert!(checks_verdict(&[]))` がそれを*正しいものとして固定*している。
docstring は後に「An empty slice is a vacuous pass」と**追認する方向で更新された** — 空集合 fail-open が
仕様として書き下された形であり、3. の「空集合を返さない」に真っ向から反する。backlog `100af807`）。
したがって:

- **(a) テストは利害関係のない Agent が書く。** 実装した本人（人間・LLM を問わず）は
  「自分の実装が通ること」に利害があるので、**自分の実装を検証するテストを自分で書かない**。
  別の Agent に書かせる（`condukt-verifier` / 独立 subagent / continuous-audit の finder≠verifier）。
  Continuous-Audit が **finder と verifier のモデル多様性を MUST** としているのと同じ理由 —
  生成と検証が盲点を共有すると、検証は儀式になる。
- **(b) 落ちることを確認していないテストは、何も証明しない。** 先に RED を観測してから GREEN にする。
  `tdd` クレートの **F→P オラクル**（`condukt state check-oracle`）がこの証跡を機械的に要求するのは
  このため。RED を見ていない GREEN は「テストが通った」ではなく「テストが何も見ていない」かもしれない。

### 3. 判定不能（cannot determine）は必ず制限側に解決する

**IO 失敗・ロック取得失敗・subprocess の異常終了・パース不能・panic・空集合**を「問題なし」に
潰してはならない。「わからない」は「大丈夫」ではない。

- `Result`/`Option` を bool へ潰すとき、`unwrap_or` の既定値は**必ず制限側**（`deny`/`block`/`true`=違反あり）にする。
- エラー時に**空の集合**を返さない。空集合は下流で「検査対象なし ＝ 合格」と読まれる。
  三値（`Absent` / `Undetermined` / `Known(T)`）で「判定できなかった」を**表現可能**にする。
  **注意: この共有型はまだ存在しない**（`grep -rn Undetermined crates/harness-core/src/` = 0 件）。
  各 crate が三値を個別に再発明している状態で、型による強制は未実装。backlog `42b7c9af`。
- subprocess は `stdout` だけでなく**必ず終了ステータスを判定に使う**。落ちたチェッカは合格したチェッカではない。
- 閾値の sanitize は**ゲートを無効化しない範囲に clamp** する（床なし clamp は「常に合格」を意味する）。

> **測定値（再導出可能）**: `git log --all -i --grep='fail.open\|fail.closed'` = **45 件**
> （`silently` まで含む広いパターンなら 126 件だが、無関係な commit と `feat` が混ざるため
> 「このクラスの修正」の根拠には使えない）。2026-07-21 の 1 日で 14 件が landed した。
> そのうち少なくとも 3 件（`3b1eb24` / `c066fc8` / `05df9b2`）は「安全な degrade だ」と
> *明示的に正当化するコメント*を伴っていた（`231e20e` はコメント無しだったので「全件」ではない）。
> **バグというより、判断が ok 側に倒れた結果**である。だからこの規範を明文で置く。

### 4. 間違いの隠蔽は、このリポジトリで最もやってはならないこと

**誤りそのものより、誤りを見えなくする行為の方が重い。** 以下は明確に禁止する:

- ゲート・チェック・テストを、**通すためだけに**緩める / 無効化する / skip する。
  （不要になったから消す、は可。**赤いから黙らせる、は不可。**）
- 失敗した検査を「環境のせい」「fail-soft だから」と**未検証のまま**通過させる。
- エラーを握りつぶして成功に見せる（`|| true`、`.ok()`、`unwrap_or(false)` による判定の消去）。
- **docstring / コメントで実挙動と違う「安全な話」を書く。** 散文が実装と食い違ったら、
  それは次のレビュアーを騙す仕掛けになる。実装を変えたら**同じコミットで**記述も直す。
- 動かなかった・確かめていないことを、動いた・確かめたかのように報告する。
  **やっていないことは「やっていない」と書く。** テストが落ちたなら出力とともにそう言う。

### 5. どうしても通せないときの正しい振る舞い

黙って迂回せず、**見えるところに残す**:

1. まず**本当に直す**ことを試みる。
2. 直せないなら**理由を明示して報告**し、判断を人間に返す（`backlog add` で残す・escalate する）。
3. skip 機構（`.donegate-skip` 等）は**理由を書いて一度だけ**。恒常的な迂回に使わない。
   **並行セッションがあるときは project root の共有 skip ファイルを使わない**
   （一度だけ消費されるため、他セッションの正当なゲートを素通りさせる）。自セッションに閉じた
   環境変数を使う。

### 6. 実装者は「大丈夫だろう」に倒れる — それを**前提**に設計する

**前提（矯正対象ではなく、与件として扱う）**: 実装を書く主体は — 人間も、とりわけ LLM も —
**「これは大丈夫だろう」「そういうことにしておく」という自己許可**に倒れる。悪意ではなく、
permissive 側が「何も壊れないように見える」からである。**注意喚起や善意で矯正できるものとして
扱わない。倒れる前提で、検出側を厚くする。**

- 対策を「気をつける」に置かない。**決定論的な機械判定**（型・コンパイルエラー・kill-rate・
  フォールト注入・単調性プロパティ）と**敵対的レビュー**（生成と検証を**別 Agent・別モデル**に分離）
  に置く。Continuous-Audit が finder と verifier のモデル多様性を MUST とするのはこの理由。
- **反証にも同じ立証責任を課す（敵対的レビュー自身の fail-open）。**
  「permissive な経路を**辿れなかった**」は「経路が**無い**」ではない。前者は**判定不能**であり、
  3. の規範がそのまま適用される — 棄却するなら「なぜ到達しないか」を、発見と同じ水準の
  逐語引用と経路の追跡で示すこと。示せないなら verdict は **REFUTED ではなく UNVERIFIED** とし、
  項目は残す。**「default は REFUTED」という指示は、検証者を棄却側に倒す**（実測: 2026-07-21 の
  監査で `specguard/forge/gather.rs` の実在する fail-open が、検証者が消費経路を 1 本しか辿らなかった
  ために誤って棄却された。件数閾値を満たす部分的な束が clean として下流へ渡る経路を見落としていた）。
  **敵対的レビューを入れれば安全、ではない。**
- **批判もまた検査対象である（非対称性の禁止）。** 自分の発見には懐疑的で、**自分への批判は
  無検証で受け入れる**という非対称は、それ自体が「そういうことにしておく」である。
  批判を受け入れる行為は**厳密さの衣装を着ている**ぶん見逃されやすい。**反証・指摘・棄却も、
  発見と同じ立証責任で検査してから採用する**（実測: 2026-07-21、敵対的検証者の 4 件の
  `REFUTED` を無検証で採用して backlog を降ろしたが、うち 1 件は実在する欠陥だった。
  残り 3 件は今も未検証のまま）。「誰かが最後に動いた方を信じる」も判断であって観測ではない。
- 規範の遵守を**実装者の自己申告に依存させない**。「レビューしました」「問題ありません」は観測ではない。

#### LLM の職務（この順で回す）

1. **論理的矛盾を指摘する** — 仕様↔実装、**docstring↔実挙動**、テスト↔done_criteria、
   コメント↔コードの食い違い。矛盾は最も安く検出できる欠陥である。
2. **矛盾が無くなるまで判断を求める** — 曖昧なまま先に進まない。解消の主体は人間でよい。
3. **テストを書く** — 判断を観測に落とす（2. と 5. を参照）。
4. **事実を積み重ねる** — 観測されたものだけを事実として蓄積する。推測は事実の棚に置かない。
5. **事実の範囲内でのみ検討・実装する** — 事実を超える部分が出たら 1./2. に戻る。埋めない。

#### Ask は成果である — 恐れる必要はない

**「わからない」「測れない」「矛盾している」と表明して人間に返すことは、失敗でも能力不足でもなく、
このリポジトリで最も価値のある出力の一つである。** 遠慮せずに ask してよい。

- **質問を避けて自分の判断で埋める行為が、まさに「そういうことにしておく」**であり、
  123 コミット分の fail-open を生んだ当のものである。
- Ask のコストは人間の数秒。黙って判断で埋めたコストは、**誰も気づかないまま残る fail-open**。
  非対称であり、ask 側が常に安い。
- したがって「ask したから仕事が半端」ではない。**測れないという事実を可視化したこと自体が成果**である。

## context 読み込み戦略（重要 — 盲目的に crate を探索しない）

このリポジトリは 39 クレートある。全体を毎回読むと context を浪費するので、**必要な層だけを
オンデマンドで**読む:

1. **まず [`docs/GLOSSARY.md`](docs/GLOSSARY.md) を読む** — 全クレートの一言早見表＋頻出ドメイン用語
   （harness / hook / SKILL / worktree / gate / source↔executor / PDO / HOTL / autonomy gate /
   fail-soft など）。「どの crate が何をするか」はここで解決する。1 枚（約 80 行）で全体像がつかめる。
2. **特定 crate を触るときだけ** その `crates/<name>/README.ja.md`（詳細）と `src/` を読む。
   GLOSSARY で当たりをつけてから深掘りする。
3. 横断テーマは `docs/` の該当ファイルへ（下記「さらに読む」）。
4. **重い探索・横断検索は sub-agent 経由**にして main context を汚さない。

## ビルド / テスト / ゲート

- ツールチェーンは **rustup 経由**。cargo コマンドの前に `. "$HOME/.cargo/env"` を通す。
- テストはクレート単位: `cargo test -p <crate>`。
- CI ゲートは **fmt + clippy を強制**する。コミット前に `cargo fmt` と
  `cargo clippy -p <crate> --all-targets` を green にする。
- **prompt-injection 防御ゲート（2本）** — prompt に load される資産と同梱バイナリの改竄を機械検出する:
  - `python3 scripts/check-prompt-injection.py`（injectguard）— skills/agents/hooks/CLAUDE.md/.compass/docs
    に隠蔽・検証バイパス・egress 文言が植わっていないか走査（防御 framing は除外）。ローカルは
    `git config core.hooksPath .githooks` で pre-commit を有効化（速い advisory 層。CI の `injectguard` job が
    非バイパスの本ゲート）。
  - `python3 scripts/check-bin-reproducibility.py`（CI `bin-reproducibility` job）— 全 bin をソースから再ビルドし、
    committed-only な悪性パターン文字列（source が生成しない焼き込み）を検出。生の committed-only 件数・size 差は
    ビルド非決定性なので**判定に使わない**（悪性デルタのみ）。host triple のみ対象。
- **Continuous-Audit 自動起動導線** — `git config core.hooksPath .githooks` を有効化していれば
  `.githooks/pre-push` が GATE_CRATES（blastguard/propguard/specguard/stuckguard/mutategate/overwatch）配下の
  変更を検知し、`scripts/continuous-audit.sh --dry-run` を勧める advisory メッセージを出す
  （pre-commit と同じ fail-soft 設計。push は絶対に止めない・常に exit 0）。cron 定期実行の雛形は
  `scripts/continuous-audit.cron.example` を参照。

## プラグインを改修したときの反映（忘れやすい）

`crates/<name>/` が唯一の正典。`/plugin install` はここを
`~/.claude/plugins/cache/<owner>/<name>/<version>/` に**プレーンコピー**する（git 外）。稼働中の
ハーネスはキャッシュ側を読むので、**repo をビルドしただけでは何も反映されない**:

### 「plugin 更新（`/plugin update` 相当）」を求められたら — `scripts/rollout-plugins.sh` を使う（正典・一発）

**version を上げた／テキスト資産を変えたプラグインを稼働ハーネスへ反映するときは、手動 `cp` ではなく
必ず `scripts/rollout-plugins.sh` を実行する。** このスクリプトは `/plugin update`（UI 専用操作）が
directory marketplace `yukineko` に対してやる2操作 —(1) `crates/<name>/` を新しい
`cache/yukineko/<name>/<version>/` **dir へコピー**、(2) `installed_plugins.json` の
`<name>@yukineko` を新 dir へ **repoint** — を再現し、続けて `rebuild-plugins.sh`（バイナリ swap）と
各 plugin の `sync-plugin-assets.sh`（skills/agents/hooks 同期）まで一括で走らせる。順序も保証する
（version dir + registry pointer を rebuild より先に作る）。

```sh
python3 scripts/check-plugin-versions.py && python3 scripts/check-version-bumped.py   # 先に version 整合を確認
scripts/rollout-plugins.sh --plugin <name> --dry-run   # 動作を確認（何も書かない）
scripts/rollout-plugins.sh --plugin <name>             # 実反映（全 plugin なら無引数）

# GATE クレート（防御ゲート: blastguard / propguard / specguard / stuckguard / overwatch、
# 非プラグインの mutategate も含む）は fleet を守るため、canary 無しの反映を
# **拒否**する（Problem-2.3）。GATE クレートを対象にするときは --canary で段階
# 反映し、各ステージ間で健全性ゲート（raw-spike または systemic recurrence の
# いずれかで rollback）を通す。--plugin 無しの全 plugin 反映も GATE を含むため
# --canary が必須（明示的に外すときのみ --no-canary）。
scripts/rollout-plugins.sh --plugin specguard --canary --dry-run   # GATE クレート: 段階反映を確認
scripts/rollout-plugins.sh --plugin specguard --canary             # GATE クレート: canary 経由で反映
```

- 冪等（version 不変なら no-op）。`--force` で無変更でも再コピー、`--no-rebuild`/`--no-sync` で段階分け。
- **GATE クレートは canary 必須**: canary 無しでの反映はエラーで止まる（fleet 防御ゲートを
  無検証で入れ替えないための安全策）。`--no-canary` は明示的な例外指定としてのみ用いる。
- **手動 `cp` は禁止**: cache の binary/asset を手で上書きするだけだと **version dir が旧名のまま残り**
  registry も更新されず、`sync-plugin-assets.sh` が version から dir を誤解決して**古い版が配布される**。
- `/plugin update`（ユーザー UI 操作）は本スクリプトで完全代替できるので、手動 UI 操作は不要。

低レベルの構成要素（通常は上の rollout 経由で十分。個別に叩くのは段階分けのときだけ）:

- バイナリを反映: `scripts/rebuild-plugins.sh`（`--no-clean` で増分）— target のバイナリを live
  キャッシュへ swap する（**既存 dir に swap するだけ。version dir は作らない**）。
- テキスト資産（skills/agents/hooks）を反映: `crates/<name>/scripts/sync-plugin-assets.sh`
  （`--check` で drift 検出）。
- **キャッシュを手編集しない**（git 外で黙って乖離する）。必ず repo を編集 → 上記で同期。

## バージョン整合（**絶対厳守** — 「今動けばいい」は後で壊れる）

各プラグインの version は **3つの正典で常に lockstep** でなければならない。片方だけ上げた状態は
**バグ**として扱う（「今は動く」で放置しない）。ズレると sync-plugin-assets.sh が version から
キャッシュ dir を誤解決し、**古い版がユーザーに配布される**。

### 変更したら必ず version を上げる（**禁忌ルール**）

- **あるプラグインの中身（コード・hooks・skills・agents 等いずれか）を1行でも触ったら、その
  プラグインの version を最低でも micro（patch, `x.y.Z` の Z）上げる。** 触ったのに据え置きは
  **禁忌**。
- version を上げるときは **必ず3ファイル同時**: `crates/<name>/Cargo.toml` の `[package].version`
  ／ `crates/<name>/.claude-plugin/plugin.json` の `version` ／ `.claude-plugin/marketplace.json`
  の当該エントリ `version`（skill-only プラグインは Cargo.toml が無いので後者2つ）。
- 言い換え: **「何か変更したのに plugin version と marketplace version が変わっていない」状態を
  commit してはならない。** 変更の大小を問わず、最低 micro を必ず上げる（意味的に大きければ
  minor/major で判断）。
- これは「今動けばいい」を禁じ、後の drift・古い版配布を根絶するための**徹底順守ルール**。

**強制ゲート（3つとも、commit 前・push 前・CI で回す）:**

```sh
python3 scripts/check-plugin-versions.py            # lockstep: 3ファイルの version が一致するか（exit 1 で drift）
python3 scripts/check-version-bumped.py             # bump-on-change: base(既定 HEAD)から変更のある plugin が bump 済みか
python3 scripts/check-version-bumped.py --base origin/main   # CI/push前は pushed ref と比較
python3 scripts/check-plugin-rollout.py             # rollout drift: source version が installed_plugins.json の registry version へ実際に反映(rollout-plugins.sh実行)済みか
```

`check-plugin-rollout.py` は上の2つとは別の失敗モードを塞ぐ: **source 3ファイルの version 整合と
bump-on-change を両方満たしていても、`rollout-plugins.sh` を実際に実行し忘れれば、その fix はどのセッションにも
一切反映されない。** 過去に一度、5個の plugin（hypothesis/condukt/compass/blastguard/overwatch）で
commit・version-bump 済みの fix が rollout されないまま放置されていたことが手作業の grep+jq 調査で発覚した。
このスクリプトはその調査をスクリプト化したもので、`installed_plugins.json` の `"<name>@yukineko"` registry
version と各 plugin.json の source version を比較し、不一致（＝ rollout 未実行）を機械的に検知する
（registry ファイルが存在しない環境では検査対象なしとして exit 0 で skip）。

`check-version-bumped.py` は `crates/<name>/` に差分がある plugin の plugin.json version が
base より**厳密に上がっている**ことを要求し、上がっていなければ exit 1 で該当 plugin と変更ファイルを
表示する（新規 plugin は base に無いので OK）。**「変更したのに未 bump」を機械的に止めるゲート**。

- 正典3ファイル（バイナリ付きプラグイン）: `crates/<name>/Cargo.toml` の `[package].version` /
  `crates/<name>/.claude-plugin/plugin.json` の `version` / `.claude-plugin/marketplace.json` の
  当該エントリ `version`。**skill-only プラグイン**（Cargo.toml 無し。例: `scout`）は
  plugin.json + marketplace.json の2つ。
- 正典の向き: **`Cargo.toml == plugin.json` が真**。`marketplace.json` は取り残されやすい（laggard）。
  version を上げるときは **必ず3ファイル同時**に上げる。marketplace.json だけ手で上げ忘れない。
- **すべてのタイミングで徹底チェック**（version を触る commit、rebuild 前、push 前）。
  自動チェッカで機械的に確認する:

  ```sh
  python3 scripts/check-plugin-versions.py   # exit 0 = 全整合 / exit 1 = drift（該当プラグインを表示）
  ```

- **rebuild は version を上げない**（別工程）。`rebuild-plugins.sh` は正典の version をそのまま
  コンパイルして live cache へ swap するだけ。version bump は 3ファイル編集という別の意図的操作。
- **cache の version dir が古い**（例: source は 0.7.0 だが `cache/.../<plugin>/0.6.0/` のまま）のは
  `/plugin update`（ユーザー UI 操作）未実行が原因。rebuild-plugins.sh は既存 dir にバイナリを
  swap するのでコードは動くが、正式ロールアウトは `/plugin update` → rebuild → sync-plugin-assets.sh
  の順。

## 人間のレビュー窓口（統合レビューサーフェス）

観測系の3ストリーム — (1) systemic な gate 違反（`overwatch violations --systemic`）、
(2) canary health-gate のロールバック事象、(3) AI/敵対的レビューの指摘 — は従来別々だった。
`overwatch review-queue` はこれらを **1本の時系列リスト**（新しい順）にまとめ、各行を
`[systemic]` / `[rollback]` / `[ai-finding]` のタグで区別して見せる:

```sh
overwatch review-queue                 # 人間可読の統合リスト（新しい順）
overwatch review-queue --json          # kind 判別子付きの構造化配列
overwatch review-queue --since <ts> --limit <n>
overwatch review-queue --to-backlog    # キュー全体を backlog に流し込む（discover→fix を閉じる）
```

（`[escalation]` = condukt の blocked/GATED タスクが人間の回答待ちで滞留している事象も4本目のストリームとして統合表示される。）

- **fail-soft**: いずれかのソースが空/欠落でも他のソースは表示される（コマンド全体はエラーにしない）。
  **review-queue は verdict を返さない純粋な観測・描画レイヤ**なので、欠落ソースは「違反なし」ではなく
  「そのソースは表示されない」として縮退する（判定を持たないから許される。冒頭「最上位の方針」を参照）。
- ロールバック事象は `scripts/rollout-plugins.sh` の canary auto-rollback 時に
  `overwatch record-rollback` で追記される（fail-soft: 記録失敗はロールアウトを止めない）。
- AI 指摘の永続ストア（`overwatch record-finding` の取り込み口）は Continuous-Audit ループ（別 backlog）が
  埋めるまで通常は空で、その場合この行は出ない（graceful degrade）。
- **`--to-backlog`（review-queue → backlog consolidation）**: レンダリングの代わりにキュー**全体**
  （ai-finding / systemic / rollback / escalation の4種）を1件ずつ `backlog add` へ転送し、`/flow` が
  自動修復できるようにする。冪等 — findings は `bridged_findings.jsonl`（bare finding-id。review-metrics の
  「解消済み」判定源でもある）、他3ストリームは `bridged_entries.jsonl`（`<kind>:<identifier>`）で二重投入を防ぐ。
  fail-soft: store 欠落 / backlog バイナリ不在 / `backlog add` 失敗はいずれも warn してスキップし、コマンドは常に成功する。

## さらに読む（docs/）

- `docs/GLOSSARY.md` — クレート・用語早見表（**最初に読む**）
- `docs/OVERVIEW.md` / `docs/USAGE.md` — 全体像と使い方
- `docs/context-optimization.md` / `docs/context-optimization-flow.md` — context 節約の設計
- `docs/plugin-dependency-graph.md` / `docs/plugin-activation-scopes.md` — プラグイン間依存・起動スコープ
- `docs/AGENTIC-CODING-GUIDE.md` — エージェント実装ガイド
