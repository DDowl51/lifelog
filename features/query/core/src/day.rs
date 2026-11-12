//! 逻辑日：一天从 `day_boundary`（默认本地时间 04:00）开始，而不是从午夜开始。
//! 凌晨 1 点还在写代码，算作「前一天」的事。

use clock_contract::Moment;
use jiff::civil::{Date, Time};
use jiff::tz::Offset;
use jiff::{Timestamp, ToSpan};

fn offset(minutes: i32) -> Result<Offset, String> {
    Offset::from_seconds(minutes * 60).map_err(|_| format!("时区偏移 {minutes} 分钟不合理"))
}

/// 解析 `HH:MM`。
pub fn parse_boundary(text: &str) -> Result<Time, String> {
    let bad = || format!("day_boundary = {text:?} 要写成 HH:MM，例如 \"04:00\"");
    let (h, m) = text.split_once(':').ok_or_else(bad)?;
    if h.len() != 2 || m.len() != 2 {
        return Err(bad());
    }
    let (h, m) = (
        h.parse::<i8>().map_err(|_| bad())?,
        m.parse::<i8>().map_err(|_| bad())?,
    );
    Time::new(h, m, 0, 0).map_err(|_| bad())
}

/// 某个时刻属于哪个逻辑日。
pub fn logical_date(at: Moment, boundary: Time) -> Result<Date, String> {
    let ts = Timestamp::from_millisecond(at.utc_ms).map_err(|e| e.to_string())?;
    let local = offset(at.offset_min)?.to_datetime(ts);
    if local.time() < boundary {
        local.date().yesterday().map_err(|e| e.to_string())
    } else {
        Ok(local.date())
    }
}

/// 把命令行上的日期参数变成逻辑日。
///
/// 接受 `today`、`yesterday`、`<N>d`（N 天前）、`YYYY-MM-DD`。
pub fn resolve_date(spec: &str, now: Moment, boundary: Time) -> Result<Date, String> {
    let today = logical_date(now, boundary)?;
    let back = |days: i64| today.checked_sub(days.days()).map_err(|e| e.to_string());
    match spec {
        "today" => Ok(today),
        "yesterday" => back(1),
        _ => {
            if let Some(n) = spec.strip_suffix('d')
                && !n.is_empty()
                && n.chars().all(|c| c.is_ascii_digit())
            {
                let days: i64 = n.parse().map_err(|_| format!("{spec:?} 的天数太大了"))?;
                return back(days);
            }
            spec.parse::<Date>().map_err(|_| {
                format!("看不懂日期 {spec:?}。可以写 today、yesterday、3d（3 天前）或 2026-11-12")
            })
        }
    }
}

/// 往前（负数）或往后挪几天。
pub fn shift(date: Date, days: i64) -> Result<Date, String> {
    date.checked_add(days.days()).map_err(|e| e.to_string())
}

/// `date` 所在那一周的七天，周一到周日。
pub fn week_dates(date: Date) -> Result<Vec<Date>, String> {
    let monday = date
        .checked_sub(i64::from(date.weekday().to_monday_zero_offset()).days())
        .map_err(|e| e.to_string())?;
    (0..7)
        .map(|i: i64| monday.checked_add(i.days()).map_err(|e| e.to_string()))
        .collect()
}

/// 逻辑日 `date` 覆盖的范围 `[start, end)`，UTC 毫秒。按固定偏移 `offset_min` 计算。
pub fn day_range(date: Date, offset_min: i32, boundary: Time) -> Result<(i64, i64), String> {
    let offset = offset(offset_min)?;
    let at = |d: Date| {
        offset
            .to_timestamp(d.to_datetime(boundary))
            .map(Timestamp::as_millisecond)
            .map_err(|e| e.to_string())
    };
    Ok((at(date)?, at(date.tomorrow().map_err(|e| e.to_string())?)?))
}

#[cfg(test)]
mod tests {
    use jiff::civil::date;

    use super::*;

    fn at(s: &str) -> Moment {
        let ts: Timestamp = s.parse().unwrap();
        Moment {
            utc_ms: ts.as_millisecond(),
            offset_min: 480,
        }
    }

