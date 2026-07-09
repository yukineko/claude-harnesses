---
name: tdd
description: API設計 → 失敗するテスト(RED) → 実装 → GREEN の順で必ず回す test-first ワークフロー。RED を証跡として記録するので「テストを先に書いた」ことが検証可能になる。tdd バイナリ(決定論ゲート)+ Stop hook と併用し、テスト無しの実装を物理的に止める。subscription で完結(API キー不要)。
argument-hint: '<実装したい振る舞い> [--cmd "<テストコマンド>"]'
allowed-tools: Read, Edit, Write, Grep, Glob, Bash
---

`/tdd <課題>` で、**API 設計 → RED(失敗するテスト)→ 実装 → GREEN** を1サイクル回す。
「テストを後回しにしない／先に書く」を、人間の意志ではなく **決定論バイナリ `tdd` の証跡**で
担保するのが目的。

**役割分担**: 判断(API 設計・テスト記述・実装)は LLM(=この skill)、決定論(RED だったか・
RED→GREEN になったか・テスト無し実装のブロック)は `tdd` バイナリと Stop hook。

## 前提確認

`tdd --version` が通るか確認する。無ければプラグイン未導入なので README の導入手順を案内する
(`${CLAUDE_PLUGIN_ROOT}/bin/tdd` が PATH に無い場合はフルパスで呼ぶ)。

タスク ID を1つ決める(例: 課題のスラッグ `parse-csv`)。以降 `--task <id>` に使う。
テストコマンドは引数 `--cmd` 優先、無ければ `tdd.toml` の `test_cmd`(既定 `cargo test`)。

## 不変条件(外さない)

1. **実装より先にテストを書く** — Phase 2 のテスト記述が終わるまで、実装本体(関数の中身)を
   書かない。シグネチャは `todo!()` / `unimplemented!()` / `pass` で**スタブのみ**。
2. **RED を必ず通す** — `tdd red` が成功(=テストが意図通り失敗)するまで実装に進まない。
   ここで「もうテストが通ってしまう」なら、それは**まだ振る舞いを試せていない**ので失敗テストを
   書き直す。
3. **GREEN は RED の後だけ** — `tdd green` は RED 証跡が無いと拒否する。証跡を捏造しない。
4. **テスト無しで終わらない** — Stop hook の `tdd gate` が、テスト追加の無い実装をブロックする。
   skill を使わない場合でもこれは効く。

## 手順

### Phase 1 — API/契約設計(実装しない)
課題から、追加・変更する **公開 API のシグネチャと型、エラー系、境界条件**を決める。
ファイルにはスタブだけ置く(本体は `todo!()` 等)。ここで「何をテストすべきか」が確定する。
迷いがあればこの時点でユーザーに `AskUserQuestion` で確認してよい。

### Phase 2 — テスト記述(まだ実装しない)
Phase 1 の API に対して、期待する振る舞いを表すテストを書く。正常系 + 主要な異常系/境界。
**この時点でスタブのままなので、テストは必ず落ちるはず。**

### Phase 3 — RED を記録
```
tdd red --task <id> [--cmd "<test cmd>"] [--author <id>]
```
- 成功(exit 0)= テストが落ちた = test-first 成立。`<proof_dir>/<id>.red.json` が書かれる。
- 失敗(exit 1, "tests passed …")= テストが通ってしまった。**実装してはいけない**。
  振る舞いを実際に検証する失敗テストへ書き直して Phase 3 をやり直す。
- **記録される identity**: `--author` を渡さない場合、証跡に記録される identity は
  `CLAUDE_CODE_SESSION_ID`(未設定なら共有バケット `_local`)に自動で default する。
  素の CLI 文字列を渡す honor-system には依存しない。`--author <id>` を明示すれば、それが
  session id より優先される(上書き)。

### Phase 4 — 実装
テストを GREEN にすることだけを目的に、スタブを実装で埋める。テストは変更しない
(テストのバグが判明した場合のみ最小限修正し、その旨を述べる)。

### Phase 5 — GREEN を記録
```
tdd green --task <id> [--cmd "<test cmd>"] [--author <id>]
```
- RED 証跡が無ければ拒否される(Phase 3 を飛ばしていないか確認)。
- テストがまだ落ちるなら Phase 4 に戻る。
- 成功すると `<proof_dir>/<id>.green.json` が書かれる。
- `--author` の扱いは Phase 3 と同じ(未指定なら `CLAUDE_CODE_SESSION_ID` default)。

### strict_separation(opt-in — RED/GREEN の著者分離を強制する)

`tdd.toml` で `strict_separation = true` にすると、`tdd green` は RED 証跡の identity と
GREEN の identity が**同一なら拒否**する(fail-closed)。既定は `false`(既存動作と完全互換)。

- **同一セッションが `--author` 無しで RED→GREEN を両方回す「よくあるケース」**は、
  identity が同じ `CLAUDE_CODE_SESSION_ID` に default するため、strict モードで実際に
  検知・拒否される(honor-system の裸文字列に依存しない)。
- 別セッション/別エージェントに GREEN を担当させたい場合は、そのセッションで
  `tdd green --task <id>` を実行するか、明示的に異なる `--author <id>` を渡す。
- **残存する回避可能性(正直に明記)**: これは HOTL(human-on-the-loop)のための抑止であり、
  暗号学的な認証ではない。`--author` を明示指定できる以上、1つのエージェントが意図的に
  2つの異なる `--author` 文字列を使い分ければこのゲートは回避できる(`CLAUDE_CODE_SESSION_ID`
  の偽装も同様)。ハードなセキュリティ境界ではなく、「同一エージェントが無自覚に
  RED も GREEN も両方書いてしまう」典型ケースを引っかけるための決定論ゲート、という位置づけ。

### Phase 6 — 検証して完了
```
tdd verify --task <id>      # RED と GREEN の両証跡が揃っていれば exit 0
```
- exit 0 を確認してから完了を報告する。リファクタする場合はここで行い、再度テストを回す。
- 変更点・テスト・RED→GREEN の証跡パスを要約して報告する。

## 失敗モード
- `tdd` 不在 → プラグイン導入を案内。
- RED が通らない(テストが最初から緑)→ 振る舞いを試せていない。アサーションを具体化。
- Stop でブロックされる → テストが付いていない実装がある。テストを足す。純粋な
  リファクタ/リネーム/ドキュメントのみなら、プロジェクト直下に `.tdd-skip` を作り
  理由を1行書く(1回だけ消費される)。
