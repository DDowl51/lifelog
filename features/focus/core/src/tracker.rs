//! 焦点追踪：把焦点变化变成 `window_focus` 时段。
//!
//! 纯逻辑，不读时钟：调用方把每次变化连同「现在」一起交进来，拿回要提交的活动。
//! 同一个时段的活动都带同一个 `dedup_key`，入库后是一行。

use activity_contracts::{Activity, payload_keys};
use clock_contract::Moment;
use window_contract::WindowInfo;

use crate::TitleRules;

/// 焦点追踪器。
pub struct FocusTracker {
    rules: TitleRules,
    /// `dedup_key` 的一部分，区分不同次启动：启动时刻的毫秒数。
    session: i64,
    /// 下一个时段的序号。断线重连不重置。
    next_seq: u64,
    state: State,
}

/// 状态机的两个状态。
enum State {
    /// 没有窗口有焦点（刚启动、焦点在桌面或锁屏上、和窗口管理器的连接断了）。
    Unfocused,
    /// 焦点在某个窗口上，正在记录一个时段。
    Focused(Span),
}

/// 进行中的时段。
struct Span {
    window_id: u64,
    app_id: Option<String>,
    /// 规范化之后的标题。
    title: Option<String>,
    workspace: Option<String>,
    start: Moment,
    dedup_key: String,
}

impl Span {
    /// 这个时段到 `end` 时刻为止的样子。
    fn to_activity(&self, end: Moment) -> Activity {
        // 时钟被往回拨了也不能交出「结束早于开始」的时段
        let end_ms = end.utc_ms.max(self.start.utc_ms);
        let mut activity = Activity::new(
            "focus",
            "window_focus",
            self.start.utc_ms,
            self.start.offset_min,
        )
        .with_end(end_ms)
        .with_payload("window_id", self.window_id)
        .with_dedup_key(&self.dedup_key);
        activity.app = self.app_id.clone();
        activity.title = self.title.clone();
        if let Some(workspace) = &self.workspace {
            activity = activity.with_payload(payload_keys::WORKSPACE, workspace.as_str());
        }
        activity
    }
}

impl FocusTracker {
    /// `session_ms`：这次启动的时刻，决定这次产生的 `dedup_key`。
    pub fn new(rules: TitleRules, session_ms: i64) -> Self {
        Self {
            rules,
            session: session_ms,
            next_seq: 1,
            state: State::Unfocused,
        }
    }

    /// 正在记录的窗口。
    pub fn focused_window(&self) -> Option<u64> {
        match &self.state {
            State::Unfocused => None,
            State::Focused(span) => Some(span.window_id),
        }
    }

    /// 焦点变了（`None` = 没有窗口有焦点）。返回要提交的活动：0～2 条（关闭旧时段、打开新时段）。
    ///
    /// 还是同一个窗口、应用、规范化后的标题、所在工作区都没变：什么都不做。
    /// 所以标题里的 spinner 再怎么抖，时段也不会被切碎。
    pub fn on_focus(&mut self, window: Option<WindowInfo>, now: Moment) -> Vec<Activity> {
        let next = window.map(|w| {
            let title = w.title.as_deref().and_then(|t| self.rules.normalize(t));
            (w.id, w.app_id, title, w.workspace)
        });

        if let (State::Focused(span), Some((id, app_id, title, workspace))) = (&self.state, &next)
            && span.window_id == *id
            && span.app_id == *app_id
            && span.title == *title
            && span.workspace == *workspace
        {
            return Vec::new();
        }

        let mut out = Vec::new();
        out.extend(self.close(now));
        if let Some((window_id, app_id, title, workspace)) = next {
            let span = Span {
                window_id,
                app_id,
                title,
                workspace,
                start: now,
                dedup_key: format!("focus:{}:{}", self.session, self.next_seq),
            };
            self.next_seq += 1;
            // 打开时立刻交一次（长度 0）：之后就算崩溃，这个时段也留下了开头
            out.push(span.to_activity(now));
            self.state = State::Focused(span);
        }
        out
    }

