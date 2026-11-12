//! 应用功能 `summary`：生成每日摘要。
//!
//! 一天：从 `Memory` 取按项目、时间线、命令、网页 → 压成原料（`summary-core`）→ 交给 `Llm`
//! → 写成 Markdown 文件（`Files`）并存进文档库（`DocumentWrite`）。
//! 只在常驻进程里运行（要写数据库）；命令行进程通过跨进程代理调用 `Summarize`。

use std::sync::Arc;

use documents_contracts::{DOCUMENT_WRITE, Document, DocumentWrite};
use files_contract::{FILES, Files};
use kernel_api::{
    AppError, Component, ConfigSection, Host, Kind, Manifest, async_trait, register_component,
};
use llm_contract::{ChatMessage, CompletionRequest, LLM, Llm};
use log_contract::{LOG, Logger};
use query_contracts::{MEMORY, Memory};
use serde::{Deserialize, Serialize};
use summary_contracts::{
    DaySummary, Outcome, SUMMARIZE, Summarize, SummarizeRequest, SummarizeResult,
};
use summary_core::{
    DEFAULT_PROMPT, DayInput, FileState, active_ms, body_of, digest, file_state, render_file,
    weekday,
};

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
struct Config {
    /// 不发给模型的来源（`focus`、`shell`、`browsing`）。发出去的内容会到模型服务商那里。
    hidden_sources: Vec<String>,
    /// 给模型的说明。
    prompt: String,
    /// 摘要文件放在哪（文件根目录下），`{date}` 换成日期。
    path: String,
    /// 不给日期时，往回补几天还没有摘要的（含昨天）。
    catch_up_days: u32,
    /// 原料最多多少字。
    max_digest_chars: usize,
    /// 摘要最多多少 token。
    max_tokens: u32,
    /// 一天在电脑前少于这么多分钟就不写。
    min_active_minutes: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            hidden_sources: Vec::new(),
            prompt: DEFAULT_PROMPT.to_owned(),
            path: "daily/{date}.md".to_owned(),
            catch_up_days: 3,
            max_digest_chars: 12_000,
            max_tokens: 1_500,
            min_active_minutes: 5,
        }
    }
}

struct Service {
    memory: Arc<dyn Memory>,
    llm: Arc<dyn Llm>,
    files: Arc<dyn Files>,
    documents: Arc<dyn DocumentWrite>,
    log: Logger,
    config: Config,
}

impl Service {
    fn path(&self, date: &str) -> String {
        self.config.path.replace("{date}", date)
    }

    /// 处理一天。`date` 是 `Memory` 认识的写法；`only_missing` 时已经有文件就不动。
    async fn one(
        &self,
        date: &str,
        force: bool,
        dry_run: bool,
        only_missing: bool,
    ) -> Result<DaySummary, AppError> {
        let projects = self.memory.projects(date.to_owned()).await?;
        let date = projects.date.clone();
        let path = self.path(&date);
        let result = |outcome, path: Option<String>| DaySummary {
            date: date.clone(),
            outcome,
            path,
            text: None,
            digest: None,
            model: None,
        };

        let existing = self.files.read(path.clone()).await?;
        if only_missing && existing.is_some() && !dry_run {
            return Ok(result(Outcome::Exists, Some(path)));
        }

        let timeline = self.memory.timeline(date.clone()).await?;
        let commands = self.memory.commands(date.clone()).await?;
        let visits = self.memory.visits(date.clone()).await?;
        let input = DayInput {
            projects: &projects,
            timeline: &timeline,
            commands: &commands,
            visits: &visits,
        };
        if active_ms(&input) < i64::from(self.config.min_active_minutes) * 60_000 {
            return Ok(result(Outcome::TooLittle, None));
        }
        let material = digest(
            &input,
            &self.config.hidden_sources,
            self.config.max_digest_chars,
        );
        if dry_run {
            return Ok(DaySummary {
                digest: Some(material),
                ..result(Outcome::DryRun, Some(path))
            });
        }
        if let Some(file) = &existing
            && file_state(file) == FileState::Edited
            && !force
        {
            return Ok(result(Outcome::KeptEdited, Some(path)));
        }

        let completion = self
            .llm
            .complete(CompletionRequest {
                messages: vec![
                    ChatMessage::system(self.config.prompt.clone()),
                    ChatMessage::user(material),
                ],
                max_tokens: Some(self.config.max_tokens),
            })
            .await?;
        let file = render_file(&date, &completion.text, &completion.model);
        self.files.write(path.clone(), file.clone()).await?;
        self.documents
            .put(Document {
                key: format!("summary_daily:{date}"),
                kind: "summary_daily".to_owned(),
                date: Some(date.clone()),
                title: format!("{date} {}", weekday(&date)),
                body: body_of(&file),
                origin: Some(path.clone()),
            })
            .await?;
        self.log
            .info(format!(
                "写好了 {date} 的摘要（{}，{} + {} token）",
                completion.model,
                completion.prompt_tokens.unwrap_or(0),
                completion.completion_tokens.unwrap_or(0)
            ))
            .await;
        Ok(DaySummary {
            text: Some(body_of(&file)),
            model: Some(completion.model),
            ..result(Outcome::Written, Some(path))
        })
    }
}

#[async_trait]
impl Summarize for Service {
    async fn summarize(&self, request: SummarizeRequest) -> Result<SummarizeResult, AppError> {
        let mut days = Vec::new();
        match &request.date {
            Some(date) => days.push(
                self.one(date, request.force, request.dry_run, false)
                    .await?,
            ),
            None => {
                // 从早到晚补：最近 N 天（含昨天）里还没有摘要的
                for back in (1..=self.config.catch_up_days.max(1)).rev() {
                    let spec = format!("{back}d");
                    days.push(
                        self.one(&spec, request.force, request.dry_run, !request.force)
                            .await?,
                    );
                }
            }
        }
        Ok(SummarizeResult { days })
    }
}

pub struct SummaryComponent;

impl Component for SummaryComponent {
    fn config_defaults(&self) -> Option<ConfigSection> {
        ConfigSection::defaults::<Config>()
    }

    fn manifest(&self) -> Manifest {
        Manifest {
            id: "summary",
            kind: Kind::Feature,
            summary: "生成每日摘要",
            provides: vec![SUMMARIZE.id()],
            requires: vec![
                MEMORY.id(),
                LLM.id(),
                FILES.id(),
                DOCUMENT_WRITE.id(),
                LOG.id(),
            ],
            optional_requires: vec![],
        }
    }

    fn start(&self, config: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
        let config: Config = config.parse()?;
        if !config.path.contains("{date}") {
            return Err(AppError::config(
                "path 里要有 {date}，否则每天的摘要会写进同一个文件",
            ));
        }
        let service = Service {
            memory: host.resolve(&MEMORY)?,
            llm: host.resolve(&LLM)?,
            files: host.resolve(&FILES)?,
            documents: host.resolve(&DOCUMENT_WRITE)?,
            log: Logger::new(host.resolve(&LOG)?, host.id()),
            config,
        };
        host.provide(&SUMMARIZE, Arc::new(service))
    }
}

register_component!(SummaryComponent);
