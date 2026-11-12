//! 把状态画到 ratatui 的帧上。

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style as RStyle};
use ratatui::text::{Line as RLine, Span as RSpan, Text};
use ratatui::widgets::Paragraph;
use tui_contract::{Line, Style};

use crate::app::{App, layout};

fn style(style: Style) -> RStyle {
    match style {
        Style::Normal => RStyle::default(),
        Style::Dim => RStyle::default().fg(Color::DarkGray),
        Style::Accent => RStyle::default().fg(Color::Cyan),
        Style::Strong => RStyle::default().add_modifier(Modifier::BOLD),
    }
}

fn convert(line: &Line) -> RLine<'static> {
    RLine::from(
        line.spans
            .iter()
            .map(|s| RSpan::styled(s.text.clone(), style(s.style)))
            .collect::<Vec<_>>(),
    )
}

const HELP: &str = "  ←/→ 前后翻  . 回到现在  ↑/↓ 滚动  Tab 换页  r 刷新  q 退出";
const HELP_INPUT: &str = "  / 输入  ↑/↓ 滚动  Tab 换页  r 刷新  q 退出";
const HELP_EDITING: &str = "  Enter 确定  Esc 取消  Ctrl-C 退出";

pub(crate) fn draw(frame: &mut Frame<'_>, app: &App) {
    let prompt = app.prompt();
    let [tabs, input, body, help] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(u16::from(prompt.is_some())),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    let mut spans = vec![RSpan::raw(" ")];
    for (i, page) in app.pages.iter().enumerate() {
        let label = format!(" {} {} ", page.key, page.title);
        let s = if i == app.current {
            RStyle::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
        } else {
            RStyle::default().fg(Color::DarkGray)
        };
        spans.push(RSpan::styled(label, s));
        spans.push(RSpan::raw(" "));
    }
    frame.render_widget(Paragraph::new(RLine::from(spans)), tabs);

    let lines: Vec<RLine<'static>> = match (&app.view, app.pages.is_empty()) {
        (_, true) => vec![RLine::styled("  没有组件登记页面", style(Style::Dim))],
        (None, false) => vec![RLine::styled("  读取中……", style(Style::Dim))],
        (Some(Err(why)), false) => vec![RLine::styled(
            format!("  出错了：{why}"),
            RStyle::default().fg(Color::Red),
        )],
        (Some(Ok(view)), false) => layout(view, body.width).iter().map(convert).collect(),
    };
    // 主体上下各留一行空白；滚动上限按真正看得见的高度算，否则滚到底时最后几行会被截掉
    let visible = body.inner(ratatui::layout::Margin {
        horizontal: 0,
        vertical: 1,
    });
    let max_scroll = u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .saturating_sub(visible.height);
    let body_text = Paragraph::new(Text::from(lines)).scroll((app.scroll.min(max_scroll), 0));
    frame.render_widget(body_text, visible);

    if let Some(prompt) = prompt {
        let mut spans = vec![RSpan::styled(format!("  {prompt}："), style(Style::Dim))];
        match &app.editing {
            Some(buffer) => {
                spans.push(RSpan::styled(buffer.clone(), style(Style::Strong)));
                spans.push(RSpan::styled("▏", style(Style::Accent)));
            }
            None => spans.push(RSpan::raw(app.input().to_owned())),
        }
        frame.render_widget(Paragraph::new(RLine::from(spans)), input);
    }

    let help_text = match (app.editing.is_some(), prompt.is_some()) {
        (true, _) => HELP_EDITING,
        (false, true) => HELP_INPUT,
        (false, false) => HELP,
    };
    frame.render_widget(
        Paragraph::new(RLine::styled(help_text, style(Style::Dim))),
        help,
    );
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use tui_contract::{Bar, Block, PageSpec, PageView};

    use super::*;

    fn screen(app: &App, w: u16, h: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..h)
            .map(|y| {
                // 中文占两格，第二格在缓冲区里是个占位的空格，跳过
                let mut row = String::new();
                let mut x = 0;
                while x < w {
                    let symbol = buffer[(x, y)].symbol();
                    row.push_str(symbol);
                    x += u16::try_from(unicode_width::UnicodeWidthStr::width(symbol).max(1))
                        .unwrap_or(1);
                }
                row.trim_end().to_owned()
            })
            .collect()
    }

    fn app() -> App {
        let mut app = App::new(vec![
            PageSpec {
                id: "day".to_owned(),
                title: "日".to_owned(),
                key: 'd',
                order: 1,
                input: None,
            },
            PageSpec {
                id: "week".to_owned(),
                title: "周".to_owned(),
                key: 'w',
                order: 2,
                input: None,
            },
        ]);
        app.view = Some(Ok(PageView {
            heading: "2026-10-20 周二".to_owned(),
            note: "共 40 分".to_owned(),
            blocks: vec![Block::Bars {
                bars: vec![Bar {
                    label: "zen".to_owned(),
                    value: 1,
                    value_text: "40 分".to_owned(),
                }],
            }],
        }));
        app
    }

    #[test]
    fn draws_tabs_page_and_help() {
        let rows = screen(&app(), 70, 8);
        assert!(
            rows[0].contains("d 日") && rows[0].contains("w 周"),
            "{rows:#?}"
        );
        assert_eq!(rows[1], "");
        assert!(
            rows[2].contains("2026-10-20 周二") && rows[2].contains("共 40 分"),
            "{rows:#?}"
        );
        assert!(
            rows[4].contains("40 分") && rows[4].contains('█') && rows[4].ends_with("zen"),
            "{rows:#?}"
        );
        assert!(rows[7].contains("q 退出"), "{rows:#?}");
    }

    #[test]
    fn the_input_box_and_its_help() {
        let mut a = app();
        a.pages[0].input = Some("找什么".to_owned());
        a.editing = Some("气象".to_owned());
        let rows = screen(&a, 70, 8);
        assert_eq!(rows[1], "  找什么：气象▏", "{rows:#?}");
        assert!(rows[7].contains("Enter 确定"), "{rows:#?}");
        a.inputs[0] = "气象站".to_owned();
        a.editing = None;
        let rows = screen(&a, 70, 8);
        assert_eq!(rows[1], "  找什么：气象站");
        assert!(rows[7].contains("/ 输入"), "{rows:#?}");
    }

    #[test]
    fn shows_errors_and_empty_states() {
        let mut a = app();
        a.view = Some(Err("数据库打不开".to_owned()));
        assert!(
            screen(&a, 60, 6)
                .iter()
                .any(|r| r.contains("出错了：数据库打不开"))
        );
        a.view = None;
        assert!(screen(&a, 60, 6).iter().any(|r| r.contains("读取中")));
        let empty = App::new(vec![]);
        assert!(
            screen(&empty, 60, 6)
                .iter()
                .any(|r| r.contains("没有组件登记页面"))
        );
    }

    #[test]
    fn scrolling_moves_the_body_but_not_beyond_the_end() {
        let mut a = app();
        a.view = Some(Ok(PageView {
            heading: "h".to_owned(),
            note: String::new(),
            blocks: vec![Block::Lines {
                lines: (0..20)
                    .map(|i| tui_contract::Line {
                        spans: vec![tui_contract::Span::new(
                            format!("  第 {i} 行"),
                            Style::Normal,
                        )],
                    })
                    .collect(),
            }],
        }));
        a.scroll = 5;
        let rows = screen(&a, 40, 10);
        assert!(rows[2].contains("第 3 行"), "{rows:#?}");
        a.scroll = 1000;
        let rows = screen(&a, 40, 10);
        assert!(rows[7].contains("第 19 行"), "滚到底就停：{rows:#?}");
    }
}
