//! `summary` 对外的契约：生成每日摘要。
//!
//! 生成要写数据库和文件，只在常驻进程里做；命令行进程通过跨进程代理调用同一个契约。

use kernel_api::{AppError, contract};
use serde::{Deserialize, Serialize};

/// 一次生成。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SummarizeRequest {
    /// 哪一天（写法同 `lifelog day`）。`None`：昨天，再补上最近几天还没有摘要的。
    #[serde(default)]
    pub date: Option<String>,
    /// 文件被手改过也重新生成、覆盖。
    #[serde(default)]
    pub force: bool,
    /// 只给出要发给模型的原料，不调用模型、不写任何东西。
    #[serde(default)]
    pub dry_run: bool,
}

/// 某一天怎么样了。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// 生成了，写进了文件和文档库。
    Written,
    /// 已经有摘要了（不给日期的补生成只补缺的）。
    Exists,
    /// 文件被手改过（或不是生成的），没有覆盖。
    KeptEdited,
    /// 这一天在电脑前的时间太少，不值得写。
    TooLittle,
    /// 只看不发：`digest` 是要发出去的原料。
    DryRun,
}

/// 一天的结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaySummary {
    /// `YYYY-MM-DD`。
    pub date: String,
    pub outcome: Outcome,
    /// 摘要文件（文件根目录下的相对路径）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// 生成的摘要（Markdown）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// 只看不发时，要发给模型的原料。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    /// 用的模型。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SummarizeResult {
    /// 按日期从早到晚。
    pub days: Vec<DaySummary>,
}

contract! {
    /// 每日摘要。
    pub trait Summarize as SUMMARIZE = service("summary", 1) {
        /// 模型接口不可用时整次失败（`unavailable`），已经写好的那几天不受影响。
        async fn summarize(&self, request: SummarizeRequest) -> Result<SummarizeResult, AppError>;
    }
}
