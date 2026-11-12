//! 用例：提交一批活动。

use std::sync::Arc;

use activity_contracts::{Activity, ActivityId, ActivityProcessor};
use clock_contract::Clock;
use kernel_api::{AppError, async_trait};
use log_contract::Logger;

use crate::validate;

/// 活动存到哪。由适配层实现。
#[async_trait]
pub trait Repo: Send + Sync {
    /// 在一个事务里写入一批，按顺序返回编号。
    /// 有 `dedup_key` 且已经存在时更新那一行，编号和 `ingested_at` 不变。
    async fn upsert(
        &self,
        activities: Vec<Activity>,
        ingested_at: i64,
    ) -> Result<Vec<ActivityId>, AppError>;
}

/// 提交活动：处理器 → 校验 → 入库。
pub struct Submit {
    pub repo: Arc<dyn Repo>,
    pub clock: Arc<dyn Clock>,
    pub log: Logger,
}

impl Submit {
    /// `processors` 已经按执行顺序排好。
    ///
    /// 返回值和输入一一对应；被处理器丢弃或没通过校验的是 `None`。
    /// 没通过校验只丢那一条并记日志；处理器出错则整批失败。
    pub async fn run(
        &self,
        activities: Vec<Activity>,
        processors: &[Arc<dyn ActivityProcessor>],
    ) -> Result<Vec<Option<ActivityId>>, AppError> {
        let mut slots: Vec<Option<ActivityId>> = vec![None; activities.len()];
        let mut accepted = Vec::new();
        let mut accepted_at = Vec::new();

        for (index, activity) in activities.into_iter().enumerate() {
            let mut current = Some(activity);
            for processor in processors {
                let Some(activity) = current.take() else {
                    break;
                };
                current = processor.process(activity).await?;
            }
            let Some(activity) = current else { continue };
            match validate(&activity) {
                Ok(()) => {
                    accepted.push(activity);
                    accepted_at.push(index);
                }
                Err(reason) => {
                    self.log
                        .warn(format!("丢弃第 {} 条活动：{reason}", index + 1))
                        .await;
                }
            }
        }

        if accepted.is_empty() {
            return Ok(slots);
        }
        let now = self.clock.now().await?.utc_ms;
        let ids = self.repo.upsert(accepted, now).await?;
        if ids.len() != accepted_at.len() {
            return Err(AppError::unavailable(format!(
                "存储返回了 {} 个编号，应该是 {} 个",
                ids.len(),
                accepted_at.len()
            )));
        }
        for (index, id) in accepted_at.into_iter().zip(ids) {
            slots[index] = Some(id);
        }
        Ok(slots)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use clock_contract::testing::FakeClock;
    use log_contract::testing::MemoryLog;

    use super::*;

    /// 记在内存里的存储：编号从 1 递增。
    #[derive(Default)]
    struct MemoryRepo(Mutex<Vec<(Activity, i64)>>);

    #[async_trait]
    impl Repo for MemoryRepo {
        async fn upsert(
            &self,
            activities: Vec<Activity>,
            ingested_at: i64,
        ) -> Result<Vec<ActivityId>, AppError> {
            let mut all = self.0.lock().unwrap();
            let mut ids = Vec::new();
            for activity in activities {
                all.push((activity, ingested_at));
                ids.push(ActivityId(i64::try_from(all.len()).unwrap()));
            }
            Ok(ids)
        }
    }

    /// 给标题加后缀；标题是 `secret` 的丢弃；标题是 `boom` 的报错。
    struct Tag(&'static str);

    #[async_trait]
    impl ActivityProcessor for Tag {
        async fn name(&self) -> Result<String, AppError> {
            Ok(self.0.to_owned())
        }

        async fn process(&self, mut activity: Activity) -> Result<Option<Activity>, AppError> {
            match activity.title.as_deref() {
                Some("secret") => Ok(None),
                Some("boom") => Err(AppError::unavailable("处理器坏了")),
                _ => {
                    let title = activity.title.unwrap_or_default();
                    activity.title = Some(format!("{title}+{}", self.0));
                    Ok(Some(activity))
                }
            }
        }
    }

    struct Fixture {
        submit: Submit,
        repo: Arc<MemoryRepo>,
        log: MemoryLog,
    }

    fn fixture() -> Fixture {
        let repo = Arc::new(MemoryRepo::default());
        let log = MemoryLog::default();
        let submit = Submit {
            repo: repo.clone(),
            clock: Arc::new(FakeClock::at(5_000, 480)),
            log: Logger::new(Arc::new(log.clone()), "activity"),
        };
        Fixture { submit, repo, log }
    }

    fn titled(title: &str) -> Activity {
        Activity::new("focus", "window_focus", 1_000, 480).with_title(title)
    }

    #[tokio::test]
    async fn stores_valid_activities_with_the_current_time() {
        let f = fixture();
        let ids = f
            .submit
            .run(vec![titled("a"), titled("b")], &[])
            .await
            .unwrap();
        assert_eq!(ids, [Some(ActivityId(1)), Some(ActivityId(2))]);
        let stored = f.repo.0.lock().unwrap().clone();
        assert_eq!(stored[0], (titled("a"), 5_000));
    }

    #[tokio::test]
    async fn an_invalid_activity_is_dropped_alone_and_logged() {
        let f = fixture();
        let mut bad = titled("bad");
        bad.source = "Focus".to_owned();
        let ids = f
            .submit
            .run(vec![titled("a"), bad, titled("c")], &[])
            .await
            .unwrap();
        assert_eq!(ids, [Some(ActivityId(1)), None, Some(ActivityId(2))]);
        let lines = f.log.lines();
        assert_eq!(lines.len(), 1);
        assert!(
            lines[0].starts_with("warn activity: 丢弃第 2 条活动：source"),
            "{lines:?}"
        );
    }

    #[tokio::test]
    async fn processors_run_in_the_given_order_and_may_drop() {
        let f = fixture();
        let chain: Vec<Arc<dyn ActivityProcessor>> = vec![Arc::new(Tag("x")), Arc::new(Tag("y"))];
        let ids = f
            .submit
            .run(vec![titled("a"), titled("secret")], &chain)
            .await
            .unwrap();
        assert_eq!(ids, [Some(ActivityId(1)), None]);
        assert_eq!(
            f.repo.0.lock().unwrap()[0].0.title.as_deref(),
            Some("a+x+y")
        );
        assert!(f.log.lines().is_empty(), "被处理器丢弃不算异常，不记日志");
    }

    #[tokio::test]
    async fn a_failing_processor_fails_the_whole_batch() {
        let f = fixture();
        let chain: Vec<Arc<dyn ActivityProcessor>> = vec![Arc::new(Tag("x"))];
        let err = f
            .submit
            .run(vec![titled("a"), titled("boom")], &chain)
            .await
            .unwrap_err();
        assert_eq!(err.message, "处理器坏了");
        assert!(
            f.repo.0.lock().unwrap().is_empty(),
            "宁可不存，也不存没加工过的"
        );
    }

    #[tokio::test]
    async fn nothing_to_store_does_not_touch_the_repo() {
        let f = fixture();
        assert!(f.submit.run(vec![], &[]).await.unwrap().is_empty());
        assert_eq!(
            f.submit
                .run(vec![titled("secret")], &[Arc::new(Tag("x"))])
                .await
                .unwrap(),
            [None]
        );
    }
}
