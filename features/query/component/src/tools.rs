//! 给 agent 用的工具（登记到 `MCP_TOOLS`）。
//!
//! 每个工具就是 `Memory` 的一个方法，结果是它返回值的 JSON。和命令行的 `--json` 是同一份数据，
//! 只多了两处为 agent 的限制：列表默认只给最近的若干条，时间线每段只给前几条命令和网页，
//! 被截掉时结果里会说明。读到的内容会发到 agent 的服务商那里，所以用的是合并了
//! `agent_hidden_sources` 的那份查询服务。

use std::sync::Arc;

use kernel_api::{AppError, async_trait};
use mcp_contract::{ParamKind, ToolCall, ToolOutput, ToolParam, ToolProvider, ToolSpec};
use query_contracts::{By, Memory, SearchRequest};
use serde::Serialize;
use serde_json::{Value, json};

use crate::Service;

/// 命令、网页列表默认给几条。
const LIST_LIMIT: u32 = 100;
/// 时间线每段最多给几条命令、几个网页。
const MARKS_PER_ENTRY: usize = 10;
/// 网址最多给几个字符：长的多半是查询参数，域名和路径都在前面。
const URL_CHARS: usize = 200;

const DATE: &str = "哪一天：today、yesterday、3d（3 天前）或 2026-11-12。按逻辑日解释（默认凌晨 4 点换日）。不填是今天。";

fn param(name: &str, description: &str, kind: ParamKind, required: bool) -> ToolParam {
    ToolParam {
        name: name.to_owned(),
        description: description.to_owned(),
        kind,
        required,
    }
}

fn date() -> ToolParam {
    param("date", DATE, ParamKind::String, false)
}

fn by(values: &[&str]) -> ToolParam {
    param(
        "by",
        "按什么分组，默认 app",
        ParamKind::Choice {
            values: values.iter().map(|v| (*v).to_owned()).collect(),
        },
        false,
    )
}

fn limit(default: u32) -> ToolParam {
    param(
        "limit",
        &format!("最多给几条，默认 {default}"),
        ParamKind::Integer,
        false,
    )
}

fn tool(name: &str, description: &str, params: Vec<ToolParam>) -> ToolSpec {
    ToolSpec {
        name: name.to_owned(),
        description: description.to_owned(),
        params,
    }
}

fn to_value<T: Serialize>(value: &T) -> Result<Value, AppError> {
    serde_json::to_value(value).map_err(|e| AppError::unavailable(format!("结果写不成 JSON：{e}")))
}

/// 列表只留最后（最近的）`limit` 条；截掉了就在结果里写上 `truncated` 和总数。
fn keep_latest(mut report: Value, limit: usize) -> Value {
    if let Some(entries) = report.get_mut("entries").and_then(Value::as_array_mut)
        && entries.len() > limit
    {
        let total = entries.len();
        entries.drain(..total - limit);
        report["truncated"] = json!(true);
        report["total"] = json!(total);
    }
    report
}

/// 时间线每段的命令、网页只留前几条；截掉了在那一段写上原来有几条。
fn trim_marks(mut timeline: Value) -> Value {
    if let Some(entries) = timeline.get_mut("entries").and_then(Value::as_array_mut) {
        for entry in entries {
            for (key, total_key) in [("commands", "commands_total"), ("visits", "visits_total")] {
                if let Some(list) = entry.get_mut(key).and_then(Value::as_array_mut)
                    && list.len() > MARKS_PER_ENTRY
                {
                    let total = list.len();
                    list.truncate(MARKS_PER_ENTRY);
                    entry[total_key] = json!(total);
                }
            }
        }
    }
    timeline
}

/// 把 `list_key` 下每一项的 `url` 截短。
fn shorten_urls(mut report: Value, list_key: &str) -> Value {
    if let Some(items) = report.get_mut(list_key).and_then(Value::as_array_mut) {
        for item in items {
            if let Some(Value::String(url)) = item.get_mut("url")
                && url.chars().count() > URL_CHARS
            {
                let short: String = url.chars().take(URL_CHARS).collect();
                *url = format!("{short}…");
            }
        }
    }
    report
}

pub(crate) struct Tools(pub(crate) Arc<Service>);

impl Tools {
    fn number(call: &ToolCall, default: u32) -> Result<u32, AppError> {
        match call.args.get("limit") {
            Some(text) => text
                .parse::<u32>()
                .ok()
                .filter(|n| *n > 0)
                .ok_or_else(|| AppError::validation("limit 要是正整数")),
            None => Ok(default),
        }
    }
}