    /// 定时调用：把进行中的时段再交一次（结束时刻 = 现在）。没有焦点时返回 `None`。
    pub fn tick(&self, now: Moment) -> Option<Activity> {
        match &self.state {
            State::Unfocused => None,
            State::Focused(span) => Some(span.to_activity(now)),
        }
    }

    /// 程序退出：结束进行中的时段。
    pub fn close(&mut self, now: Moment) -> Option<Activity> {
        match std::mem::replace(&mut self.state, State::Unfocused) {
            State::Unfocused => None,
            State::Focused(span) => Some(span.to_activity(now)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 上海时间 2026-10-20 09:00:00 之后第 `secs` 秒。
    fn at(secs: i64) -> Moment {
        Moment {
            utc_ms: 1_792_458_000_000 + secs * 1_000,
            offset_min: 480,
        }
    }

    fn window(id: u64, app: &str, title: &str) -> Option<WindowInfo> {
        Some(WindowInfo {
            id,
            app_id: Some(app.to_owned()),
            title: Some(title.to_owned()),
            workspace: None,
        })
    }

    fn tracker() -> FocusTracker {
        let rules = TitleRules::new(vec![" — Zen Browser".to_owned()]);
        FocusTracker::new(rules, at(0).utc_ms)
    }

    /// 启动后焦点在 1 号 ghostty 上。
    fn started() -> FocusTracker {
        let mut t = tracker();
        assert_eq!(
            t.on_focus(window(1, "com.mitchellh.ghostty", "◐ nvim"), at(0))
                .len(),
            1
        );
        t
    }

    fn key(a: &Activity) -> &str {
        a.dedup_key.as_deref().unwrap()
    }

    #[test]
    fn first_focus_opens_a_span() {
        let mut t = tracker();
        let out = t.on_focus(window(1, "com.mitchellh.ghostty", "◐ nvim"), at(0));
        assert_eq!(out.len(), 1);
        let a = &out[0];
        assert_eq!(
            (a.source.as_str(), a.kind.as_str()),
            ("focus", "window_focus")
        );
        assert_eq!(a.app.as_deref(), Some("com.mitchellh.ghostty"));
        assert_eq!(a.title.as_deref(), Some("nvim"), "标题已经规范化");
        assert_eq!(a.ts_start, at(0).utc_ms);
        assert_eq!(a.ts_end, Some(a.ts_start), "刚打开时长度为 0");
        assert_eq!(a.tz_offset_min, 480);
        assert_eq!(a.payload["window_id"], 1);
        assert_eq!(key(a), "focus:1792458000000:1");
        assert_eq!(t.focused_window(), Some(1));
    }

    #[test]
    fn no_focus_opens_nothing() {
        let mut t = tracker();
        assert!(t.on_focus(None, at(0)).is_empty());
        assert_eq!(t.focused_window(), None);
        assert!(t.tick(at(30)).is_none());
    }

    #[test]
    fn focus_change_closes_the_old_span_and_opens_a_new_one() {
        let mut t = started();
        let out = t.on_focus(window(2, "zen", "Rust — Zen Browser"), at(750));
        assert_eq!(out.len(), 2);
        let (closed, opened) = (&out[0], &out[1]);
        assert_eq!(closed.app.as_deref(), Some("com.mitchellh.ghostty"));
        assert_eq!(closed.ts_start, at(0).utc_ms);
        assert_eq!(closed.ts_end, Some(at(750).utc_ms));
        assert_eq!(opened.app.as_deref(), Some("zen"));
        assert_eq!(opened.title.as_deref(), Some("Rust"));
        assert_eq!(opened.ts_start, at(750).utc_ms);
        assert_ne!(key(closed), key(opened));
    }

    #[test]
    fn spinner_jitter_keeps_the_same_span() {
        let mut t = started();
        for (i, title) in ["◑ nvim", "◐ nvim", "◑ nvim"].into_iter().enumerate() {
            let now = at(1 + i64::try_from(i).unwrap());
            assert!(
                t.on_focus(window(1, "com.mitchellh.ghostty", title), now)
                    .is_empty()
            );
        }
        assert_eq!(t.focused_window(), Some(1));
    }

    #[test]
    fn a_real_title_change_starts_a_new_span() {
        let mut t = started();
        let out = t.on_focus(window(1, "com.mitchellh.ghostty", "◐ cargo test"), at(300));
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].title.as_deref(), Some("nvim"));
        assert_eq!(out[0].ts_end, Some(at(300).utc_ms));
        assert_eq!(out[1].title.as_deref(), Some("cargo test"));
        assert_ne!(key(&out[0]), key(&out[1]));
    }

    #[test]
    fn an_app_change_in_the_same_window_starts_a_new_span() {
        let mut t = started();
        assert_eq!(
            t.on_focus(window(1, "other.app", "◐ nvim"), at(10)).len(),
            2
        );
    }

    #[test]
    fn losing_focus_closes_and_later_focus_reopens() {
        let mut t = started();
        let out = t.on_focus(None, at(600));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].ts_end, Some(at(600).utc_ms));
        assert!(t.tick(at(660)).is_none());
        assert!(t.on_focus(None, at(661)).is_empty(), "已经没有焦点了");

