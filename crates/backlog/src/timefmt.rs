//! Human-readable rendering of unix timestamps (moved out of main.rs unchanged).

/// Unix タイムスタンプ (秒) を "YYYY-MM-DD HH:MM UTC" 形式の文字列に変換する。
/// 標準ライブラリのみで実装 (外部クレート不使用)。
pub fn format_unix_datetime(secs: u64) -> String {
    // グレゴリオ暦変換 (ユリウス通日ベース)
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hh = time_of_day / 3600;
    let mm = (time_of_day % 3600) / 60;

    // 1970-01-01 からの日数を年月日に変換 (Fliegel-Van Flandern algorithm)
    let jd = days + 2440588; // Julian Day Number for 1970-01-01
    let l = jd + 68569;
    let n = 4 * l / 146097;
    let l = l - (146097 * n).div_ceil(4);
    let i = 4000 * (l + 1) / 1461001;
    let l = l - 1461 * i / 4 + 31;
    let j = 80 * l / 2447;
    let day = l - 2447 * j / 80;
    let l = j / 11;
    let month = j + 2 - 12 * l;
    let year = 100 * (n - 49) + i + l;

    format!(
        "{:04}-{:02}-{:02} {:02}:{:02} UTC",
        year, month, day, hh, mm
    )
}
