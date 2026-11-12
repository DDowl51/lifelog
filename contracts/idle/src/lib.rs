//! 契约：报告用户离开了还是回来了。
//!
//! 「多久没动算离开」是提供者的配置。提供者知道这个时长，所以由它算出离开从什么时候开始。

use clock_contract::Moment;
use kernel_api::{AppError, EventDef, contract};
use serde::{Deserialize, Serialize};

/// 用户在不在。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum IdleChanged {
    /// 在 `at` 时刻发现用户离开了；最后一次操作在 `since`（`since` ≤ `at`）。
    Away { since: Moment, at: Moment },
    /// 用户在 `at` 时刻回来了。
    Back { at: Moment },
}

contract! {
    /// 用户在不在。
    pub trait Idle as IDLE = service("idle", 1) {
        /// 现在是不是离开状态。
        async fn away(&self) -> Result<bool, AppError>;
    }
}

/// 事件：用户离开了，或者回来了。同一个状态不会连着报两次。
pub static IDLE_CHANGED: EventDef<IdleChanged> = EventDef::new("idle", "changed");

#[cfg(feature = "testing")]
pub mod testing {
    //! 手动控制的假实现。

    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, OnceLock};

    use clock_contract::Moment;
    use kernel_api::{
        AppError, Component, ConfigSection, Host, Kind, Manifest, Publisher, async_trait,
    };

    use super::{IDLE, IDLE_CHANGED, Idle, IdleChanged};

    /// 测试说离开就离开。克隆出来的是同一个。
    #[derive(Clone, Default)]
    pub struct ManualIdle {
        away: Arc<AtomicBool>,
        publisher: Arc<OnceLock<Publisher<IdleChanged>>>,
    }

    impl ManualIdle {
        async fn publish(&self, change: IdleChanged) -> Result<(), AppError> {
            let publisher = self
                .publisher
                .get()
                .ok_or_else(|| AppError::unavailable("idle-manual 还没启动"))?;
            publisher.publish(change).await
        }

        /// 在 `at` 发现离开了，最后一次操作在 `since`。
        pub async fn away(&self, since: Moment, at: Moment) -> Result<(), AppError> {
            self.away.store(true, Ordering::Relaxed);
            self.publish(IdleChanged::Away { since, at }).await
        }

        /// 在 `at` 回来了。
        pub async fn back(&self, at: Moment) -> Result<(), AppError> {
            self.away.store(false, Ordering::Relaxed);
            self.publish(IdleChanged::Back { at }).await
        }
    }

    #[async_trait]
    impl Idle for ManualIdle {
        async fn away(&self) -> Result<bool, AppError> {
            Ok(self.away.load(Ordering::Relaxed))
        }
    }

    /// 组件 `idle-manual`。
    impl Component for ManualIdle {
        fn manifest(&self) -> Manifest {
            Manifest {
                id: "idle-manual",
                kind: Kind::Service,
                summary: "测试用：手动控制离开和回来",
                provides: vec![IDLE.id()],
                requires: vec![],
                optional_requires: vec![],
            }
        }

        fn start(&self, _config: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
            let _ = self.publisher.set(host.publisher(&IDLE_CHANGED)?);
            host.provide(&IDLE, Arc::new(self.clone()))
        }
    }
}
