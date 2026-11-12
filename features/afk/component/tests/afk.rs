//! `afk` 的组件测试，全用假实现。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use activity_contracts::testing::RecordingWrite;
use afk::AfkComponent;
use clock_contract::Moment;
use clock_contract::testing::FakeClock;
use idle_contract::testing::ManualIdle;
use log_contract::testing::MemoryLog;
use serde_json::json;
use testkit::TestApp;
use timer_contract::testing::ManualTimer;

const BASE: i64 = 1_792_458_000_000;

fn at(secs: i64) -> Moment {
    Moment {
        utc_ms: BASE + secs * 1_000,
        offset_min: 480,
    }
}

struct Fakes {
    app: TestApp,
    idle: ManualIdle,
    timer: ManualTimer,
    clock: FakeClock,
    recorded: RecordingWrite,
}

async fn fakes() -> Fakes {
    let (idle, timer, clock, recorded) = (
        ManualIdle::default(),
        ManualTimer::default(),
        FakeClock::at(BASE, 480),
        RecordingWrite::default(),
    );
    let app = testkit::app()
        .with(idle.clone())
        .with(timer.clone())
        .with(clock.clone())
        .with(MemoryLog::default())
        .with(recorded.clone())
        .with(AfkComponent)
        .config("afk", json!({ "flush_interval_secs": 10 }))
        .unwrap()
        .boot()
        .await
        .unwrap();
    Fakes {
        app,
        idle,
        timer,
        clock,
        recorded,
    }
}

fn spans(recorded: &RecordingWrite) -> Vec<(i64, i64, String)> {
    recorded
        .stored()
        .into_iter()
        .map(|a| {
            (
                (a.ts_start - BASE) / 1_000,
                (a.ts_end.unwrap() - BASE) / 1_000,
                a.source,
            )
        })
        .collect()
}

#[tokio::test]
async fn records_away_spans_and_flushes_them() {
    let f = fakes().await;
    f.idle.away(at(0), at(300)).await.unwrap();
    f.app.settle().await;
    f.clock.set(at(310).utc_ms);
    f.timer.fire_all().await.unwrap();
    f.app.settle().await;
    assert_eq!(spans(&f.recorded), [(0, 310, "afk".to_owned())]);

    f.idle.back(at(400)).await.unwrap();
    f.app.settle().await;
    f.timer.fire_all().await.unwrap();
    f.app.settle().await;
    assert_eq!(spans(&f.recorded), [(0, 400, "afk".to_owned())]);
    assert_eq!(f.recorded.submitted().len(), 3, "回来之后定时器不再写");
}

#[tokio::test]
async fn stopping_while_away_closes_the_span() {
    let f = fakes().await;
    f.idle.away(at(0), at(300)).await.unwrap();
    f.app.settle().await;
    f.clock.set(at(1000).utc_ms);
    let recorded = f.recorded.clone();
    f.app.stop().await;
    assert_eq!(spans(&recorded), [(0, 1000, "afk".to_owned())]);
}

#[tokio::test]
async fn cannot_start_without_an_idle_provider() {
    let err = testkit::app()
        .with(ManualTimer::default())
        .with(FakeClock::at(0, 0))
        .with(MemoryLog::default())
        .with(RecordingWrite::default())
        .with(AfkComponent)
        .boot()
        .await
        .err()
        .unwrap();
    assert!(
        err.message.contains("`afk`") && err.message.contains("idle@1"),
        "{err}"
    );
}
