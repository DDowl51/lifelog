//! 窗口表：把 niri 的消息变成「现在哪个窗口有焦点」。纯逻辑。

use std::collections::HashMap;

use window_contract::WindowInfo;

use crate::ipc::{NiriEvent, Window};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Entry {
    app_id: Option<String>,
    title: Option<String>,
    workspace_id: Option<u64>,
}

impl From<Window> for Entry {
    fn from(window: Window) -> Self {
        Self {
            app_id: window.app_id,
            title: window.title,
            workspace_id: window.workspace_id,
        }
    }
}

/// niri 不会在每条消息里都带上完整的窗口信息（切焦点时只给编号），所以要自己记着所有窗口。
#[derive(Default)]
pub(crate) struct Table {
    windows: HashMap<u64, Entry>,
    /// 工作区编号 → 名字（没命名的是 `None`）。
    workspaces: HashMap<u64, Option<String>>,
    focused: Option<u64>,
    /// 上一次报告出去的焦点。
    reported: Option<WindowInfo>,
}

impl Table {
    /// 现在有焦点的窗口。
    pub(crate) fn current(&self) -> Option<WindowInfo> {
        self.focused.map(|id| {
            // 焦点切到一个没见过的窗口：只知道编号
            let entry = self.windows.get(&id).cloned().unwrap_or_default();
            let workspace = entry
                .workspace_id
                .and_then(|w| self.workspaces.get(&w).cloned().flatten());
            WindowInfo {
                id,
                app_id: entry.app_id,
                title: entry.title,
                workspace,
            }
        })
    }

    /// 处理一条消息。有焦点的窗口换了、或者它的应用标识 / 标题变了，返回新的焦点；
    /// 没变化返回 `None`。
    pub(crate) fn apply(&mut self, event: NiriEvent) -> Option<Option<WindowInfo>> {
        match event {
            NiriEvent::WindowsChanged { windows } => {
                self.windows.clear();
                self.focused = None;
                for window in windows {
                    if window.is_focused {
                        self.focused = Some(window.id);
                    }
                    self.windows.insert(window.id, window.into());
                }
            }
            NiriEvent::WindowOpenedOrChanged { window } => {
                if window.is_focused {
                    self.focused = Some(window.id);
                }
                self.windows.insert(window.id, window.into());
            }
            NiriEvent::WindowClosed { id } => {
                self.windows.remove(&id);
                if self.focused == Some(id) {
                    self.focused = None;
                }
            }
            NiriEvent::WindowFocusChanged { id } => self.focused = id,
            NiriEvent::WorkspacesChanged { workspaces } => {
                self.workspaces = workspaces.into_iter().map(|w| (w.id, w.name)).collect();
            }
        }
        self.report()
    }

    /// 连接断了：之前知道的都作废。之前有焦点的话，报告「没有焦点」。
    pub(crate) fn disconnect(&mut self) -> Option<Option<WindowInfo>> {
        self.windows.clear();
        self.workspaces.clear();
        self.focused = None;
        self.report()
    }

