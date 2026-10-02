# jev — TypeSafe AI System One の advisory 専用ラッパ

`jev` は**テキストではなく型付き・較正済みの判定**を返すモデル。確率（Noul）、
分布つきの選択（Choice）、ルーブリック上の位置（Score）の 3 つのプリミティブを持つ。
このクレートはそれをハーネス一家向けに、ただ 1 つのルールの下で包む。

> **jev は advisory。流れを止める/通す権限は一切持たない。**

これは CLAUDE.md 第7節（外部サービスにゲートを預けない）そのものであり、
ここでは**散文の約束ではなく型で**強制している。

## subscription-native ではない — 既定で無効

この repo のハーネスはほぼすべて API キー不要でサブスクリプション内で完結するが、
jev は違う。**課金を伴う第三者のクラウド API** である（入力 $0.042/M tokens、
出力無料。測定日 2026-10-02）。したがって:

- **`TYPESAFE_API_KEY` が無ければ、このプラグインは何もしない。**「優雅に縮退する」
  のではなく、ネットワーク呼び出しが 0 回で、いかなる判定にも影響できない値を返す。
- 呼び出しは毎回ローカルの使用量 ledger に追記されるので、支出が観測可能（`jev ledger`）。

## 安全に追加できる理由となる 2 つの性質

### 1. 返り値の型が「問題なし」と言えない

```rust
pub enum Advice {
    Escalate { question: String, probability: f64, detail: String },
    NoSignal { reason: NoSignalReason },
}
```

`Clean` / `Ok` / `Pass` は無く、今後も足さない。`NoSignal` は次のすべてを吸収する:
キー不在、環境が読めない、HTTP エラー、timeout、パース不能、呼び出し側の閾値未満、
**そして jev が「いや、これは問題ない」と積極的に答えた場合**。

最後の 1 つが核心である。「jev が承認した」は「jev が一度も走らなかった」と
**意図的に区別不能**にしてある。だから消費者側に「キーが無いときだけ挙動が変わる」
コードを書くことが*できない* — 不在は「そう書いてあるから」ではなく**構造的に**
no-op になる。同時に、逆向きの fail-open も閉じる:
**何も clean にできない advisor は、fail-open になりえない**。

`Clean` variant の追加は規約違反では済まず**コンパイルが通らない**。`Advice` は
`#[non_exhaustive]` ではなく `Advice::render` が網羅 match しているため
（実測 2026-10-02: 注入は `error[E0004]: non-exhaustive patterns` で死んだ）。

### 2. gate verdict と結合しない

`harness_core::verdict::Verdict` はこのクレートのコードに引数でも返り値でも登場せず、
`Advice` を `Verdict` に変換する関数も無い。`tests/no_verdict_coupling.rs` が
ソースを走査し、トークンが復活したら fail する。

transport の三値は `harness_core::verdict::Determination` を使っており、これは
意図的である: repo 共有の「判明/判定不能」コンテナであり（CLAUDE.md 第3節は
各 crate が再発明せずここへ収斂することを求めている）、verdict を運ばず、
そこから到達できる `Verdict` は `Required::Blocked` の偽造不能な `Undet` 経由の
`Verdict::Undetermined`（＝**block する**側）だけである。`Verdict::Clean` への
経路は存在しない。

## 「キーが無ければソケットも開かない」を**数えられる事実**にする

中心の要件は否定文なので、happy path を読んで確かめることはできない。transport は
注入式で、`RecordingTransport` は何も送らず呼び出し回数だけを数える。
`tests/no_key_no_network.rs` が 4 つの環境で**正確な回数**を固定する:

| `TYPESAFE_API_KEY` | availability | exit code | transport 呼び出し |
|---|---|---|---|
| 未設定 | `NotConfigured` | 1 | **0** |
| `""` / 空白のみ | `NotConfigured` | 1 | **0** |
| 非 UTF-8 | `Undetermined` | 10 | **0** |
| 実キー | `Configured` | 0 | **1** |

