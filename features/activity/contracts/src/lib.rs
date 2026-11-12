//! `activity` 对外的契约。
//!
//! 「活动」是 lifelog 记录的东西：焦点停在某个窗口上的一段时间、执行的一条命令、访问的一个网页。
//! 各种数据源把活动交给 [`ActivityWrite`]，查询方通过 [`ActivityRead`] 读。

use kernel_api::{AppError, EventDef, contract};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// 一条活动。
///
/// 瞬时活动（执行了一条命令）只有 `ts_start`；
/// 时段活动（焦点停在某个窗口上）还有 `ts_end`。进行中的时段会用同一个 `dedup_key`
/// 反复提交，`ts_end` 逐渐变大，存下来始终是一行。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Activity {
    /// 来源，例如 `focus`、`shell`。只允许小写字母、数字和 `_` `-` `.`。
    pub source: String,
    /// 种类，例如 `window_focus`、`command`。字符限制同上。
    pub kind: String,
    /// 开始时刻，UTC 毫秒。
    pub ts_start: i64,
    /// 结束时刻，UTC 毫秒。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ts_end: Option<i64>,
    /// 发生时本地时间比 UTC 快多少分钟，用来算它属于哪个逻辑日。
    pub tz_offset_min: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// 来源自定义的字段，例如命令的工作目录、退出码。
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub payload: Map<String, Value>,
    /// 同一个 `dedup_key` 只存一条，后提交的覆盖先提交的。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedup_key: Option<String>,
    /// 是否被隐私规则脱敏过。
    #[serde(default)]
    pub redacted: bool,
}

impl Activity {
    pub fn new(source: &str, kind: &str, ts_start: i64, tz_offset_min: i32) -> Self {
        Self {
            source: source.to_owned(),
            kind: kind.to_owned(),
            ts_start,
            ts_end: None,
            tz_offset_min,
            app: None,
            title: None,
            project: None,
            payload: Map::new(),
            dedup_key: None,
            redacted: false,
        }
    }

    #[must_use]
    pub fn with_end(mut self, ts_end: i64) -> Self {
        self.ts_end = Some(ts_end);
        self
    }

    #[must_use]
    pub fn with_app(mut self, app: &str) -> Self {
        self.app = Some(app.to_owned());
        self
    }

    #[must_use]
    pub fn with_title(mut self, title: &str) -> Self {
        self.title = Some(title.to_owned());
        self
    }

    #[must_use]
    pub fn with_project(mut self, project: &str) -> Self {
        self.project = Some(project.to_owned());
        self
    }

    #[must_use]
    pub fn with_dedup_key(mut self, key: &str) -> Self {
        self.dedup_key = Some(key.to_owned());
        self
    }

    #[must_use]
    pub fn with_payload(mut self, key: &str, value: impl Into<Value>) -> Self {
        self.payload.insert(key.to_owned(), value.into());
        self
    }
}

/// `payload` 里约定的键。数据源写、处理器读，用同一个名字；别的键由各来源自定。
pub mod payload_keys {
    /// 发生时的工作目录，绝对路径。例如命令在哪个目录执行。
    pub const CWD: &str = "cwd";
    /// 访问的网址。
    pub const URL: &str = "url";
    /// 窗口所在工作区的名字。
    pub const WORKSPACE: &str = "workspace";
    /// 产生这条活动的程序的 profile 名（比如浏览器的 profile、每个项目一个的浏览器实例）。
    pub const PROFILE: &str = "profile";
}

/// 存储给活动分配的编号。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ActivityId(pub i64);

/// 读出来的活动：活动本身加上入库时补充的信息。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredActivity {
    pub id: ActivityId,
    pub activity: Activity,
    /// 第一次入库的时刻，UTC 毫秒。同一个 `dedup_key` 再次提交时不变。
    pub ingested_at: i64,
}

/// 时间范围 `[start, end)`，UTC 毫秒。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeRange {
    pub start: i64,
    pub end: i64,
}

/// 查哪些活动：和 `range` 有重叠的，再按 `source`、`kind` 过滤（`None` 表示不过滤）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityQuery {
    pub range: TimeRange,
    pub source: Option<String>,
    pub kind: Option<String>,
}

/// 统计时长时按哪一列分组。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupBy {
    App,
    Title,
    Project,
}

/// 一组的统计结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DurationRow {
    /// 分组列的值；这一列为空的时段归到 `None`。
    pub key: Option<String>,
    /// 落在查询范围内的总毫秒数（跨出范围的部分已经裁掉）。
    pub total_ms: i64,
    /// 有多少个时段参与了统计。
    pub spans: u64,
}

contract! {
    /// 提交活动。
    pub trait ActivityWrite as ACTIVITY_WRITE = service("activity.write", 1) {
        /// 提交一批。每条先依次经过各处理器，再校验，通过的在一个事务里入库。
        ///
        /// 返回值和输入一一对应：入库的给编号；被处理器丢弃或没通过校验的是 `None`。
        /// 有 `dedup_key` 且已经存在时更新那一行（编号和 `ingested_at` 不变）。
        async fn submit(&self, activities: Vec<Activity>) -> Result<Vec<Option<ActivityId>>, AppError>;
    }
}