#[async_trait]
impl ToolProvider for Tools {
    async fn tools(&self) -> Result<Vec<ToolSpec>, AppError> {
        let mut tools = vec![
            tool(
                "lifelog_day",
                "某一天在各个应用、窗口标题或项目上花了多久。时长已经扣掉了离开电脑的时间。",
                vec![date(), by(&["app", "title", "project"])],
            ),
            tool(
                "lifelog_timeline",
                "某一天按时间顺序做了什么：每一段是在哪个窗口、多久，期间穿插的短暂窗口、执行的命令、访问的网页；离开电脑的时段也列出。",
                vec![date()],
            ),
            tool(
                "lifelog_week",
                "某一天所在的那一周（周一到周日），每天在各个应用或项目上花了多久。",
                vec![date(), by(&["app", "title", "project"])],
            ),
            tool(
                "lifelog_projects",
                "某一天在各个项目上：窗口时长、执行了几条命令、访问了几个网页。",
                vec![date()],
            ),
            tool(
                "lifelog_commands",
                "某一天在 shell 里执行过的命令：时刻、耗时、退出码、目录、项目。",
                vec![date(), limit(LIST_LIMIT)],
            ),
            tool(
                "lifelog_visits",
                "某一天在浏览器里访问过的网页：时刻、浏览器、标题、网址、项目。",
                vec![date(), limit(LIST_LIMIT)],
            ),
        ];
        if self.0.search.is_some() {
            tools.push(tool(
                "lifelog_search",
                "在记下的窗口标题、命令、网页（标题和网址）里检索，最近的在前；同一个东西出现多次合成一条并给出次数。适合回答「我之前在哪里见过 / 做过 X」。",
                vec![
                    param(
                        "query",
                        "要找的词，空格分开，每个词都要出现；最后一个英文词按前缀匹配。中日英都可以。",
                        ParamKind::String,
                        true,
                    ),
                    param("since", "从这一天起，写法同 date", ParamKind::String, false),
                    param("until", "到这一天为止（含），写法同 date", ParamKind::String, false),
                    param(
                        "source",
                        "只找一种：focus（窗口）、shell（命令）、browsing（网页）",
                        ParamKind::Choice {
                            values: ["focus", "shell", "browsing"].map(str::to_owned).to_vec(),
                        },
                        false,
                    ),
                    limit(30),
                ],
            ));
        }
        Ok(tools)
    }

    async fn call(&self, call: ToolCall) -> Result<ToolOutput, AppError> {
        let memory: &dyn Memory = self.0.as_ref();
        let date = call
            .args
            .get("date")
            .cloned()
            .unwrap_or_else(|| "today".to_owned());
        let by = match call.args.get("by").map(String::as_str) {
            Some("title") => By::Title,
            Some("project") => By::Project,
            _ => By::App,
        };
        let value = match call.name.as_str() {
            "lifelog_day" => to_value(&memory.day(date, by).await?)?,
            "lifelog_timeline" => trim_marks(to_value(&memory.timeline(date).await?)?),
            "lifelog_week" => to_value(&memory.week(date, by).await?)?,
            "lifelog_projects" => to_value(&memory.projects(date).await?)?,
            "lifelog_commands" => {
                let n = Self::number(&call, LIST_LIMIT)?;
                keep_latest(to_value(&memory.commands(date).await?)?, n as usize)
            }
            "lifelog_visits" => {
                let n = Self::number(&call, LIST_LIMIT)?;
                shorten_urls(
                    keep_latest(to_value(&memory.visits(date).await?)?, n as usize),
                    "entries",
                )
            }
            "lifelog_search" => shorten_urls(
                to_value(
                    &memory
                        .search(SearchRequest {
                            text: call.args.get("query").cloned().unwrap_or_default(),
                            since: call.args.get("since").cloned(),
                            until: call.args.get("until").cloned(),
                            source: call.args.get("source").cloned(),
                            limit: Self::number(&call, 30)?,
                        })
                        .await?,
                )?,
                "hits",
            ),
            other => return Err(AppError::not_found(format!("query 没有工具 `{other}`"))),
        };
        Ok(ToolOutput {
            text: value.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_keep_the_latest() {
        let report = json!({ "date": "x", "entries": [1, 2, 3, 4] });
        assert_eq!(
            keep_latest(report.clone(), 2),
            json!({ "date": "x", "entries": [3, 4], "truncated": true, "total": 4 })
        );
        assert_eq!(keep_latest(report.clone(), 4), report);
    }

    #[test]
    fn long_urls_are_shortened() {
        let long = format!("https://example.com/{}", "q".repeat(300));
        let out = shorten_urls(
            json!({ "entries": [{ "url": long }, { "url": "https://a/" }] }),
            "entries",
        );
        let first = out["entries"][0]["url"].as_str().unwrap();
        assert_eq!(first.chars().count(), 201);
        assert!(first.ends_with('…') && first.starts_with("https://example.com/"));
        assert_eq!(out["entries"][1]["url"], "https://a/");
    }

    #[test]
    fn timeline_marks_are_trimmed() {
        let many: Vec<u32> = (0..15).collect();
        let timeline =
            json!({ "entries": [{ "commands": many, "visits": [1] }, { "kind": "away" }] });
        let out = trim_marks(timeline);
        assert_eq!(out["entries"][0]["commands"].as_array().unwrap().len(), 10);
        assert_eq!(out["entries"][0]["commands_total"], 15);
        assert!(out["entries"][0].get("visits_total").is_none());
        assert_eq!(out["entries"][1], json!({ "kind": "away" }));
    }
}
