# schemaguard

> 🌐 [English](README.md) ・ **日本語**

**LLM の構造化出力を source→executor 境界で検証するスキーマゲート (subscription-native)**

## 目的

harness のある段が次の段に JSON ペイロードを引き渡すとき (分解 decomposition、エピソード記録 episode、プレイブック playbook、scout の計測 scout-measure)、その値を **名前付きで宣言済みのスキーマ** に照らして検証する CLI である。

責務は 3 つに絞られている:

- **検証** — `check --schema <name>` で JSON 値 (`--file <path>` または stdin) を宣言済みスキーマに照合する。
- **構造化エラーの提示** — 違反時に `{valid, schema, errors[], undetermined[], not_checked[]}` を出力し、`errors[]` には `{path, problem}` を並べる。これが producer がモデルに再依頼 (re-ask) するための契約となる。
- **メトリクス計上** — パース失敗・フィールド違反の双方を reject としてスキーマ別に記録し、`metrics` で参照できる。

宣言済みスキーマは `decomposition` / `episode` / `playbook` / `scout-measure` の 4 つ (`schemaguard list` で確認できる)。

subscription-native であり、バンドルされた単一の Rust バイナリで動く。**API キーは不要**。lifecycle hook ではなく素の CLI であり、構造化された受け渡しが起きる箇所で呼び出して終了コードで分岐する設計である。

## どうして必要か

source→executor の境界で渡される JSON が壊れていたり想定スキーマから外れていると、それが黙って捨てられたり (silently-dropped)、不正なまま次段に流れ込んでしまう。そうなると失敗が観測できず、原因の追跡が難しくなる。

schemaguard を境界に挟むことで:

- 不正なペイロードが消失する代わりに **観測可能** になる (reject がメトリクスに残る)。
- producer は構造化エラーを使って **ちょうど一度だけ正確に再依頼** できる。
- どのスキーマで何件 reject されたかが集計され、品質の劣化が見える化される。

## どう使うか

プラグインマーケットプレイス経由でインストールすると、バンドルされた `bin/schemaguard` が利用可能になる。配線すべき lifecycle hook は無く、構造化出力を生成するスキルや hook から境界で直接呼び出して終了コードで分岐する。

終了コードの意味:

| コード | 意味 |
|---|---|
| `0` | JSON がパースでき、実行された検査はすべて合格 (実行されなかった検査は `not_checked[]` に列挙される) |
| `1` | パースは成功したがスキーマ違反あり (→ 再依頼) |
| `2` | 判定不能 — JSON のパース失敗、未知のスキーマ指定、または宣言された制約を渡された値に適用できなかった (`undetermined[]`) |

サブコマンド:

| サブコマンド | 役割 | 終了コード |
|---|---|---|
| `check --schema <name>` | JSON 値 (`--file <path>` または stdin) を名前付きスキーマで検証し `{valid, schema, errors[], not_checked[]}` を出力 | `0` 妥当 / `1` 違反 / `2` パース失敗・未知スキーマ・適用不能な制約 |
| `metrics` | スキーマ別の reject 件数を出力 (`--json` で機械可読) | `0` |
| `list` | 既知のスキーマ名を一覧表示 | `0` |

### 判定は 3 値である (2 値ではない)

`check` の出力は「検査して失敗」「検査できなかった」「意図的に検査しなかった」を別々の配列に分けて出す。
違反 0 件という空集合が、この 3 つを区別できないまま `valid:true` に潰れるのを避けるためである。

- **`errors[]`** — 検査が**実行され、失敗した**もの。`{path, problem}`。再依頼の契約。
- **`undetermined[]`** — 宣言された制約を**適用できなかった**もの (例: `items` サブスキーマを配列でない値に
  適用しようとした場合)。「検査できなかった」は「合格」ではないので、この場合は `0` ではなく **`2`** で終了する。
- **`not_checked[]`** — **意図的に実行しなかった**検査と、その根拠となる宣言。欠落した `required: false`
  フィールド (その型/enum/items 制約は一度も評価されていない)、`Ty::Any` フィールド (型検査なし)、
  `items` を宣言していない配列 (要素は検査されない)、スキーマが宣言していない余剰キー (許容される) が入る。
  これらは**ブロックしない**。出力するのは、`"valid": true` かつ `"errors": []` が
  「宣言された制約はすべて評価され合格した」と読まれてしまうのを防ぐためである
  (実際には一度も評価されていない制約がありうる)。

最小例 (standalone / cargo):

```sh
cargo install --path .
schemaguard list                                          # 宣言済みスキーマ名を表示
echo '{...}' | schemaguard check --schema decomposition   # stdin を検証
schemaguard check --schema episode --file out.json        # ファイルを検証
schemaguard metrics --json                                # スキーマ別 reject 件数
```

呼び出し側は終了コード `1`/`2` のとき `errors[]` を再依頼の手がかりとしてモデルに戻し、一度だけ再生成させる運用になる。

## ビルド

```sh
cargo test
```

コミット済みの `bin/schemaguard-*` バイナリがプラグインとして配布されるため、エンドユーザーは cargo も API キーも不要である。検証の振る舞いを変えたら、ワークスペースをビルド (`cargo build --workspace --release`) して再コミットする。
