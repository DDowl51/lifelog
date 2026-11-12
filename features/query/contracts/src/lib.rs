//! `query` 对外的契约：只读查询。
//!
//! 命令行、终端界面、以后的 MCP 都只是这个契约的薄包装：
//! 命令行的 `--json` 输出就是这里的返回类型序列化之后的样子。
//!
//! 所有时长都已经扣掉了离开的时间：离开期间焦点停在哪个窗口上不算数。

use kernel_api::{AppError, contract};
pub use search_contracts::{DocumentHit, Hit};
use serde::{Deserialize, Serialize};

/// 时长统计按什么分组。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum By {
    App,
    Title,
    /// 按项目。项目由 `project` 在入库时归类，归不出来的是 `None`。
    Project,
}

/// 一组的统计。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DayRow {
    /// 应用标识、标题或项目；没有的归到 `None`。
    pub key: Option<String>,
    pub total_ms: i64,
    /// 有多少个时段。
    pub spans: u64,
}

/// 某一天在各个窗口上花的时间。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DayReport {
    /// 逻辑日，`YYYY-MM-DD`。
    pub date: String,
    /// 一天从本地时间几点开始，`HH:MM`。
    pub day_boundary: String,
    /// 这一天的范围 `[start, end)`，UTC 毫秒。
    pub start: i64,
    pub end: i64,
    /// 计算范围时用的本地偏移（分钟）。
    pub tz_offset_min: i32,
    pub by: By,
    /// 窗口时间的合计，已经扣掉了离开的时间。
    pub total_ms: i64,
    /// 这一天里离开了多久。
    pub away_ms: i64,
    /// 按时长降序。
    pub rows: Vec<DayRow>,
}

/// 时间线上的一项是什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    /// 焦点在某个窗口上。
    Window,
    /// 离开了。
    Away,
}

/// 时间线上的一项。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineEntry {
    pub kind: EntryKind,
    /// `[start, end)`，UTC 毫秒。
    pub start: i64,
    pub end: i64,
    /// 发生时本地时间比 UTC 快多少分钟。
    pub tz_offset_min: i32,
    pub app: Option<String>,
    pub title: Option<String>,
    /// 这一项期间穿插的短暂窗口（切过去看一眼又回来的那些），按时长降序。
    /// 它们的时间算在 `[start, end)` 里。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub during: Vec<Brief>,
    /// 这一项期间执行的命令（到下一项开始之前），相同的合在一起，按第一次出现的先后。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<Mark>,
    /// 这一项期间访问的网页，同上。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub visits: Vec<Mark>,
}

/// 时间线一项里的一条命令或一个网页。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mark {
    /// 第一次出现的时刻，UTC 毫秒。
    pub at: i64,
    /// 命令；或网页标题（没有标题时是网址）。
    pub text: String,
    /// 网页的域名。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// 这一项期间出现了几次。
    pub count: u32,
}

/// 穿插在一项里的短暂窗口，同一个应用和标题合计。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Brief {
    pub app: Option<String>,
    pub title: Option<String>,
    pub total_ms: i64,
}

/// 某一天按时间顺序做了什么。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Timeline {
    pub date: String,
    pub day_boundary: String,
    pub start: i64,
    pub end: i64,
    pub tz_offset_min: i32,
    /// 按开始时刻排序。
    pub entries: Vec<TimelineEntry>,
}

/// 一周里的一天。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WeekDay {
    pub date: String,
    pub total_ms: i64,
    pub away_ms: i64,
}

/// 一周里的一组。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WeekRow {
    pub key: Option<String>,
    /// 周一到周日，每天多少毫秒。
    pub days_ms: Vec<i64>,
    pub total_ms: i64,
}

/// 一周（周一到周日）在各个窗口上花的时间。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WeekReport {
    /// 这周的周一，`YYYY-MM-DD`。
    pub week_start: String,
    pub day_boundary: String,
    pub by: By,
    pub days: Vec<WeekDay>,
    pub total_ms: i64,
    pub away_ms: i64,
    /// 按一周的合计降序。
    pub rows: Vec<WeekRow>,
}

