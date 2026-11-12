//! 应用功能 `query`：对外的只读查询。
//!
//! 提供 `Memory` 契约，并把同样的查询登记成命令行子命令。
//! 命令行只是薄包装：`--json` 输出的就是 `Memory` 的返回值。
//!
//! 窗口时长都扣掉了离开的时间（离开时段由 `afk` 记录；没装 `afk` 就没有可扣的）。

mod tools;

use std::sync::Arc;

use activity_contracts::{ACTIVITY_READ, Activity, ActivityQuery, ActivityRead, TimeRange};
use cli_contract::{
    ArgKind, ArgSpec, CLI_COMMANDS, CommandProvider, CommandSpec, Format, Invocation, Output,
};
use clock_contract::{CLOCK, Clock, Moment};
use jiff::civil::{Date, Time};
use kernel_api::{
    AppError, Component, ConfigSection, Host, Kind, Manifest, async_trait, register_component,
};
use mcp_contract::MCP_TOOLS;
use query_contracts::{
    By, CommandEntry, Commands as CommandList, DayReport, DayRow, MEMORY, Memory, ProjectDay,
    SearchReport, SearchRequest, Timeline, VisitEntry, Visits, WeekDay, WeekReport, WeekRow,
};
use query_core::{
    absorb, attach, command_entries, commands_page, day_page, day_range, effective, group,
    parse_boundary, project_rows, render_commands, render_day, render_projects, render_search,
    render_timeline, render_visits, render_week, resolve_date, search_page, shift,
    timeline_entries, timeline_page, visit_entries, visits_page, week_dates, week_page,
};
use search_contracts::{SEARCH, Search, SearchQuery};
use serde::Deserialize;
use serde::Serialize;
use tui_contract::{PageProvider, PageRequest, PageSpec, PageView, TUI_PAGES};

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
struct Config {
    /// 一天从本地时间几点开始，`HH:MM`。
    day_boundary: String,
    /// 不对外展示的来源。
    hidden_sources: Vec<String>,
    /// 只对 agent（MCP 工具）额外隐藏的来源：agent 读到的内容会发到它的服务商那里。
    agent_hidden_sources: Vec<String>,
    /// 时间线里，短于这么多秒的窗口段不列出来。
    timeline_min_secs: u64,
    /// 时间线里，同一个窗口中间只隔了这么多秒以内的，合成一项。
    timeline_merge_gap_secs: u64,
    /// 时间线里，短于这么多秒的项并进前面的长项，记在「其间」里。0 = 不并。
    timeline_absorb_secs: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            day_boundary: "04:00".to_owned(),
            hidden_sources: Vec::new(),
            agent_hidden_sources: Vec::new(),
            timeline_min_secs: 10,
            timeline_merge_gap_secs: 60,
            timeline_absorb_secs: 60,
        }
    }
}

/// 窗口时段和离开时段各是哪种活动。
const WINDOW: (&str, &str) = ("focus", "window_focus");
const AWAY: (&str, &str) = ("afk", "away");
const COMMAND: (&str, &str) = ("shell", "command");
const VISIT: (&str, &str) = ("browsing", "visit");

/// 一个逻辑日的原料。
struct Day {
    date: Date,
    range: (i64, i64),
    offset_min: i32,
    windows: Vec<Activity>,
    away: Vec<Activity>,
}

#[derive(Clone)]
struct Service {
    read: Arc<dyn ActivityRead>,
    clock: Arc<dyn Clock>,
    boundary: Time,
    boundary_text: String,
    hidden: Vec<String>,
    timeline_min_ms: i64,
    timeline_merge_gap_ms: i64,
    timeline_absorb_ms: i64,
    /// 没有启用 `search` 时是 `None`。
    search: Option<Arc<dyn Search>>,
}

impl Service {
    fn hidden(&self, source: &str) -> bool {
        self.hidden.iter().any(|s| s == source)
    }