    fn four() -> Time {
        parse_boundary("04:00").unwrap()
    }

    #[test]
    fn boundary_parsing() {
        assert_eq!(
            parse_boundary("04:00").unwrap(),
            Time::new(4, 0, 0, 0).unwrap()
        );
        assert_eq!(parse_boundary("00:00").unwrap(), Time::midnight());
        assert_eq!(
            parse_boundary("23:30").unwrap(),
            Time::new(23, 30, 0, 0).unwrap()
        );
        for bad in ["4:00", "04", "24:00", "04:60", "04:00:00", "aa:bb", ""] {
            assert!(parse_boundary(bad).unwrap_err().contains("HH:MM"), "{bad}");
        }
    }

    #[test]
    fn before_the_boundary_belongs_to_the_previous_day() {
        // 本地 2026-10-20 03:59:59
        assert_eq!(
            logical_date(at("2026-10-19T19:59:59Z"), four()).unwrap(),
            date(2026, 10, 19)
        );
        // 本地 2026-10-20 04:00:00
        assert_eq!(
            logical_date(at("2026-10-19T20:00:00Z"), four()).unwrap(),
            date(2026, 10, 20)
        );
    }

    #[test]
    fn the_offset_decides_the_local_day() {
        let tokyo = Moment {
            offset_min: 540,
            ..at("2026-10-19T20:00:00Z")
        };
        let la = Moment {
            offset_min: -420,
            ..at("2026-10-19T20:00:00Z")
        };
        assert_eq!(logical_date(tokyo, four()).unwrap(), date(2026, 10, 20));
        assert_eq!(logical_date(la, four()).unwrap(), date(2026, 10, 19));
    }

    #[test]
    fn day_range_starts_at_the_boundary() {
        let (start, end) = day_range(date(2026, 10, 20), 480, four()).unwrap();
        assert_eq!(start, at("2026-10-19T20:00:00Z").utc_ms);
        assert_eq!(end, at("2026-10-20T20:00:00Z").utc_ms);
        assert_eq!(end - start, 24 * 3_600_000);
    }

    #[test]
    fn date_arguments() {
        // 本地 2026-10-20 12:00
        let now = at("2026-10-20T04:00:00Z");
        let resolve = |spec: &str| resolve_date(spec, now, four());
        assert_eq!(resolve("today").unwrap(), date(2026, 10, 20));
        assert_eq!(resolve("0d").unwrap(), date(2026, 10, 20));
        assert_eq!(resolve("yesterday").unwrap(), date(2026, 10, 19));
        assert_eq!(resolve("1d").unwrap(), date(2026, 10, 19));
        assert_eq!(resolve("30d").unwrap(), date(2026, 9, 20));
        assert_eq!(resolve("2026-01-05").unwrap(), date(2026, 1, 5));
        for bad in ["tomorow", "d", "-1d", "2026-13-01", "10/20", ""] {
            assert!(resolve(bad).unwrap_err().contains("today"), "{bad}");
        }
    }

    #[test]
    fn weeks_start_on_monday() {
        let days = week_dates(date(2026, 10, 22)).unwrap(); // 周四
        assert_eq!(days.first(), Some(&date(2026, 10, 19)));
        assert_eq!(days.last(), Some(&date(2026, 10, 25)));
        assert_eq!(week_dates(date(2026, 10, 19)).unwrap(), days, "周一");
        assert_eq!(week_dates(date(2026, 10, 25)).unwrap(), days, "周日");
        // 跨月
        assert_eq!(
            week_dates(date(2026, 11, 1)).unwrap()[0],
            date(2026, 10, 26)
        );
    }

    #[test]
    fn today_before_the_boundary_is_still_yesterday() {
        // 本地 2026-10-20 01:00：还算 19 号
        let now = at("2026-10-19T17:00:00Z");
        assert_eq!(
            resolve_date("today", now, four()).unwrap(),
            date(2026, 10, 19)
        );
        assert_eq!(
            resolve_date("yesterday", now, four()).unwrap(),
            date(2026, 10, 18)
        );
    }
}
