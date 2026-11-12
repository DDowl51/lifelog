//! `afk` 的纯逻辑：把「离开了 / 回来了」变成离开时段。
//!
//! 时段的写法和 `focus` 一样：打开时写一次，定时刷新，关闭时写最后一次，
//! 同一个时段用同一个 `dedup_key`，入库后是一行。

use activity_contracts::Activity;
use clock_contract::Moment;
use idle_contract::IdleChanged;

pub const SOURCE: &str = "afk";
pub const KIND: &str = "away";

struct Open {
    start: Moment,
    dedup_key: String,
}

impl Open {
    fn to_activity(&self, end: Moment) -> Activity {
        Activity::new(SOURCE, KIND, self.start.utc_ms, self.start.offset_min)
            .with_end(end.utc_ms.max(self.start.utc_ms))
            .with_dedup_key(&self.dedup_key)
    }
}

/// 离开追踪器。
pub struct AwayTracker {
    session: i64,
    next_seq: u64,
    open: Option<Open>,
}

impl AwayTracker {
    /// `session_ms`：这次启动的时刻，决定这次产生的 `dedup_key`。
    pub fn new(session_ms: i64) -> Self {
        Self {
            session: session_ms,
            next_seq: 1,
            open: None,
        }
    }

    pub fn is_away(&self) -> bool {
        self.open.is_some()
    }

    /// 状态变了。返回要提交的活动（0 或 1 条）。
    pub fn on_change(&mut self, change: IdleChanged) -> Option<Activity> {
        match change {
            IdleChanged::Away { since, at } => {
                if self.open.is_some() {
                    return None;
                }
                let open = Open {
                    start: since,
                    dedup_key: format!("afk:{}:{}", self.session, self.next_seq),
                };
                self.next_seq += 1;
                let activity = open.to_activity(at);
                self.open = Some(open);
                Some(activity)
            }
            IdleChanged::Back { at } => self.close(at),
        }
    }

    /// 定时调用：把进行中的离开时段再交一次。
    pub fn tick(&self, now: Moment) -> Option<Activity> {
        self.open.as_ref().map(|open| open.to_activity(now))
    }

    /// 回来了，或者程序退出：结束进行中的离开时段。
    pub fn close(&mut self, now: Moment) -> Option<Activity> {
        self.open.take().map(|open| open.to_activity(now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: i64) -> Moment {
        Moment {
            utc_ms: 1_000_000 + secs * 1_000,
            offset_min: 480,
        }
    }

    fn span(a: &Activity) -> (i64, i64, &str) {
        (
            (a.ts_start - 1_000_000) / 1_000,
            (a.ts_end.unwrap() - 1_000_000) / 1_000,
            a.dedup_key.as_deref().unwrap(),
        )
    }

    #[test]
    fn away_then_back_is_one_span_from_the_last_input() {
        let mut t = AwayTracker::new(7);
        let opened = t
            .on_change(IdleChanged::Away {
                since: at(0),
                at: at(300),
            })
            .unwrap();
        assert_eq!(span(&opened), (0, 300, "afk:7:1"));
        assert_eq!(
            (opened.source.as_str(), opened.kind.as_str()),
            ("afk", "away")
        );
        assert!(t.is_away());

        assert_eq!(span(&t.tick(at(330)).unwrap()), (0, 330, "afk:7:1"));
        assert_eq!(
            span(&t.on_change(IdleChanged::Back { at: at(400) }).unwrap()),
            (0, 400, "afk:7:1")
        );
        assert!(!t.is_away());
        assert!(t.tick(at(430)).is_none());
    }

    #[test]
    fn repeated_states_are_ignored_and_sequence_continues() {
        let mut t = AwayTracker::new(7);
        assert!(
            t.on_change(IdleChanged::Back { at: at(1) }).is_none(),
            "本来就在"
        );
        t.on_change(IdleChanged::Away {
            since: at(0),
            at: at(300),
        });
        assert!(
            t.on_change(IdleChanged::Away {
                since: at(10),
                at: at(310)
            })
            .is_none(),
            "已经离开了"
        );
        t.on_change(IdleChanged::Back { at: at(400) });
        let second = t
            .on_change(IdleChanged::Away {
                since: at(500),
                at: at(800),
            })
            .unwrap();
        assert_eq!(span(&second).2, "afk:7:2");
    }

    #[test]
    fn close_on_exit() {
        let mut t = AwayTracker::new(7);
        t.on_change(IdleChanged::Away {
            since: at(0),
            at: at(300),
        });
        assert_eq!(span(&t.close(at(900)).unwrap()), (0, 900, "afk:7:1"));
        assert!(t.close(at(901)).is_none());
    }

    #[test]
    fn never_ends_before_it_starts() {
        let mut t = AwayTracker::new(7);
        let a = t
            .on_change(IdleChanged::Away {
                since: at(300),
                at: at(0),
            })
            .unwrap();
        assert_eq!(a.ts_end, Some(a.ts_start));
    }
}