contract! {
    /// 读活动。
    pub trait ActivityRead as ACTIVITY_READ = service("activity.read", 1) {
        /// 和查询范围有重叠的活动，按开始时刻升序。
        async fn list(&self, query: ActivityQuery) -> Result<Vec<StoredActivity>, AppError>;

        /// 按 `group_by` 分组统计时段活动的时长，只算落在查询范围内的部分。按总时长降序。
        async fn durations(&self, query: ActivityQuery, group_by: GroupBy) -> Result<Vec<DurationRow>, AppError>;

        /// 编号大于 `after`（`None` 表示从头）的活动，按编号升序，最多 `limit` 条。
        /// 给「按还没处理的往下拉」的使用方（比如建索引）。
        async fn since(&self, after: Option<ActivityId>, limit: u32) -> Result<Vec<StoredActivity>, AppError>;

        /// 按编号取，顺序和 `ids` 一致；不存在的跳过。
        async fn get(&self, ids: Vec<ActivityId>) -> Result<Vec<StoredActivity>, AppError>;
    }
}

contract! {
    /// 扩展点：入库之前加工活动（归类、脱敏、排除）。
    ///
    /// 处理器执行的先后是策略，写在 `activity` 的配置里。
    pub trait ActivityProcessor as ACTIVITY_PROCESSORS = point("activity.processors", 1) {
        /// 处理器的名字，配置里用它排顺序。
        async fn name(&self) -> Result<String, AppError>;

        /// 加工一条活动。返回 `None` 表示丢弃。
        /// 返回错误时整批提交失败：宁可不存，也不存没加工过的。
        async fn process(&self, activity: Activity) -> Result<Option<Activity>, AppError>;
    }
}

/// 有活动入库了。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Committed {
    pub ids: Vec<ActivityId>,
}

/// 事件：一批活动入库之后发布。
pub static ACTIVITY_COMMITTED: EventDef<Committed> = EventDef::new("activity.write", "committed");

#[cfg(feature = "testing")]
pub mod testing {
    //! 只记录、不入库的假 `ActivityWrite`。数据源类的功能测试时用它，不必装存储。

    use std::sync::{Arc, Mutex, PoisonError};

    use kernel_api::{AppError, Component, ConfigSection, Host, Kind, Manifest, async_trait};

    use super::{ACTIVITY_WRITE, Activity, ActivityId, ActivityWrite};

    /// 把提交上来的活动记在内存里。克隆出来的是同一份。
    #[derive(Clone, Default)]
    pub struct RecordingWrite(Arc<Mutex<Vec<Activity>>>);

    impl RecordingWrite {
        /// 每一次提交的每一条，按先后。
        pub fn submitted(&self) -> Vec<Activity> {
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        }

        /// 按 `dedup_key` 合并：每个 key 只留最后一条，顺序按第一次出现。
        /// 和真的入库之后的结果一样。
        pub fn stored(&self) -> Vec<Activity> {
            let mut merged: Vec<Activity> = Vec::new();
            for activity in self.submitted() {
                let same = merged
                    .iter_mut()
                    .find(|m| m.dedup_key.is_some() && m.dedup_key == activity.dedup_key);
                match same {
                    Some(slot) => *slot = activity,
                    None => merged.push(activity),
                }
            }
            merged
        }
    }

    #[async_trait]
    impl ActivityWrite for RecordingWrite {
        async fn submit(
            &self,
            activities: Vec<Activity>,
        ) -> Result<Vec<Option<ActivityId>>, AppError> {
            let mut all = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            let mut ids = Vec::with_capacity(activities.len());
            for activity in activities {
                all.push(activity);
                ids.push(Some(ActivityId(
                    i64::try_from(all.len()).unwrap_or(i64::MAX),
                )));
            }
            Ok(ids)
        }
    }

    /// 组件 `activity-recorder`。
    impl Component for RecordingWrite {
        fn manifest(&self) -> Manifest {
            Manifest {
                id: "activity-recorder",
                kind: Kind::Feature,
                summary: "测试用：只记录提交上来的活动",
                provides: vec![ACTIVITY_WRITE.id()],
                requires: vec![],
                optional_requires: vec![],
            }
        }

        fn start(&self, _config: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
            host.provide(&ACTIVITY_WRITE, Arc::new(self.clone()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_omits_empty_fields_and_round_trips() {
        let bare = Activity::new("shell", "command", 1_000, 480);
        assert_eq!(
            serde_json::to_string(&bare).unwrap(),
            r#"{"source":"shell","kind":"command","ts_start":1000,"tz_offset_min":480,"redacted":false}"#
        );

        let full = bare
            .with_end(2_000)
            .with_app("ghostty")
            .with_title("nvim")
            .with_project("lifelog")
            .with_payload("cwd", "/home")
            .with_dedup_key("k:1");
        let back: Activity = serde_json::from_str(&serde_json::to_string(&full).unwrap()).unwrap();
        assert_eq!(back, full);
    }
}
