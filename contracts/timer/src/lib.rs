//! 契约：按固定间隔触发。
//!
//! 只管「每隔多久」。「每天几点做一次、错过要补」不归它：那种任务交给进程外的调度器
//! （systemd timer）去执行一条命令。

use kernel_api::{AppError, EventDef, contract};
use serde::{Deserialize, Serialize};

/// 一个定时器的编号。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TimerId(pub u64);

/// 某个定时器到点了。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fired {
    pub id: TimerId,
}

contract! {
    /// 定时器。
    pub trait Timer as TIMER = service("timer", 1) {
        /// 登记一个每隔 `period_ms` 毫秒触发一次的定时器。第一次触发在一个间隔之后。
        /// `period_ms` 为 0 时返回 `validation`。
        async fn every(&self, period_ms: u64) -> Result<TimerId, AppError>;

        /// 取消。取消一个不存在的定时器不算错。
        async fn cancel(&self, id: TimerId) -> Result<(), AppError>;
    }
}

/// 事件：有定时器到点。所有订阅者都会收到每个定时器的事件，自己按 `id` 认领。
///
/// 处理不过来时不会补发：下一次触发在「处理完的时刻 + 间隔」。
pub static TIMER_FIRED: EventDef<Fired> = EventDef::new("timer", "fired");

#[cfg(feature = "testing")]
pub mod testing {
    //! 手动触发的假定时器和契约测试。

    use std::sync::{Arc, Mutex, OnceLock, PoisonError};

    use kernel_api::{
        AppError, Component, ConfigSection, ErrorKind, Host, Kind, Manifest, Publisher, async_trait,
    };

    use super::{Fired, TIMER, TIMER_FIRED, Timer, TimerId};

    /// 假定时器：只有测试调用 [`ManualTimer::fire_all`] 时才触发。克隆出来的是同一个。
    #[derive(Clone, Default)]
    pub struct ManualTimer {
        timers: Arc<Mutex<Vec<(TimerId, u64)>>>,
        publisher: Arc<OnceLock<Publisher<Fired>>>,
    }

    impl ManualTimer {
        /// 登记在册的定时器和各自的间隔（毫秒）。
        pub fn registered(&self) -> Vec<(TimerId, u64)> {
            self.timers
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        }

        /// 让每个定时器各触发一次。
        pub async fn fire_all(&self) -> Result<(), AppError> {
            let publisher = self
                .publisher
                .get()
                .ok_or_else(|| AppError::unavailable("timer-manual 还没启动"))?;
            for (id, _) in self.registered() {
                publisher.publish(Fired { id }).await?;
            }
            Ok(())
        }
    }

    #[async_trait]
    impl Timer for ManualTimer {
        async fn every(&self, period_ms: u64) -> Result<TimerId, AppError> {
            if period_ms == 0 {
                return Err(AppError::validation("间隔不能是 0"));
            }
            let mut timers = self.timers.lock().unwrap_or_else(PoisonError::into_inner);
            let id = TimerId(timers.iter().map(|(id, _)| id.0).max().unwrap_or(0) + 1);
            timers.push((id, period_ms));
            Ok(id)
        }

        async fn cancel(&self, id: TimerId) -> Result<(), AppError> {
            self.timers
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .retain(|(t, _)| *t != id);
            Ok(())
        }
    }

    /// 组件 `timer-manual`。
    impl Component for ManualTimer {
        fn manifest(&self) -> Manifest {
            Manifest {
                id: "timer-manual",
                kind: Kind::Service,
                summary: "测试用：手动触发的假定时器",
                provides: vec![TIMER.id()],
                requires: vec![],
                optional_requires: vec![],
            }
        }

        fn start(&self, _config: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
            let _ = self.publisher.set(host.publisher(&TIMER_FIRED)?);
            host.provide(&TIMER, Arc::new(self.clone()))
        }
    }

    /// `Timer` 的契约测试（不涉及时间流逝的部分）。每个实现都要跑。
    pub async fn timer_contract(timer: Arc<dyn Timer>) -> Result<(), AppError> {
        assert_eq!(
            timer.every(0).await.err().map(|e| e.kind),
            Some(ErrorKind::Validation)
        );
        let a = timer.every(60_000).await?;
        let b = timer.every(60_000).await?;
        assert_ne!(a, b, "每个定时器的编号不同");
        timer.cancel(a).await?;
        timer.cancel(a).await?;
        timer.cancel(TimerId(u64::MAX)).await?;
        timer.cancel(b).await?;
        Ok(())
    }
}