    async fn list(
        &self,
        range: (i64, i64),
        (source, kind): (&str, &str),
    ) -> Result<Vec<Activity>, AppError> {
        let query = ActivityQuery {
            range: TimeRange {
                start: range.0,
                end: range.1,
            },
            source: Some(source.to_owned()),
            kind: Some(kind.to_owned()),
        };
        Ok(self
            .read
            .list(query)
            .await?
            .into_iter()
            .map(|s| s.activity)
            .collect())
    }

    async fn resolve(&self, spec: &str) -> Result<(Date, Moment), AppError> {
        let now = self.clock.now().await?;
        let date = resolve_date(spec, now, self.boundary).map_err(AppError::validation)?;
        Ok((date, now))
    }

    /// 读一个逻辑日的窗口时段和离开时段。
    async fn load(&self, date: Date, offset_min: i32) -> Result<Day, AppError> {
        let range = day_range(date, offset_min, self.boundary).map_err(AppError::validation)?;
        let windows = if self.hidden(WINDOW.0) {
            Vec::new()
        } else {
            self.list(range, WINDOW).await?
        };
        // 隐藏了离开时段也照样扣：隐藏只是不展示
        let away = self.list(range, AWAY).await?;
        Ok(Day {
            date,
            range,
            offset_min,
            windows,
            away,
        })
    }

    /// 一段时间里的命令（来源被隐藏时是空的）。
    async fn commands_in(&self, range: (i64, i64)) -> Result<Vec<CommandEntry>, AppError> {
        if self.hidden(COMMAND.0) {
            return Ok(Vec::new());
        }
        Ok(command_entries(&self.list(range, COMMAND).await?))
    }

    /// 一段时间里访问的网页（来源被隐藏时是空的）。
    async fn visits_in(&self, range: (i64, i64)) -> Result<Vec<VisitEntry>, AppError> {
        if self.hidden(VISIT.0) {
            return Ok(Vec::new());
        }
        Ok(visit_entries(&self.list(range, VISIT).await?))
    }

    fn report(&self, day: &Day, by: By) -> DayReport {
        let (pieces, away) = effective(&day.windows, &day.away, day.range);
        let rows: Vec<DayRow> = group(&pieces, by)
            .into_iter()
            .map(|(key, total_ms, spans)| DayRow {
                key,
                total_ms,
                spans,
            })
            .collect();
        let away_ms = if self.hidden(AWAY.0) {
            0
        } else {
            away.iter().map(|(s, e)| e - s).sum()
        };
        DayReport {
            date: day.date.to_string(),
            day_boundary: self.boundary_text.clone(),
            start: day.range.0,
            end: day.range.1,
            tz_offset_min: day.offset_min,
            by,
            total_ms: rows.iter().map(|r| r.total_ms).sum(),
            away_ms,
            rows,
        }
    }
}

#[async_trait]
impl Memory for Service {
    async fn day(&self, date: String, by: By) -> Result<DayReport, AppError> {
        let (date, now) = self.resolve(&date).await?;
        let day = self.load(date, now.offset_min).await?;
        Ok(self.report(&day, by))
    }

    async fn timeline(&self, date: String) -> Result<Timeline, AppError> {
        let (date, now) = self.resolve(&date).await?;
        let day = self.load(date, now.offset_min).await?;
        let (pieces, mut away) = effective(&day.windows, &day.away, day.range);
        let entries_away = if self.hidden(AWAY.0) {
            Vec::new()
        } else {
            std::mem::take(&mut away)
        };
        let mut entries = absorb(
            timeline_entries(
                &pieces,
                &entries_away,
                day.offset_min,
                self.timeline_min_ms,
                self.timeline_merge_gap_ms,
            ),
            self.timeline_absorb_ms,
        );
        attach(
            &mut entries,
            &self.commands_in(day.range).await?,
            &self.visits_in(day.range).await?,
        );
        Ok(Timeline {
            date: day.date.to_string(),
            day_boundary: self.boundary_text.clone(),
            start: day.range.0,
            end: day.range.1,
            tz_offset_min: day.offset_min,
            entries,
        })
    }

