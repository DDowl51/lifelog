//! 系统服务 `timer-tokio`：用 tokio 的时钟提供 `Timer`。

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use kernel_api::{
    AppError, Cancel, Component, ConfigSection, Host, Kind, Manifest, Publisher, async_trait,
    register_component,
};
use serde::{Deserialize, Serialize};
use timer_contract::{Fired, TIMER, TIMER_FIRED, Timer, TimerId};
use tokio::sync::mpsc;
use tokio::time::Instant;

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Config {}

enum Command {
    Add(TimerId, Duration),
    Cancel(TimerId),
}

struct Service {
    next_id: AtomicU64,
    commands: mpsc::UnboundedSender<Command>,
}

impl Service {
    fn send(&self, command: Command) -> Result<(), AppError> {
        self.commands
            .send(command)
            .map_err(|_| AppError::unavailable("定时器已经停止"))
    }
}

#[async_trait]
impl Timer for Service {
    async fn every(&self, period_ms: u64) -> Result<TimerId, AppError> {
        if period_ms == 0 {
            return Err(AppError::validation("间隔不能是 0"));
        }
        let id = TimerId(self.next_id.fetch_add(1, Ordering::Relaxed));
        self.send(Command::Add(id, Duration::from_millis(period_ms)))?;
        Ok(id)
    }

    async fn cancel(&self, id: TimerId) -> Result<(), AppError> {
        self.send(Command::Cancel(id))
    }
}

struct Entry {
    id: TimerId,
    period: Duration,
    next: Instant,
}

async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// 一个任务管所有定时器：睡到最近的那个到点为止。
async fn run(
    mut commands: mpsc::UnboundedReceiver<Command>,
    fired: Publisher<Fired>,
    cancel: Cancel,
) {
    let mut timers: Vec<Entry> = Vec::new();
    loop {
        let nearest = timers.iter().map(|t| t.next).min();
        tokio::select! {
            () = cancel.cancelled() => break,
            command = commands.recv() => match command {
                Some(Command::Add(id, period)) => {
                    timers.push(Entry { id, period, next: Instant::now() + period });
                }
                Some(Command::Cancel(id)) => timers.retain(|t| t.id != id),
                None => break,
            },
            () = sleep_until(nearest) => {
                let now = Instant::now();
                for timer in timers.iter_mut().filter(|t| t.next <= now) {
                    // 从现在起算下一次：处理不过来时不补发
                    timer.next = now + timer.period;
                    if fired.publish(Fired { id: timer.id }).await.is_err() {
                        return;
                    }
                }
            }
        }
    }
}

pub struct TimerTokio;

impl Component for TimerTokio {
    fn config_defaults(&self) -> Option<ConfigSection> {
        ConfigSection::defaults::<Config>()
    }

    fn manifest(&self) -> Manifest {
        Manifest {
            id: "timer-tokio",
            kind: Kind::Service,
            summary: "按固定间隔触发",
            provides: vec![TIMER.id()],
            requires: vec![],
            optional_requires: vec![],
        }
    }

    fn start(&self, config: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
        let _: Config = config.parse()?;
        let (tx, rx) = mpsc::unbounded_channel();
        let fired = host.publisher(&TIMER_FIRED)?;
        host.spawn("timers", move |cancel| run(rx, fired, cancel));
        host.provide(
            &TIMER,
            Arc::new(Service {
                next_id: AtomicU64::new(1),
                commands: tx,
            }),
        )
    }
}

register_component!(TimerTokio);

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use timer_contract::testing::timer_contract;

    use super::*;

    /// 记下收到的触发。
    struct Listener(Arc<Mutex<Vec<TimerId>>>);

    impl Component for Listener {
        fn manifest(&self) -> Manifest {
            Manifest {
                id: "listener",
                kind: Kind::Feature,
                summary: "测试用",
                provides: vec![],
                requires: vec![TIMER.id()],
                optional_requires: vec![],
            }
        }

        fn start(&self, _: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
            let seen = self.0.clone();
            host.subscribe(&TIMER_FIRED, "note", move |fired: Fired| {
                let seen = seen.clone();
                async move {
                    seen.lock().unwrap().push(fired.id);
                    Ok(())
                }
            })
        }
    }

    #[tokio::test]
    async fn obeys_the_timer_contract() {
        let app = testkit::app().with(TimerTokio).boot().await.unwrap();
        timer_contract(app.resolve(&TIMER).unwrap()).await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn fires_every_period_until_cancelled() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let app = testkit::app()
            .with(TimerTokio)
            .with(Listener(seen.clone()))
            .background_delivery()
            .boot()
            .await
            .unwrap();
        let timer = app.resolve(&TIMER).unwrap();

        let fast = timer.every(1_000).await.unwrap();
        let slow = timer.every(2_500).await.unwrap();
        tokio::time::sleep(Duration::from_millis(3_200)).await;
        app.settle().await;
        // 1s、2s、2.5s、3s
        assert_eq!(*seen.lock().unwrap(), [fast, fast, slow, fast]);

        timer.cancel(fast).await.unwrap();
        tokio::time::sleep(Duration::from_millis(2_000)).await;
        app.settle().await;
        assert_eq!(
            seen.lock().unwrap().len(),
            5,
            "取消之后只剩慢的那个在 5s 触发一次"
        );
        assert_eq!(seen.lock().unwrap()[4], slow);
        app.stop().await;
    }
}
