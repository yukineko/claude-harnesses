## north_star
PDO運用の measure ループを機構で閉じ切る (build ≠ validate): 出荷済み・未計測(awaiting-measurement)の計測負債を可観測化し、shipped-vs-measured の健全性を1コマンドで surface する

## definition_of_done
- SessionStart 注入が awaiting-measurement の計測負債を surface する (件数 + 最古仮説の経過日数)。hypothesis が 0 件なら無音、負債があれば起動 context に負債行が出る
- shipped-vs-measured の PDO 健全性メトリクスを1コマンドで JSON 出力できる (shipped / validated / awaiting 件数 + 平均計測遅延)。回帰テストで数値を固定
- measurement-debt 加齢ゲート: awaiting-measurement が閾値日数超で停滞したとき明示 warning を出す (閾値超入力で warning、未満で無音の赤緑テスト)

## measuring_stick
擁護可能性 × ゴールへの接近距離 ÷ コスト

## current_gap
measure ループの機構 (hypothesis crate / awaiting-measurement 状態 / flow measure step) は在るが観測面が欠落している: SessionStart は open 仮説しか surface せず awaiting-measurement の計測負債が見えない、shipped-vs-measured の健全性メトリクス・measurement-debt 加齢ゲートも未実装。build を validate に閉じる観測面が無い。最小スライスは DoD1 (SessionStart で計測負債を surface)。

## next_action

## parked

