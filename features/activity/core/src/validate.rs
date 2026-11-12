//! 活动合不合法。入库前检查。

use activity_contracts::Activity;

/// 不合法时返回原因。
pub fn validate(activity: &Activity) -> Result<(), String> {
    check_name("source", &activity.source)?;
    check_name("kind", &activity.kind)?;

    if let Some(end) = activity.ts_end
        && end < activity.ts_start
    {
        return Err(format!("结束时刻 {end} 早于开始时刻 {}", activity.ts_start));
    }

    if activity.dedup_key.as_deref() == Some("") {
        return Err("dedup_key 不能是空字符串（不需要去重就不要给）".to_owned());
    }

    // 世界上最大的偏移是 ±14 小时
    if !(-14 * 60..=14 * 60).contains(&activity.tz_offset_min) {
        return Err(format!("tz_offset_min = {} 不合理", activity.tz_offset_min));
    }

    Ok(())
}

/// `source`、`kind` 只允许小写字母、数字和 `_` `-` `.`。
fn check_name(field: &str, value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("{field} 不能为空"));
    }
    let ok = value
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'));
    if ok {
        Ok(())
    } else {
        Err(format!("{field} = {value:?} 只能用小写字母、数字和 _ - ."))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Activity {
        Activity::new("focus", "window_focus", 1_000, 480)
            .with_end(2_000)
            .with_dedup_key("focus:1:1")
    }

    #[test]
    fn accepts_spans_and_instants() {
        assert_eq!(validate(&sample()), Ok(()));
        assert_eq!(
            validate(&Activity::new("shell", "command", 1_000, 0)),
            Ok(())
        );
        // 长度为 0 的时段是合法的：时段刚打开时就是这样
        assert_eq!(validate(&sample().with_end(1_000)), Ok(()));
        assert_eq!(validate(&Activity::new("a.b-c_1", "k", 0, -720)), Ok(()));
    }

    #[test]
    fn rejects_bad_fields() {
        let reason = |a: Activity| validate(&a).unwrap_err();

        let mut a = sample();
        a.source = String::new();
        assert!(reason(a).contains("source 不能为空"));

        let mut a = sample();
        a.kind = "Window Focus".to_owned();
        assert!(reason(a).contains("kind"));

        assert!(reason(sample().with_end(999)).contains("早于"));
        assert!(reason(sample().with_dedup_key("")).contains("dedup_key"));

        let mut a = sample();
        a.tz_offset_min = 900;
        assert!(reason(a).contains("tz_offset_min"));
    }
}