    async fn commands(&self, date: String) -> Result<CommandList, AppError> {
        let (date, now) = self.resolve(&date).await?;
        let range = day_range(date, now.offset_min, self.boundary).map_err(AppError::validation)?;
        Ok(CommandList {
            date: date.to_string(),
            day_boundary: self.boundary_text.clone(),
            start: range.0,
            end: range.1,
            tz_offset_min: now.offset_min,
            entries: self.commands_in(range).await?,
        })
    }

    async fn projects(&self, date: String) -> Result<ProjectDay, AppError> {
        let (date, now) = self.resolve(&date).await?;
        let day = self.load(date, now.offset_min).await?;
        let (pieces, away) = effective(&day.windows, &day.away, day.range);
        let rows = project_rows(
            &pieces,
            &self.commands_in(day.range).await?,
            &self.visits_in(day.range).await?,
        );
        Ok(ProjectDay {
            date: day.date.to_string(),
            day_boundary: self.boundary_text.clone(),
            start: day.range.0,
            end: day.range.1,
            tz_offset_min: day.offset_min,
            away_ms: if self.hidden(AWAY.0) {
                0
            } else {
                away.iter().map(|(s, e)| e - s).sum()
            },
            rows,
        })
    }

    async fn visits(&self, date: String) -> Result<Visits, AppError> {
        let (date, now) = self.resolve(&date).await?;
        let range = day_range(date, now.offset_min, self.boundary).map_err(AppError::validation)?;
        Ok(Visits {
            date: date.to_string(),
            day_boundary: self.boundary_text.clone(),
            start: range.0,
            end: range.1,
            tz_offset_min: now.offset_min,
            entries: self.visits_in(range).await?,
        })
    }

    async fn search(&self, request: SearchRequest) -> Result<SearchReport, AppError> {
        let search = self
            .search
            .as_ref()
            .ok_or_else(|| AppError::unavailable("这个程序里没有启用检索（search 组件）"))?;
        let now = self.clock.now().await?;
        let day = |spec: &str| -> Result<(i64, i64), AppError> {
            let date = resolve_date(spec, now, self.boundary).map_err(AppError::validation)?;
            day_range(date, now.offset_min, self.boundary).map_err(AppError::validation)
        };
        let since = request.since.as_deref().map(day).transpose()?.map(|r| r.0);
        let until = request.until.as_deref().map(day).transpose()?.map(|r| r.1);
        if let Some(source) = &request.source
            && self.hidden(source)
        {
            return Ok(SearchReport {
                text: request.text,
                hits: Vec::new(),
                documents: Vec::new(),
                truncated: false,
            });
        }
        let result = search
            .search(SearchQuery {
                text: request.text.clone(),
                since,
                until,
                sources: request.source.into_iter().collect(),
                exclude_sources: self.hidden.clone(),
                limit: request.limit,
            })
            .await?;
        Ok(SearchReport {
            text: request.text,
            hits: result.hits,
            documents: result.documents,
            truncated: result.truncated,
        })
    }

    async fn week(&self, date: String, by: By) -> Result<WeekReport, AppError> {
        let (date, now) = self.resolve(&date).await?;
        let dates = week_dates(date).map_err(AppError::validation)?;
        let mut reports = Vec::with_capacity(7);
        for date in &dates {
            let day = self.load(*date, now.offset_min).await?;
            reports.push(self.report(&day, by));
        }

        let mut rows: Vec<WeekRow> = Vec::new();
        for (i, report) in reports.iter().enumerate() {
            for row in &report.rows {
                let index = match rows.iter().position(|r| r.key == row.key) {
                    Some(index) => index,
                    None => {
                        rows.push(WeekRow {
                            key: row.key.clone(),
                            days_ms: vec![0; 7],
                            total_ms: 0,
                        });
                        rows.len() - 1
                    }
                };
                rows[index].days_ms[i] += row.total_ms;
                rows[index].total_ms += row.total_ms;
            }
        }
        rows.sort_by(|a, b| {
            b.total_ms
                .cmp(&a.total_ms)
                .then_with(|| a.key.is_none().cmp(&b.key.is_none()))
                .then_with(|| a.key.cmp(&b.key))
        });

        let days: Vec<WeekDay> = reports
            .iter()
            .map(|r| WeekDay {
                date: r.date.clone(),
                total_ms: r.total_ms,
                away_ms: r.away_ms,
            })
            .collect();
        Ok(WeekReport {
            week_start: dates[0].to_string(),
            day_boundary: self.boundary_text.clone(),
            by,
            total_ms: days.iter().map(|d| d.total_ms).sum(),
            away_ms: days.iter().map(|d| d.away_ms).sum(),
            days,
            rows,
        })
    }
}

