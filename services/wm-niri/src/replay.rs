//! 回放：不连 socket，由调用方把录制下来的 niri 消息一行一行喂进来。
//!
//! 是 `Windows` 的另一个实现（组件 `wm-niri-replay`），测试和演示用。
//! 消息解析和窗口表与真实连接用的是同一份代码。

use std::sync::{Arc, OnceLock};

use clock_contract::Moment;
use kernel_api::{AppError, Component, ConfigSection, Host, Kind, Manifest};
use window_contract::{FOCUS_CHANGED, WINDOWS};

use crate::{Engine, Service};

/// 回放用的窗口管理器。克隆出来的是同一个。
#[derive(Clone, Default)]
pub struct NiriReplay {
    engine: Arc<OnceLock<Arc<Engine>>>,
}

impl NiriReplay {
    fn engine(&self) -> Result<&Arc<Engine>, AppError> {
        self.engine
            .get()
            .ok_or_else(|| AppError::unavailable("wm-niri-replay 还没启动"))
    }

    /// 在 `at` 时刻收到 niri 的一行消息。返回这一行是不是我们关心的事件。
    pub async fn feed(&self, at: Moment, line: &str) -> Result<bool, AppError> {
        self.engine()?.line(line, at).await
    }

    /// 在 `at` 时刻连接断了。
    pub async fn disconnect(&self, at: Moment) -> Result<(), AppError> {
        self.engine()?.disconnected(at).await
    }
}

impl Component for NiriReplay {
    fn manifest(&self) -> Manifest {
        Manifest {
            id: "wm-niri-replay",
            kind: Kind::Service,
            summary: "回放录制的 niri 消息",
            provides: vec![WINDOWS.id()],
            requires: vec![],
            optional_requires: vec![],
        }
    }

    fn start(&self, _config: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
        let engine = Arc::new(Engine::new(host.publisher(&FOCUS_CHANGED)?));
        let _ = self.engine.set(engine.clone());
        host.provide(&WINDOWS, Arc::new(Service(engine)))
    }
}
