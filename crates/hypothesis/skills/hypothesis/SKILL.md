---
name: hypothesis
description: PDO（プロダクト発見）の仮説ライフサイクルを管理する。仮説の追加・検証待ち・検証・棄却・一覧表示と、RAT（最も危険な前提のデリスク）・confidence 設定・compass ゴールへの紐づけを扱う。
argument-hint: "[サブコマンド: add / validate / reject / await-measurement / assume / rat / tested / confidence / list]"
allowed-tools: Bash(hypothesis:*), Read
---

# hypothesis スキル

PDO（プロダクト発見）の仮説ライフサイクルを管理します。仮説の追加・検証・棄却・一覧表示を行い、compass のゴールと連動させることで、発見活動を構造化します。

## コマンド一覧

### 新規仮説追加

```
hypothesis add "仮説テキスト" [--goal "ゴールキーワード"]
```

- `"仮説テキスト"` — 追加する仮説の内容を文字列で指定する
- `--goal "ゴールキーワード"` — compass の `charter.md` に記載された `north_star` または DoD キーワードを指定して仮説を紐づける（省略可）

例:
```
hypothesis add "ユーザーはオンボーディングで離脱している" --goal "retention"
```

### 検証済みにマーク

```
hypothesis validate <id> [--evidence "根拠"] [--measurement "指標=値"] [--run <id>]
```

- `<id>` — 仮説 ID（`hypothesis list` で確認。8桁の16進文字列、例 `a1b2c3d4`）
- `--evidence "根拠"` / `--measurement "指標=値"` — 検証の根拠。**少なくとも一方が必須**（両方省略は
  エラーになる）。ビルドが完了しただけでは validate できない（build ≠ validate）— 観察された結果
  （インタビュー・実測値など）を伴わずにステータスは変更できない
- `--run <id>` — 紐づく condukt run ID（省略可）

例:
```
hypothesis validate a1b2c3d4 --evidence "ユーザーインタビュー5件中4件で確認"
hypothesis validate a1b2c3d4 --measurement "activation=0.45"
```

### 棄却にマーク

```
hypothesis reject <id> --reason "理由" [--run <id>]
```

- `<id>` — 仮説 ID
- `--reason "理由"` — 棄却理由。**必須**（省略・空文字はエラーになる）— 何が反証したかを記録せずに
  棄却済みにはできない
- `--run <id>` — 紐づく condukt run ID（省略可）

例:
```
hypothesis reject a1b2c3d4 --reason "A/B テストで有意差なし"
```

### 検証待ちにマーク（build ≠ validate の中間状態）

```
hypothesis await-measurement <id> [--run <id>]
```

- 紐づく成果物は出荷済みだがまだ計測していない場合に使う。`open`（未着手）でも `validated`/`rejected`
  （計測済み）でもない、中間ステータス `awaiting-measurement` にする
- 計測が終わったら通常どおり `validate`/`reject` を（証拠つきで）実行する

### RAT（riskiest-assumption test）— 仮説を支える前提を切り崩す

```
hypothesis assume <id> --text "前提" --risk low|medium|high --evidence strong|weak|none
hypothesis rat <id>
hypothesis tested <id> <index>
```

- `assume` — 仮説が成り立つための前提（assumption）を1件登録する。`--risk` は前提が外れたときの
  ダメージ、`--evidence` は現時点でその前提を裏付ける証拠の強さ
- `rat <id>` — 登録済み前提のうち、まだテストされておらず「リスク大 × 証拠薄」な最も危険な
  leap of faith を `<index>\t<assumption text>` 形式で出力する（デリスクすべき前提が無ければ何も
  出力せず exit 0）
- `tested <id> <index>` — `rat` が示した前提を RAT で検証した後、`--assume` で付けたインデックスの
  前提を tested とマークする

### 確信度（confidence）の設定

```
hypothesis confidence <id> <value>
```

- 仮説の発見確信度（0.0–1.0）を設定する。`add --confidence <value>` でも新規登録時に指定できる
  （省略時デフォルト 0.5）
- `hypothesis list` の並び順を決める（下記）

### 一覧表示

```
hypothesis list [--status open|awaiting-measurement|validated|rejected]
```

- `--status open` — 未着手の仮説のみ表示
- `--status awaiting-measurement` — 出荷済み・計測待ちの仮説のみ表示
- `--status validated` — 検証済みの仮説のみ表示
- `--status rejected` — 棄却済みの仮説のみ表示
- フィルタなしで全仮説を表示
- 表示順は **confidence（確信度）降順**（同値なら作成日時が古い順）— 次に検証すべき仮説が
  自然と上に来る

例:
```
hypothesis list --status open
```

## compass との連動

`--goal` オプションに `charter.md` の `north_star` フィールドや DoD（Definition of Done）に記載されたキーワードを指定することで、仮説を compass のゴールに紐づけられます。

セッション開始時に `hypothesis session-start` が自動実行され、現在オープンな仮説の数とゴール別の内訳が表示されます。compass が示す次のアクションと照合しながら、検証すべき仮説を優先付けしてください。

### 典型的なワークフロー

1. compass で現在のゴール（north_star）を確認する
2. `hypothesis add` でゴールに紐づいた仮説を登録する（任意でリスクの高い前提を `hypothesis assume` で
   登録し、`hypothesis rat` で最も危険な前提を確認・デリスクする）
3. 発見活動（インタビュー・実験・データ分析）を実施する。成果物を出荷したがまだ計測していない場合は
   `hypothesis await-measurement` で `awaiting-measurement` にしておく
4. 観察された根拠（`--evidence` または `--measurement`）が得られたら `hypothesis validate`
   （証拠必須）または `hypothesis reject`（`--reason` 必須）でステータスを更新する
5. `hypothesis list --status open` で confidence 降順に未着手の仮説を確認し、次の発見活動を計画する