/// 执行过的一条命令。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandEntry {
    /// 开始时刻，UTC 毫秒。
    pub at: i64,
    pub tz_offset_min: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<i32>,
    /// 命令（多行的已经连成一行）。
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

/// 一天里一个项目的情况。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectRow {
    /// 项目；归不了类的是 `None`。
    pub project: Option<String>,
    /// 窗口时长，已经扣掉了离开的时间。
    pub window_ms: i64,
    /// 执行了几条命令。
    pub commands: u32,
    /// 访问了几个网页。
    pub visits: u32,
}

/// 某一天按项目分。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectDay {
    pub date: String,
    pub day_boundary: String,
    pub start: i64,
    pub end: i64,
    pub tz_offset_min: i32,
    pub away_ms: i64,
    /// 按窗口时长降序；一样长时按命令数、网页数；归不了类的排最后。
    pub rows: Vec<ProjectRow>,
}

/// 某一天执行过的命令。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Commands {
    pub date: String,
    pub day_boundary: String,
    pub start: i64,
    pub end: i64,
    pub tz_offset_min: i32,
    /// 按开始时刻排序。
    pub entries: Vec<CommandEntry>,
}

/// 访问过的一个网页。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VisitEntry {
    /// 访问时刻，UTC 毫秒。
    pub at: i64,
    pub tz_offset_min: i32,
    /// 浏览器的名字。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// 网址。被隐私规则排除的是 `[redacted]`。
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

/// 某一天访问过的网页。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Visits {
    pub date: String,
    pub day_boundary: String,
    pub start: i64,
    pub end: i64,
    pub tz_offset_min: i32,
    /// 按访问时刻排序。
    pub entries: Vec<VisitEntry>,
}

/// 一次检索。日期的写法同 `day`；`until` 那一天也算在内。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchRequest {
    pub text: String,
    #[serde(default)]
    pub since: Option<String>,
    #[serde(default)]
    pub until: Option<String>,
    /// 只找这个来源的（`focus`、`shell`、`browsing`……）。
    #[serde(default)]
    pub source: Option<String>,
    pub limit: u32,
}

/// 检索结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchReport {
    pub text: String,
    /// 最近的在前。同一来源里标题和网址都一样的合成一条。
    pub hits: Vec<Hit>,
    /// 命中的摘要和笔记，最相关的在前。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub documents: Vec<DocumentHit>,
    /// 还有更多没给出来：加大 `limit` 或缩小范围。
    pub truncated: bool,
}

contract! {
    /// 只读查询。
    pub trait Memory as MEMORY = service("memory", 1) {
        /// 某一天的窗口使用时长。
        ///
        /// `date` 接受 `today`、`yesterday`、`3d`（3 天前）、`2026-11-12`，按逻辑日解释；
        /// 写得不对返回 `validation`。
        async fn day(&self, date: String, by: By) -> Result<DayReport, AppError>;

        /// 某一天按时间顺序做了什么，每一段带着这段时间里的命令和网页。日期的写法同 `day`。
        async fn timeline(&self, date: String) -> Result<Timeline, AppError>;

        /// 某一天在 shell 里执行过的命令。日期的写法同 `day`。
        async fn commands(&self, date: String) -> Result<Commands, AppError>;

        /// 某一天按项目分：窗口时长、命令数、网页数。日期的写法同 `day`。
        async fn projects(&self, date: String) -> Result<ProjectDay, AppError>;

        /// 在窗口标题、命令、网页里检索。没有启用检索时返回 `unavailable`。
        async fn search(&self, request: SearchRequest) -> Result<SearchReport, AppError>;

        /// 某一天在浏览器里访问过的网页。日期的写法同 `day`。
        async fn visits(&self, date: String) -> Result<Visits, AppError>;

        /// 某一天所在的那一周（周一到周日）。日期的写法同 `day`。
        async fn week(&self, date: String, by: By) -> Result<WeekReport, AppError>;
    }
}
