## north_star
rollout drift(committedかつversion-bump済みだがplugin cacheへ実際にデプロイされていない状態)を機械的に検知するCIゲートを新設し、本セッションで発覚した「fixは committed/version-bump済みなのに未 rollout」という事故クラスの再発を防ぐ

## definition_of_done
- 各 crates/<name>/.claude-plugin/plugin.json の version と ~/.claude/plugins/installed_plugins.json の該当 plugin の registry version を比較し、不一致があれば非ゼロ終了でリストを出力する新規スクリプト(例 scripts/check-plugin-rollout.py)が追加される
- 意図的にdrift状態(一時的にregistry versionを1つ古くする、またはCargo.toml versionだけ上げてrolloutしない状態)を作って新スクリプトの検知が失敗(exit非ゼロ)することを確認し、正常状態に戻してexit 0になることを確認する(fix無しでは検知できないことの証明に相当する検証プロセス)
- 既存の check-plugin-versions.py / check-version-bumped.py と同様、CLAUDE.mdの該当セクションに新ゲートの実行コマンドが追記され、pre-push相当のタイミングで案内される

## measuring_stick
擁護可能性 × ゴールへの接近距離 ÷ コスト

## current_gap
既存の check-plugin-versions.py(3ファイル間のversion一致)と check-version-bumped.py(変更されたplugin のbump有無)は source(Cargo.toml/plugin.json/marketplace.json)側の整合しか見ておらず、実際に ~/.claude/plugins/installed_plugins.json のregistry versionへ反映(rollout-plugins.sh実行)されているかは一切チェックしていない。本セッションはこのギャップにより5個のplugin(hypothesis/condukt/compass/blastguard/overwatch)でfixがcommit・version-bump済みなのに未rolloutのまま放置されていたことが判明した(手作業のfleet-wide grep+jqループで発見)。最小right-sizeな一手は、この手作業ループをスクリプト化した scripts/check-plugin-rollout.py を新設し、source側versionとregistry版versionの不一致を機械的・決定論的に検知することで、次に同じ事故が起きても人間が偶然気づく前に検知できるようにすること。

## next_action

## parked