struct Commands(Arc<Service>);

fn arg(name: &str, help: &str, kind: ArgKind) -> ArgSpec {
    ArgSpec {
        name: name.to_owned(),
        help: help.to_owned(),
        kind,
    }
}

fn date_arg(help: &str) -> ArgSpec {
    arg("date", help, ArgKind::Positional)
}

fn by_arg(values: &[&str], help: &str) -> ArgSpec {
    arg(
        "by",
        help,
        ArgKind::Option {
            values: values.iter().map(|v| (*v).to_owned()).collect(),
            default: Some("app".to_owned()),
        },
    )
}

fn command(name: &str, about: &str, args: Vec<ArgSpec>) -> CommandSpec {
    CommandSpec {
        name: name.to_owned(),
        about: about.to_owned(),
        args,
    }
}

fn json<T: Serialize>(value: &T) -> Result<String, AppError> {
    serde_json::to_string(value).map_err(|e| AppError::unavailable(format!("结果写不成 JSON：{e}")))
}

const DATE_HELP: &str = "today（默认）、yesterday、3d（3 天前）或 2026-11-12";

#[async_trait]
impl CommandProvider for Commands {
    async fn commands(&self) -> Result<Vec<CommandSpec>, AppError> {
        let by = || by_arg(&["app", "title", "project"], "按应用、窗口标题还是项目统计");
        Ok(vec![
            command(
                "day",
                "某一天在各个窗口上花的时间",
                vec![date_arg(DATE_HELP), by()],
            ),
            command(
                "today",
                "今天在各个窗口上花的时间（等于 day today）",
                vec![by()],
            ),
            command(
                "timeline",
                "某一天按时间顺序做了什么",
                vec![date_arg(DATE_HELP)],
            ),
            command(
                "commands",
                "某一天在 shell 里执行过的命令",
                vec![date_arg(DATE_HELP)],
            ),
            command(
                "search",
                "在窗口标题、命令、网页里找，最近的在前",
                vec![
                    arg(
                        "words",
                        "要找的词；每个词都要出现，最后一个英文词按前缀匹配",
                        ArgKind::Rest,
                    ),
                    arg(
                        "since",
                        "从这一天起，写法同 day",
                        ArgKind::Option {
                            values: vec![],
                            default: None,
                        },
                    ),
                    arg(
                        "until",
                        "到这一天为止（含），写法同 day",
                        ArgKind::Option {
                            values: vec![],
                            default: None,
                        },
                    ),
                    arg(
                        "source",
                        "只找一种：focus（窗口）、shell（命令）、browsing（网页）",
                        ArgKind::Option {
                            values: ["focus", "shell", "browsing"].map(str::to_owned).to_vec(),
                            default: None,
                        },
                    ),
                    arg(
                        "limit",
                        "最多列几条",
                        ArgKind::Option {
                            values: vec![],
                            default: Some("30".to_owned()),
                        },
                    ),
                ],
            ),
            command(
                "projects",
                "某一天在各个项目上：窗口时长、命令数、网页数",
                vec![date_arg(DATE_HELP)],
            ),
            command(
                "visits",
                "某一天在浏览器里访问过的网页",
                vec![date_arg(DATE_HELP)],
            ),
            command(
                "week",
                "某一天所在的那一周（周一到周日），每天在各个窗口上花的时间",
                vec![date_arg("这一周里的任意一天，写法同 day；默认今天"), by()],
            ),
        ])
    }

