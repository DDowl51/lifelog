//! 契约：现在几点。
//!
//! 组件不直接读系统时钟，都经过这个契约，测试时才能换成可以拨动的假时钟。

use kernel_api::{AppError, contract};
use serde::{Deserialize, Serialize};

/// 一个时刻，以及当时本地时间相对 UTC 的偏移。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Moment {
    /// 自 1970-01-01T00:00:00Z 起的毫秒数。
    pub utc_ms: i64,
    /// 本地时间比 UTC 快多少分钟（东八区是 480）。
    pub offset_min: i32,
}

contract! {
    /// 现在几点。
    pub trait Clock as CLOCK = service("clock", 1) {
        /// 当前时刻。同一个提供者先后两次调用，后一次不早于前一次。
        async fn now(&self) -> Result<Moment, AppError>;
    }
}

#[cfg(any(test, feature = "testing"))]
pub mod testing {
    //! 假时钟和契约测试。

    use std::sync::{Arc, Mutex, PoisonError};

    use kernel_api::{AppError, Component, ConfigSection, Host, Kind, Manifest, async_trait};

    use super::{CLOCK, Clock, Moment};

    /// 可以拨动的时钟。克隆出来的是同一个钟。
    #[derive(Clone)]
    pub struct FakeClock(Arc<Mutex<Moment>>);

    impl FakeClock {
        pub fn at(utc_ms: i64, offset_min: i32) -> Self {
            Self(Arc::new(Mutex::new(Moment { utc_ms, offset_min })))
        }

        pub fn get(&self) -> Moment {
            *self.0.lock().unwrap_or_else(PoisonError::into_inner)
        }

        /// 拨到某个时刻。
        pub fn set(&self, utc_ms: i64) {
            self.0.lock().unwrap_or_else(PoisonError::into_inner).utc_ms = utc_ms;
        }

        /// 往前拨。
        pub fn advance_ms(&self, ms: i64) {
            self.0.lock().unwrap_or_else(PoisonError::into_inner).utc_ms += ms;
        }
    }

    #[async_trait]
    impl Clock for FakeClock {
        async fn now(&self) -> Result<Moment, AppError> {
            Ok(self.get())
        }
    }

    /// 组件 `clock-fake`：用假时钟提供 `Clock`。
    impl Component for FakeClock {
        fn manifest(&self) -> Manifest {
            Manifest {
                id: "clock-fake",
                kind: Kind::Service,
                summary: "测试用的假时钟",
                provides: vec![CLOCK.id()],
                requires: vec![],
                optional_requires: vec![],
            }
        }

        fn start(&self, _config: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
            host.provide(&CLOCK, Arc::new(self.clone()))
        }
    }

    /// `Clock` 的契约测试。每个实现都要跑。
    pub async fn clock_contract(clock: Arc<dyn Clock>) -> Result<(), AppError> {
        let first = clock.now().await?;
        let second = clock.now().await?;
        assert!(
            second.utc_ms >= first.utc_ms,
            "时间不能倒退：{first:?} → {second:?}"
        );
        // 世界上最大的偏移是 ±14 小时
        assert!(
            (-14 * 60..=14 * 60).contains(&first.offset_min),
            "偏移不合理：{first:?}"
        );
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[tokio::test]
        async fn fake_clock_obeys_the_contract() {
            let clock = FakeClock::at(1_000, 480);
            clock_contract(Arc::new(clock.clone())).await.unwrap();
            clock.advance_ms(500);
            assert_eq!(
                clock.now().await.unwrap(),
                Moment {
                    utc_ms: 1_500,
                    offset_min: 480
                }
            );
        }
    }
}
