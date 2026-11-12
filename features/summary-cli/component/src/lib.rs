//! 应用功能 `summary-cli`：把 `lifelog summarize` 登记到命令行。
//!
//! 它只用 `Summarize` 契约。命令行进程里，那个契约是跨进程代理，真正的生成在常驻进程里；
//! 这里不知道、也不需要知道这一点。

use std::sync::Arc;

use cli_contract::{
    ArgKind, ArgSpec, CLI_COMMANDS, CommandProvider, CommandSpec, Format, Invocation, Output,
};
use kernel_api::{
    AppError, Component, ConfigSection, Host, Kind, Manifest, NoConfig, async_trait,
    register_component,
};
use summary_contracts::{
    DaySummary, Outcome, SUMMARIZE, Summarize, SummarizeRequest, SummarizeResult,
};

fn arg(name: &str, help: &str, kind: ArgKind) -> ArgSpec {
    ArgSpec {
        name: name.to_owned(),
        help: help.to_owned(),
        kind,
    }
}

/// 给人看的一行。
fn line(day: &DaySummary) -> String {
    let path = day.path.as_deref().unwrap_or("");
    match day.outcome {
        Outcome::Written => format!(
            "{}  写好了  {path}{}",
            day.date,
            day.model
                .as_deref()
                .map(|m| format!("（{m}）"))
                .unwrap_or_default()
        ),
        Outcome::Exists => format!("{}  已经有了  {path}", day.date),
        Outcome::KeptEdited => {
            format!("{}  手改过，没有覆盖（要覆盖加 --force）  {path}", day.date)
        }
        Outcome::TooLittle => format!("{}  在电脑前的时间太少，跳过", day.date),
        Outcome::DryRun => format!("{}  只看不发：下面是要发给模型的内容", day.date),
    }
}

/// 给人看的文本。只看不发时把原料也印出来。
pub fn render(result: &SummarizeResult) -> String {
    let mut out = String::new();
    for day in &result.days {
        out.push_str(&line(day));
        out.push('\n');
        if let Some(digest) = &day.digest {
            out.push('\n');
            out.push_str(digest.trim_end());
            out.push_str("\n\n");
        }
    }
    out
}

struct Commands(Arc<dyn Summarize>);

#[async_trait]
impl CommandProvider for Commands {
    async fn commands(&self) -> Result<Vec<CommandSpec>, AppError> {
        Ok(vec![CommandSpec {
            name: "summarize".to_owned(),
            about: "生成每日摘要（交给常驻进程做）。不给日期：昨天，再补最近几天还没有的"
                .to_owned(),
            args: vec![
                arg(
                    "date",
                    "哪一天：yesterday、3d、2026-11-12……",
                    ArgKind::Positional,
                ),
                arg("force", "文件被手改过也重新生成、覆盖", ArgKind::Flag),
                arg(
                    "dry-run",
                    "只印出要发给模型的内容，不调用模型、不写任何东西",
                    ArgKind::Flag,
                ),
            ],
        }])
    }

    async fn run(&self, invocation: Invocation) -> Result<Output, AppError> {
        if invocation.command != "summarize" {
            return Err(AppError::not_found(format!(
                "summary-cli 没有子命令 `{}`",
                invocation.command
            )));
        }
        let flag = |name: &str| invocation.args.get(name).is_some_and(|v| v == "true");
        let result = self
            .0
            .summarize(SummarizeRequest {
                date: invocation.args.get("date").cloned(),
                force: flag("force"),
                dry_run: flag("dry-run"),
            })
            .await?;
        let text = match invocation.format {
            Format::Json => serde_json::to_string(&result)
                .map_err(|e| AppError::unavailable(format!("结果写不成 JSON：{e}")))?,
            Format::Text { .. } => render(&result),
        };
        Ok(Output { text })
    }
}

pub struct SummaryCliComponent;

impl Component for SummaryCliComponent {
    fn config_defaults(&self) -> Option<ConfigSection> {
        ConfigSection::defaults::<NoConfig>()
    }

    fn manifest(&self) -> Manifest {
        Manifest {
            id: "summary-cli",
            kind: Kind::Feature,
            summary: "把 lifelog summarize 登记到命令行",
            provides: vec![],
            requires: vec![SUMMARIZE.id(), CLI_COMMANDS.id()],
            optional_requires: vec![],
        }
    }

    fn start(&self, config: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
        config.parse::<NoConfig>()?;
        let summarize = host.resolve(&SUMMARIZE)?;
        host.contribute(&CLI_COMMANDS, Arc::new(Commands(summarize)))
    }
}

register_component!(SummaryCliComponent);

#[cfg(test)]
mod tests {
    use super::*;

    fn day(outcome: Outcome) -> DaySummary {
        DaySummary {
            date: "2026-11-12".to_owned(),
            outcome,
            path: Some("daily/2026-11-12.md".to_owned()),
            text: None,
            digest: (outcome == Outcome::DryRun).then(|| "日期：2026-11-12\n".to_owned()),
            model: (outcome == Outcome::Written).then(|| "deepseek-chat".to_owned()),
        }
    }

    #[test]
    fn text_for_people() {
        let result = SummarizeResult {
            days: vec![
                day(Outcome::TooLittle),
                day(Outcome::Written),
                day(Outcome::KeptEdited),
                day(Outcome::DryRun),
            ],
        };
        assert_eq!(
            render(&result),
            "\
2026-11-12  在电脑前的时间太少，跳过
2026-11-12  写好了  daily/2026-11-12.md（deepseek-chat）
2026-11-12  手改过，没有覆盖（要覆盖加 --force）  daily/2026-11-12.md
2026-11-12  只看不发：下面是要发给模型的内容

日期：2026-11-12

"
        );
    }
}