        let out = t.on_focus(window(2, "zen", "x"), at(900));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].ts_start, at(900).utc_ms);
        assert!(key(&out[0]).ends_with(":2"), "序号接着往下走");
    }

    #[test]
    fn tick_extends_the_open_span() {
        let t = started();
        let first = t.tick(at(30)).unwrap();
        let second = t.tick(at(60)).unwrap();
        assert_eq!(key(&first), key(&second));
        assert_eq!(second.ts_start, at(0).utc_ms);
        assert_eq!(second.ts_end, Some(at(60).utc_ms));
    }

    #[test]
    fn close_ends_the_span_once() {
        let mut t = started();
        assert_eq!(t.close(at(3000)).unwrap().ts_end, Some(at(3000).utc_ms));
        assert!(t.close(at(3001)).is_none());
    }

    #[test]
    fn a_window_known_only_by_id_gets_a_span_without_app() {
        let mut t = tracker();
        let out = t.on_focus(
            Some(WindowInfo {
                id: 99,
                app_id: None,
                title: None,
                workspace: None,
            }),
            at(0),
        );
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].app.clone(), out[0].title.clone()), (None, None));
    }

    #[test]
    fn a_title_that_normalizes_to_nothing_is_recorded_as_none() {
        let mut t = tracker();
        let out = t.on_focus(window(1, "ghostty", "◐ "), at(0));
        assert_eq!(out[0].title, None);
    }

    #[test]
    fn a_clock_going_backwards_never_yields_a_negative_span() {
        let mut t = started();
        let closed = t.close(at(-5)).unwrap();
        assert_eq!(closed.ts_end, Some(closed.ts_start));
    }

    #[test]
    fn moving_to_another_workspace_starts_a_new_span() {
        let mut t = tracker();
        let on = |workspace: Option<&str>| {
            window(1, "ghostty", "claude").map(|mut w| {
                w.workspace = workspace.map(str::to_owned);
                w
            })
        };
        let first = t.on_focus(on(Some("work-a")), at(0));
        assert_eq!(
            first[0].payload.get(payload_keys::WORKSPACE),
            Some(&serde_json::json!("work-a"))
        );
        assert!(
            t.on_focus(on(Some("work-a")), at(5)).is_empty(),
            "没变就不切"
        );
        let moved = t.on_focus(on(Some("work-b")), at(10));
        assert_eq!(moved.len(), 2, "关旧的、开新的");
        let unnamed = t.on_focus(on(None), at(20));
        assert!(!unnamed[1].payload.contains_key(payload_keys::WORKSPACE));
    }
}
