//! `query` 的纯逻辑：逻辑日、日期参数、扣除离开时间、报告的文本排版。

mod absorb;
mod commands;
mod day;
mod effective;
mod mix;
mod pages;
mod render;
mod search;
mod visits;

pub use absorb::absorb;
pub use commands::{command_entries, commands_page, render_commands};
pub use day::{day_range, logical_date, parse_boundary, resolve_date, shift, week_dates};
pub use effective::{Piece, effective, group, timeline_entries};
pub use mix::{attach, project_rows};
pub use pages::{day_page, timeline_page, week_page};
pub use render::{format_duration, render_day, render_projects, render_timeline, render_week};
pub use search::{render_search, search_page};
pub use visits::{render_visits, visit_entries, visits_page};