    fn report(&mut self) -> Option<Option<WindowInfo>> {
        let now = self.current();
        if now == self.reported {
            return None;
        }
        self.reported = now.clone();
        Some(now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::Window;

    fn window(id: u64, app: &str, title: &str, focused: bool) -> Window {
        Window {
            id,
            title: Some(title.to_owned()),
            app_id: Some(app.to_owned()),
            is_focused: focused,
            workspace_id: None,
        }
    }

    fn info(id: u64, app: &str, title: &str) -> WindowInfo {
        WindowInfo {
            id,
            app_id: Some(app.to_owned()),
            title: Some(title.to_owned()),
            workspace: None,
        }
    }

    fn changed(id: u64, app: &str, title: &str, focused: bool) -> NiriEvent {
        NiriEvent::WindowOpenedOrChanged {
            window: window(id, app, title, focused),
        }
    }

    /// 启动时的快照：1 号 ghostty 有焦点，2 号 zen。
    fn started() -> Table {
        let mut t = Table::default();
        let out = t.apply(NiriEvent::WindowsChanged {
            windows: vec![
                window(1, "ghostty", "◐ nvim", true),
                window(2, "zen", "Rust", false),
            ],
        });
        assert_eq!(out, Some(Some(info(1, "ghostty", "◐ nvim"))));
        t
    }

    #[test]
    fn snapshot_without_focus_reports_nothing() {
        let mut t = Table::default();
        let out = t.apply(NiriEvent::WindowsChanged {
            windows: vec![window(1, "a", "x", false)],
        });
        assert_eq!(out, None);
        assert_eq!(t.current(), None);
    }

    #[test]
    fn focus_change_reports_the_new_window() {
        let mut t = started();
        assert_eq!(
            t.apply(NiriEvent::WindowFocusChanged { id: Some(2) }),
            Some(Some(info(2, "zen", "Rust")))
        );
        // 再说一遍同一个窗口：没变化
        assert_eq!(t.apply(NiriEvent::WindowFocusChanged { id: Some(2) }), None);
    }

    #[test]
    fn title_change_of_the_focused_window_is_reported_raw() {
        let mut t = started();
        // 标题原样报告，抖动也报：要不要当回事由订阅方决定
        assert_eq!(
            t.apply(changed(1, "ghostty", "◑ nvim", true)),
            Some(Some(info(1, "ghostty", "◑ nvim")))
        );
        assert_eq!(t.apply(changed(1, "ghostty", "◑ nvim", true)), None);
    }

    #[test]
    fn background_window_changes_are_not_reported_but_remembered() {
        let mut t = started();
        assert_eq!(t.apply(changed(2, "zen", "Other", false)), None);
        // 之后切过去，用的是更新后的标题
        assert_eq!(
            t.apply(NiriEvent::WindowFocusChanged { id: Some(2) }),
            Some(Some(info(2, "zen", "Other")))
        );
    }

    #[test]
    fn a_new_focused_window_takes_focus() {
        let mut t = started();
        assert_eq!(
            t.apply(changed(3, "keepassxc", "KeePassXC", true)),
            Some(Some(info(3, "keepassxc", "KeePassXC")))
        );
    }

    #[test]
    fn closing_the_focused_window_reports_no_focus() {
        let mut t = started();
        assert_eq!(
            t.apply(NiriEvent::WindowClosed { id: 2 }),
            None,
            "关掉别的窗口不影响"
        );
        assert_eq!(t.apply(NiriEvent::WindowClosed { id: 1 }), Some(None));
        assert_eq!(t.current(), None);
    }

    #[test]
    fn focus_none_then_back() {
        let mut t = started();
        assert_eq!(
            t.apply(NiriEvent::WindowFocusChanged { id: None }),
            Some(None)
        );
        assert_eq!(
            t.apply(NiriEvent::WindowFocusChanged { id: Some(1) }),
            Some(Some(info(1, "ghostty", "◐ nvim")))
        );
    }

    #[test]
    fn unknown_window_is_reported_with_only_its_id() {
        let mut t = Table::default();
        assert_eq!(
            t.apply(NiriEvent::WindowFocusChanged { id: Some(99) }),
            Some(Some(WindowInfo {
                id: 99,
                app_id: None,
                title: None,
                workspace: None,
            }))
        );
    }

    #[test]
    fn disconnect_reports_no_focus_once_and_forgets_windows() {
        let mut t = started();
        assert_eq!(t.disconnect(), Some(None));
        assert_eq!(t.disconnect(), None);
        // 重连后的快照重新报告
        let out = t.apply(NiriEvent::WindowsChanged {
            windows: vec![window(1, "ghostty", "◑ nvim", true)],
        });
        assert_eq!(out, Some(Some(info(1, "ghostty", "◑ nvim"))));
    }

    #[test]
    fn the_workspace_name_comes_along_and_changes_are_reported() {
        use crate::ipc::Workspace;

        let mut t = Table::default();
        let named = |name: Option<&str>| NiriEvent::WorkspacesChanged {
            workspaces: vec![
                Workspace {
                    id: 7,
                    name: name.map(str::to_owned),
                },
                Workspace { id: 8, name: None },
            ],
        };
        t.apply(named(Some("work-weather")));
        let mut w = window(1, "ghostty", "claude", true);
        w.workspace_id = Some(7);
        let out = t.apply(NiriEvent::WindowsChanged {
            windows: vec![w.clone()],
        });
        let workspace = |out: Option<Option<WindowInfo>>| out.unwrap().unwrap().workspace;
        assert_eq!(workspace(out), Some("work-weather".to_owned()));

        // 改名
        let out = t.apply(named(Some("work-station")));
        assert_eq!(workspace(out), Some("work-station".to_owned()));
        // 名字没变：不报
        assert_eq!(t.apply(named(Some("work-station"))), None);

        // 窗口挪到没命名的工作区
        w.workspace_id = Some(8);
        let out = t.apply(NiriEvent::WindowOpenedOrChanged { window: w });
        assert_eq!(workspace(out), None);
        assert_eq!(t.current().unwrap().id, 1);
    }
}
