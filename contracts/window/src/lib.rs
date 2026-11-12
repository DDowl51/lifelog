//! 契约：报告哪个窗口有焦点。
//!
//! 描述的是能力，不是某个窗口管理器的协议：这里没有任何窗口管理器的名字。
//! 换窗口管理器 = 写另一个提供这个契约的系统服务。

use clock_contract::Moment;
use kernel_api::{AppError, EventDef, contract};
use serde::{Deserialize, Serialize};

/// 一个窗口。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowInfo {
    /// 窗口管理器给的编号。只在它的一次运行期间唯一。
    pub id: u64,
    /// 应用的标识，例如 `com.mitchellh.ghostty`。
    pub app_id: Option<String>,
    /// 窗口标题，原样，不做任何清理。
    pub title: Option<String>,
    /// 所在工作区的名字。没有命名的工作区、或者窗口管理器没有工作区这个概念时是 `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
}

/// 焦点变了。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FocusChanged {
    /// 发生的时刻。
    pub at: Moment,
    /// 现在有焦点的窗口；`None` 表示没有窗口有焦点，或者和窗口管理器的连接断了。
    pub window: Option<WindowInfo>,
}

contract! {
    /// 窗口焦点。
    pub trait Windows as WINDOWS = service("windows", 1) {
        /// 现在有焦点的窗口。
        async fn focused(&self) -> Result<Option<WindowInfo>, AppError>;
    }
}

/// 事件：有焦点的窗口换了，或者它的应用标识、标题、所在工作区变了。
///
/// 标题每变一次就发一次（有的终端每秒都在改标题），要不要当回事由订阅方决定。
pub static FOCUS_CHANGED: EventDef<FocusChanged> = EventDef::new("windows", "focus_changed");

#[cfg(any(test, feature = "testing"))]
pub mod testing {
    //! 手动控制的假窗口管理器。

    use std::sync::{Arc, Mutex, OnceLock, PoisonError};

    use clock_contract::Moment;
    use kernel_api::{
        AppError, Component, ConfigSection, Host, Kind, Manifest, Publisher, async_trait,
    };

    use super::{FOCUS_CHANGED, FocusChanged, WINDOWS, WindowInfo, Windows};

    /// 假窗口管理器：测试说焦点在哪就在哪。克隆出来的是同一个。
    #[derive(Clone, Default)]
    pub struct FakeWindows {
        current: Arc<Mutex<Option<WindowInfo>>>,
        publisher: Arc<OnceLock<Publisher<FocusChanged>>>,
    }

    impl FakeWindows {
        /// 把焦点换到 `window`，并发布事件。要在启动之后调用。
        pub async fn focus(&self, at: Moment, window: Option<WindowInfo>) -> Result<(), AppError> {
            *self.current.lock().unwrap_or_else(PoisonError::into_inner) = window.clone();
            let publisher = self
                .publisher
                .get()
                .ok_or_else(|| AppError::unavailable("windows-fake 还没启动"))?;
            publisher.publish(FocusChanged { at, window }).await
        }
    }

    #[async_trait]
    impl Windows for FakeWindows {
        async fn focused(&self) -> Result<Option<WindowInfo>, AppError> {
            Ok(self
                .current
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone())
        }
    }

    /// 组件 `windows-fake`。
    impl Component for FakeWindows {
        fn manifest(&self) -> Manifest {
            Manifest {
                id: "windows-fake",
                kind: Kind::Service,
                summary: "测试用：手动控制的假窗口管理器",
                provides: vec![WINDOWS.id()],
                requires: vec![],
                optional_requires: vec![],
            }
        }

        fn start(&self, _config: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
            let _ = self.publisher.set(host.publisher(&FOCUS_CHANGED)?);
            host.provide(&WINDOWS, Arc::new(self.clone()))
        }
    }

    /// 造一个窗口，省得每次写全字段。
    pub fn window(id: u64, app_id: &str, title: &str) -> WindowInfo {
        WindowInfo {
            id,
            app_id: Some(app_id.to_owned()),
            title: Some(title.to_owned()),
            workspace: None,
        }
    }
}