    async fn run(&self, invocation: Invocation) -> Result<Output, AppError> {
        let date = match invocation.command.as_str() {
            "today" => "today".to_owned(),
            _ => invocation
                .args
                .get("date")
                .cloned()
                .unwrap_or_else(|| "today".to_owned()),
        };
        let by = match invocation.args.get("by").map(String::as_str) {
            Some("title") => By::Title,
            Some("project") => By::Project,
            _ => By::App,
        };
        let format = invocation.format;
        let text = match invocation.command.as_str() {
            "day" | "today" => {
                let report = self.0.day(date, by).await?;
                match format {
                    Format::Json => json(&report)?,
                    Format::Text { color, .. } => render_day(&report, color),
                }
            }
            "timeline" => {
                let timeline = self.0.timeline(date).await?;
                match format {
                    Format::Json => json(&timeline)?,
                    Format::Text { color, width } => render_timeline(&timeline, color, width),
                }
            }
            "commands" => {
                let commands = self.0.commands(date).await?;
                match format {
                    Format::Json => json(&commands)?,
                    Format::Text { color, width } => render_commands(&commands, color, width),
                }
            }
            "search" => {
                let args = &invocation.args;
                let text = args.get("words").cloned().unwrap_or_default();
                if text.trim().is_empty() {
                    return Err(AppError::validation(
                        "要给出要找的词，例如 lifelog search jiff 文档",
                    ));
                }
                let limit = match args.get("limit") {
                    Some(l) => l.parse::<u32>().ok().filter(|l| *l > 0).ok_or_else(|| {
                        AppError::validation(format!("--limit 要是正整数，不是 `{l}`"))
                    })?,
                    None => 30,
                };
                let report = self
                    .0
                    .search(SearchRequest {
                        text,
                        since: args.get("since").cloned(),
                        until: args.get("until").cloned(),
                        source: args.get("source").cloned(),
                        limit,
                    })
                    .await?;
                match format {
                    Format::Json => json(&report)?,
                    Format::Text { color, width } => render_search(&report, color, width),
                }
            }
            "projects" => {
                let projects = self.0.projects(date).await?;
                match format {
                    Format::Json => json(&projects)?,
                    Format::Text { color, .. } => render_projects(&projects, color),
                }
            }
            "visits" => {
                let visits = self.0.visits(date).await?;
                match format {
                    Format::Json => json(&visits)?,
                    Format::Text { color, width } => render_visits(&visits, color, width),
                }
            }
            "week" => {
                let report = self.0.week(date, by).await?;
                match format {
                    Format::Json => json(&report)?,
                    Format::Text { color, .. } => render_week(&report, color),
                }
            }
            other => return Err(AppError::not_found(format!("query 没有子命令 `{other}`"))),
        };
        Ok(Output { text })
    }
}

/// 终端界面的页面。翻页的一格：日和时间线是一天，周是一周。
struct Pages(Arc<Service>);

impl Pages {
    /// 今天往前挪 `days` 天，写成 `Memory` 认识的日期。
    async fn date(&self, days: i64) -> Result<String, AppError> {
        let (today, _) = self.0.resolve("today").await?;
        Ok(shift(today, days)
            .map_err(AppError::validation)?
            .to_string())
    }
}

fn page(id: &str, title: &str, key: char, order: i32) -> PageSpec {
    PageSpec {
        id: id.to_owned(),
        title: title.to_owned(),
        key,
        order,
        input: None,
    }
}

#[async_trait]
impl PageProvider for Pages {
    async fn pages(&self) -> Result<Vec<PageSpec>, AppError> {
        let mut pages = vec![
            page("day", "日", 'd', 10),
            page("timeline", "时间线", 't', 20),
            page("commands", "命令", 'c', 25),
            page("visits", "网页", 'v', 27),
            page("week", "周", 'w', 30),
        ];
        // 进程里没有启用检索时，就没有搜索页
        if self.0.search.is_some() {
            let mut search = page("search", "搜索", 's', 40);
            search.input = Some("找什么".to_owned());
            pages.push(search);
        }
        Ok(pages)
    }

