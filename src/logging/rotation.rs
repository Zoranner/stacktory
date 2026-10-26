//! 需要落盘的 UTC 时间标签和按天轮转的文件名。仓库不引入日期库，这里集中
//! 实现公历换算，供运行日志和审计文件共用。

use std::time::{SystemTime, UNIX_EPOCH};

const SECS_PER_DAY: u64 = 86_400;

/// `YYYYMMDD`，用于按天轮转的文件名。
pub(crate) fn date_label(time: SystemTime) -> Option<String> {
    let seconds = time.duration_since(UNIX_EPOCH).ok()?.as_secs();
    let (year, month, day) = civil_from_days((seconds / SECS_PER_DAY) as i64);
    Some(format!("{year:04}{month:02}{day:02}"))
}

/// `YYYY-MM-DDTHH:MM:SSZ`，用于日志行前缀。
pub(crate) fn timestamp_label(time: SystemTime) -> Option<String> {
    let seconds = time.duration_since(UNIX_EPOCH).ok()?.as_secs();
    let (year, month, day) = civil_from_days((seconds / SECS_PER_DAY) as i64);
    let time_of_day = seconds % SECS_PER_DAY;
    let hour = time_of_day / 3_600;
    let minute = (time_of_day % 3_600) / 60;
    let second = time_of_day % 60;
    Some(format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z"
    ))
}

/// 从 `<前缀><YYYYMMDD><后缀>` 形式的轮转文件名中取出日期标签。
///
/// 只有 8 位纯数字才算日期标签，避免把正文文件名当成轮转结果。
pub(crate) fn rotated_label(
    name: &str,
    rotated_prefix: &str,
    rotated_suffix: &str,
) -> Option<String> {
    let label = name
        .strip_prefix(rotated_prefix)?
        .strip_suffix(rotated_suffix)?;
    (label.len() == 8 && label.bytes().all(|byte| byte.is_ascii_digit())).then(|| label.to_owned())
}

/// 把 1970-01-01 起的天数转换为公历日期（Howard Hinnant 的 civil_from_days）。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let days = days + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_index + 2) / 5 + 1) as u32;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::{civil_from_days, date_label, rotated_label, timestamp_label};
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn converts_epoch_days_to_calendar_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
        assert_eq!(civil_from_days(20_654), (2026, 7, 20));
    }

    #[test]
    fn formats_dates_and_timestamps_in_utc() {
        let time = UNIX_EPOCH + Duration::from_secs(1_784_000_000);
        assert_eq!(date_label(time).as_deref(), Some("20260714"));
        assert_eq!(
            timestamp_label(time).as_deref(),
            Some("2026-07-14T03:33:20Z")
        );
    }

    #[test]
    fn accepts_only_dated_rotation_names() {
        assert_eq!(
            rotated_label("events.20260913.jsonl", "events.", ".jsonl").as_deref(),
            Some("20260913")
        );
        assert_eq!(rotated_label("events.jsonl", "events.", ".jsonl"), None);
        assert_eq!(
            rotated_label("events.2026.jsonl", "events.", ".jsonl"),
            None
        );
        assert_eq!(
            rotated_label("stacktory.20260913.log", "stacktory.", ".log").as_deref(),
            Some("20260913")
        );
    }
}