`1` と `10` はどちらも「jev を使うな」の意味だが、別コードにしてある。
**意図的な opt-out と壊れた環境は別の事実**であり、後者を前者に畳むと
「故障」を「選択」として報告してしまう（第3節）。

**F→P オラクル**: この 4 ケースは GREEN の前に RED を観測している。`Client::ask`
から availability ガードを外した版（env をそのまま bearer にして呼ぶ素朴な実装）では、
キー不在の 4 つの assertion がすべて `left: 1, right: 0` で落ちた。
**落ちたことのないテストは何も証明しない。**

## 鍵の衛生

- **環境変数のみ。** `--api-key` フラグは作らない（フラグはシェル履歴に残る）。
- **argv に載せない。** beacon の `curl` パターンはリクエストを argv で渡すが、
  webhook URL なら許容でも bearer token では駄目である。argv はリクエストの生存期間中
  `ps` と `/proc/<pid>/cmdline` から読めるため。この transport は URL・ヘッダ・body を
  `curl -K -` の **stdin config** に書く。
- **二重に redact** — 完全一致（キーを手元に持っているため）と、パターン
  （`Bearer …` / `apikey_…`）の両方で、クレートを出る全文字列に適用する。
  危険なのはエラー経路であり、キーをエコーバックする 401 body が
  `tests/no_credential_leak.rs` の実験対象である。
- `Credential` の `Debug` は末尾 4 文字しか出さない。後から誰かがデバッグのために
  `{:?}` を足しても漏れない。
- ledger はトークン数・model・レイテンシ・status・**question キー**を記録するが、
  評価対象の `state` は**決して**記録しない（会計ファイルが「ハーネスが見た全ての物の
  コピー」になってしまう）。

## 使い方

```sh
jev check                 # 三値の可用性を JSON で。exit 0 / 1 / 10
echo '{"state": "...", "questions": {"q": {"type":"noul","instructions":"..."}}}' \
  | jev ask               # 回答を JSON で。キーが無ければネットワーク呼び出しなし
jev ledger                # 件数・トークン・推定 USD
```

設定は `~/.jev/config.toml`（`JEV_CONFIG` で移動可）:

```toml
model = "jev-1.13.0"   # 固定推奨。下記参照
max_time_secs = 8
```

env override: `JEV_MODEL` / `JEV_MAX_TIME_SECS` / `JEV_LEDGER` / `JEV_ENDPOINT`。
**キーはここでは設定できない**（環境変数のみ）。

## 使う前に知っておくべき落とし穴

測定日 2026-10-02、docs.typesafe.ai より。このクレートは**どれも隠さない**。
呼び出し側が知っている必要があるため:

- **数え上げは規模とともに誤差が増える。** コード側でループすること。
- **日付演算は信頼できない。** Choice で年月日を抽出し、計算はコードで。
- **フレーミングで答が変わる。** 同じ問いを Noul と Choice で聞くと別の答になり、
  否定の和は 1.0 にならない。混ぜて比較しない。
- **敵対的テキストで答が動く** — 自分の分類を主張する内容は答をずらせる。
  これが jev を advisory に留める中核的な理由である（入力はしばしば攻撃者の影響下にある）。
- モデルは**字面どおり**に答える。意図は読まない。
- `noul = 0.5` は「中程度」ではなく「等確率」。スペクトラムには Score を使う。
- **`jev-latest` はドリフトする。** 具体版へ解決され、その版は変わる。ある版で
  調整した閾値は一緒にずれる。**閾値を調整する前にバージョンを固定すること。**
  ledger には*レスポンスが報告した* model を記録するので、実際に何が答えたかは
  推測ではなく観測できる。

## どこに配線してよいか／いけないか

[`docs/jev-integration-scenes.md`](../../docs/jev-integration-scenes.md) を参照。
要約: ランキング・重複排除・人間向け注記は可。ゲート（donegate / reviewgate /
propguard / tdd / parallelguard / precommit-audit / budgetguard / condukt 完了ゲート /
F→P オラクル）と `.githooks/` 配下は**恒久的に対象外**。

## テスト

```sh
cargo test -p jev     # オフライン。ネットワーク transport は一度も構築しない
```