    async fn render(&self, page: String, request: PageRequest) -> Result<PageView, AppError> {
        let offset = request.offset.min(0);
        match page.as_str() {
            "day" => {
                let date = self.date(offset).await?;
                let by_app = self.0.day(date.clone(), By::App).await?;
                let by_title = self.0.day(date.clone(), By::Title).await?;
                let projects = self.0.projects(date).await?;
                Ok(day_page(&by_app, &projects, &by_title))
            }
            "timeline" => Ok(timeline_page(
                &self.0.timeline(self.date(offset).await?).await?,
            )),
            "commands" => Ok(commands_page(
                &self.0.commands(self.date(offset).await?).await?,
            )),
            "visits" => Ok(visits_page(&self.0.visits(self.date(offset).await?).await?)),
            "search" => {
                if request.input.trim().is_empty() {
                    return Ok(search_page(None));
                }
                let report = self
                    .0
                    .search(SearchRequest {
                        text: request.input.clone(),
                        since: None,
                        until: None,
                        source: None,
                        limit: SEARCH_PAGE_LIMIT,
                    })
                    .await;
                match report {
                    Ok(report) => Ok(search_page(Some(&report))),
                    // 输入的全是标点之类：当成没找到，不算出错
                    Err(e) if e.kind == kernel_api::ErrorKind::Validation => {
                        Ok(search_page(Some(&SearchReport {
                            text: request.input.clone(),
                            hits: Vec::new(),
                            documents: Vec::new(),
                            truncated: false,
                        })))
                    }
                    Err(e) => Err(e),
                }
            }
            "week" => Ok(week_page(
                &self
                    .0
                    .week(self.date(offset.saturating_mul(7)).await?, By::App)
                    .await?,
            )),
            other => Err(AppError::not_found(format!("query 没有页面 `{other}`"))),
        }
    }
}

/// 搜索页最多列几条。终端里可以滚动，比命令行默认的多一些。
const SEARCH_PAGE_LIMIT: u32 = 200;

fn millis(secs: u64) -> i64 {
    i64::try_from(secs.saturating_mul(1_000)).unwrap_or(i64::MAX)
}

pub struct QueryComponent;

impl Component for QueryComponent {
    fn config_defaults(&self) -> Option<ConfigSection> {
        ConfigSection::defaults::<Config>()
    }

    fn manifest(&self) -> Manifest {
        Manifest {
            id: "query",
            kind: Kind::Feature,
            summary: "对外的只读查询",
            provides: vec![MEMORY.id()],
            requires: vec![ACTIVITY_READ.id(), CLOCK.id()],
            optional_requires: vec![
                CLI_COMMANDS.id(),
                TUI_PAGES.id(),
                SEARCH.id(),
                MCP_TOOLS.id(),
            ],
        }
    }

    fn start(&self, config: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
        let config: Config = config.parse()?;
        let boundary = parse_boundary(&config.day_boundary).map_err(AppError::config)?;
        let service = Arc::new(Service {
            read: host.resolve(&ACTIVITY_READ)?,
            clock: host.resolve(&CLOCK)?,
            boundary,
            boundary_text: config.day_boundary,
            hidden: config.hidden_sources.clone(),
            timeline_min_ms: millis(config.timeline_min_secs),
            timeline_merge_gap_ms: millis(config.timeline_merge_gap_secs),
            timeline_absorb_ms: millis(config.timeline_absorb_secs),
            search: host.try_resolve(&SEARCH)?,
        });
        // 给 agent 的那份：再多隐藏几个来源
        let mut agent_hidden = config.hidden_sources;
        agent_hidden.extend(config.agent_hidden_sources);
        let agent = Arc::new(Service {
            hidden: agent_hidden,
            ..(*service).clone()
        });
        // 进程里没有命令行、终端界面、工具服务时，这些登记没有效果
        host.contribute(&CLI_COMMANDS, Arc::new(Commands(service.clone())))?;
        host.contribute(&TUI_PAGES, Arc::new(Pages(service.clone())))?;
        host.contribute(&MCP_TOOLS, Arc::new(tools::Tools(agent)))?;
        host.provide(&MEMORY, service)
    }
}

register_component!(QueryComponent);
