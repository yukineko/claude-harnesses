# blastguard

> 🌐 [English](README.md) ・ **日本語**

**プロジェクトを破壊しかねない操作を実行前に止める PreToolUse ガード**

## 目的

blastguard は Claude Code の **PreToolUse** フックである。エージェントが実行しようと
しているツール呼び出しを stdin から受け取り、純粋関数で allow / deny を判定し、
**deny のときだけ** PreToolUse の `deny` JSON を出力して、その操作を実行前に握り潰す。

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

設計は意図的に**保守的**である。曖昧なものはすべて allow に倒すので、通常作業の邪魔を
しない。非再帰の `rm file.txt`、追記（`>>`）、fd リダイレクト（`2>&1`, `>&2`）、
`/dev/null` 等への切り詰めリダイレクトはいずれも通す。

さらに、リポジトリの**設定ファイル**に対する編集 / 削除は、形が破壊的に見えても常に
除外（allow）する: `.claude/**`、`**/settings.local.json`、`**/.claude/settings.json`、
`**/package.json`、`**/*.toml` / `*.yaml` / `*.yml` / `*.lock`、`.config/**` など。

## どうして必要か

エージェントによるコーディングでは、`rm -rf`・`git reset --hard`・`git clean -fdx`・
単一 `>` での上書きといった一手が、コミット前の作業や巨大なディレクトリを一瞬で消し
飛ばす。これらは取り返しがつかず、しかもツール呼び出しの中に紛れて流れてくるため、
人間が毎回目視で止めるのは現実的でない。

blastguard はこの「破壊的だが不可逆な少数のパターン」だけを実行前に遮断する安全網に
徹する。判定は純粋関数で決定論的に行われ、**ターンを決して壊さない**という不変条件を
守る: 入力が空 / 不正、対象外のツール、内部 panic のいずれでも、黙って exit 0 する
（panic の握り潰しは `harness_core::hook::run_hook` が保証する）。広く構えすぎて通常
作業を妨げるより、明確に危険なものだけを確実に止めることを優先している。

## ライブラリとしての再利用

`src/lib.rs` は同じ検出ロジックを他クレートへも公開している（純粋関数・I/O なし）:
specguard の forge は LLM が生成した `test_cmd` を `sh -c` に渡す前に
`detect::detect` で検証し、condukt のスケジューラは段階的な
`classify::classify`（risk / reversibility 判定）を使って、上流の LLM が
誤ラベル付けした場合でも deploy・`git push`・release のような対外的で不可逆な
アクションを GATED ゲートへ強制的に通す。

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
exec する POSIX-sh ランチャで、該当ビルドが同梱されていなければ黙って exit 0 する。
API キーは不要で、**1 フック + 同梱バイナリだけで完結する subscription-native** な
構成である。

CLI 表面は最小限で、stdin を触る前に `--version` / `-V` と `--help` / `-h` のみ
短絡する。

ビルド / テスト:

```sh
cargo build --release -p blastguard   # -> target/release/blastguard
make bins                             # 各プラットフォーム向け同梱バイナリを更新
cargo test -p blastguard              # ユニット + 統合テスト
```
