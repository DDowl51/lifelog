//! 界面的状态和按键处理，以及把页面内容排成行。纯逻辑，不碰终端。

use tui_contract::{Bar, Block, Line, PageSpec, PageView, Span, Style};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// 外壳关心的按键。和终端库无关。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Key {
    Char(char),
    Left,
    Right,
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    Tab,
    BackTab,
    Esc,
    Enter,
    Backspace,
    /// Ctrl-C
    Interrupt,
    Other,
}

/// 按键之后要做什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Effect {
    Nothing,
    /// 只要重画。
    Redraw,
    /// 要重新取当前页的内容。
    Reload,
    Quit,
}

/// 界面的状态。
pub(crate) struct App {
    pub(crate) pages: Vec<PageSpec>,
    pub(crate) current: usize,
    pub(crate) offset: i64,
    pub(crate) scroll: u16,
    /// 当前页的内容；取失败时是给人看的原因。
    pub(crate) view: Option<Result<PageView, String>>,
    /// 每个页面输入框里提交过的文字（没有输入框的页面一直是空的）。
    pub(crate) inputs: Vec<String>,
    /// 正在输入时，输入框里的文字。这时所有按键都进输入框。
    pub(crate) editing: Option<String>,
}

impl App {
    pub(crate) fn new(pages: Vec<PageSpec>) -> Self {
        let mut app = Self {
            inputs: vec![String::new(); pages.len()],
            pages,
            current: 0,
            offset: 0,
            scroll: 0,
            view: None,
            editing: None,
        };
        app.start_editing_if_empty();
        app
    }

    /// 当前页面的输入框提示语；没有输入框是 `None`。
    pub(crate) fn prompt(&self) -> Option<&str> {
        self.pages.get(self.current)?.input.as_deref()
    }

    /// 当前页面提交过的文字。
    pub(crate) fn input(&self) -> &str {
        self.inputs.get(self.current).map_or("", String::as_str)
    }

    /// 切到一个带输入框、还什么都没输入过的页面时，直接开始输入。
    fn start_editing_if_empty(&mut self) {
        if self.prompt().is_some() && self.input().is_empty() {
            self.editing = Some(String::new());
        }
    }

    fn switch(&mut self, index: usize) -> Effect {
        if index == self.current {
            return Effect::Nothing;
        }
        // 不同页面的「一格」意思不同（一天、一周），切页时回到现在
        self.current = index;
        self.offset = 0;
        self.scroll = 0;
        self.editing = None;
        self.start_editing_if_empty();
        Effect::Reload
    }

    /// 输入中的按键：字都进输入框，Enter 提交，Esc 放弃。
    fn edit(&mut self, key: Key) -> Effect {
        let Some(buffer) = self.editing.as_mut() else {
            return Effect::Nothing;
        };
        match key {
            Key::Char(c) => {
                buffer.push(c);
                Effect::Redraw
            }
            Key::Backspace => {
                buffer.pop();
                Effect::Redraw
            }
            Key::Enter => {
                let text = buffer.trim().to_owned();
                self.editing = None;
                if let Some(slot) = self.inputs.get_mut(self.current) {
                    *slot = text;
                }
                self.offset = 0;
                self.scroll = 0;
                Effect::Reload
            }
            Key::Esc => {
                self.editing = None;
                Effect::Redraw
            }
            Key::Interrupt => Effect::Quit,
            _ => Effect::Nothing,
        }
    }

    pub(crate) fn handle(&mut self, key: Key) -> Effect {
        if self.editing.is_some() {
            return self.edit(key);
        }
        let count = self.pages.len();
        match key {
            Key::Char('/') if self.prompt().is_some() => {
                self.editing = Some(self.input().to_owned());
                Effect::Redraw
            }
            Key::Char('q') | Key::Esc | Key::Interrupt => Effect::Quit,
            _ if count == 0 => Effect::Nothing,
            Key::Left | Key::Char('h') => {
                self.offset -= 1;
                self.scroll = 0;
                Effect::Reload
            }
            Key::Right | Key::Char('l') => {
                if self.offset >= 0 {
                    return Effect::Nothing;
                }
                self.offset += 1;
                self.scroll = 0;
                Effect::Reload
            }
            Key::Home | Key::Char('.') => {
                self.offset = 0;
                self.scroll = 0;
                Effect::Reload
            }
            Key::Char('r') => Effect::Reload,
            Key::Tab => self.switch((self.current + 1) % count),
            Key::BackTab => self.switch((self.current + count - 1) % count),
            Key::Up | Key::Char('k') => self.scroll_by(-1),
            Key::Down | Key::Char('j') => self.scroll_by(1),
            Key::PageUp => self.scroll_by(-10),
            Key::PageDown => self.scroll_by(10),
            Key::Char(c) => match self.pages.iter().position(|p| p.key == c) {
                Some(index) => self.switch(index),
                None => Effect::Nothing,
            },
            Key::Enter | Key::Backspace | Key::Other => Effect::Nothing,
        }
    }

    fn scroll_by(&mut self, delta: i32) -> Effect {
        let next = (i32::from(self.scroll) + delta).clamp(0, i32::from(u16::MAX));
        let next = u16::try_from(next).unwrap_or(0);
        if next == self.scroll {
            return Effect::Nothing;
        }
        self.scroll = next;
        Effect::Redraw
    }
}

fn width(text: &str) -> usize {
    text.width()
}

/// 截到不超过 `max` 列，截掉了就以 `…` 结尾。
pub(crate) fn truncate(text: &str, max: usize) -> String {
    if width(text) <= max {
        return text.to_owned();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = c.width().unwrap_or(0);
        if used + w + 1 > max {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

fn pad_left(text: &str, to: usize) -> String {
    format!("{}{text}", " ".repeat(to.saturating_sub(width(text))))
}

fn pad_right(text: &str, to: usize) -> String {
    format!("{text}{}", " ".repeat(to.saturating_sub(width(text))))
}

fn line(spans: Vec<Span>) -> Line {
    Line { spans }
}

fn bars(bars: &[Bar], total_width: usize) -> Vec<Line> {
    let value_width = bars.iter().map(|b| width(&b.value_text)).max().unwrap_or(0);
    let max = bars.iter().map(|b| b.value).max().unwrap_or(0).max(1);
    // 两边各留两格；数值、条、名字之间各两格
    let room = total_width.saturating_sub(2 + value_width + 2 + 2);
    let bar_width = (room / 3).clamp(4, 30);
    let label_width = room.saturating_sub(bar_width).max(8);
    bars.iter()
        .map(|b| {
            let filled = usize::try_from(
                (b.value.max(0) * i64::try_from(bar_width).unwrap_or(0) + max / 2) / max,
            )
            .unwrap_or(0)
            .clamp(usize::from(b.value > 0), bar_width);
            line(vec![
                Span::new(
                    format!("  {}  ", pad_left(&b.value_text, value_width)),
                    Style::Normal,
                ),
                Span::new("█".repeat(filled), Style::Accent),
                Span::new(" ".repeat(bar_width - filled + 2), Style::Normal),
                Span::new(truncate(&b.label, label_width), Style::Normal),
            ])
        })
        .collect()
}

fn table(header: &[String], rows: &[Vec<String>], total_width: usize) -> Vec<Line> {
    let columns = header
        .len()
        .max(rows.iter().map(Vec::len).max().unwrap_or(0));
    let mut widths = vec![0; columns];
    for row in std::iter::once(header).chain(rows.iter().map(Vec::as_slice)) {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(width(cell));
        }
    }
    // 第一列（名字）挤一挤，让数字列放得下
    let others: usize = widths.iter().skip(1).map(|w| w + 2).sum();
    if let Some(first) = widths.first_mut() {
        *first = (*first).min(total_width.saturating_sub(2 + others).max(6));
    }
    let render = |row: &[String], style: Style| {
        let mut text = String::from("  ");
        for (i, w) in widths.iter().enumerate() {
            let cell = row.get(i).map_or("", String::as_str);
            if i == 0 {
                text.push_str(&pad_right(&truncate(cell, *w), *w));
            } else {
                text.push_str("  ");
                text.push_str(&pad_left(cell, *w));
            }
        }
        line(vec![Span::new(text, style)])
    };
    std::iter::once(render(header, Style::Dim))
        .chain(rows.iter().map(|r| render(r, Style::Normal)))
        .collect()
}

/// 把一页内容排成行。`width` 是可用的列数。
pub(crate) fn layout(view: &PageView, total_width: u16) -> Vec<Line> {
    let total_width = usize::from(total_width);
    let mut out = vec![line(vec![
        Span::new(format!("  {}", view.heading), Style::Strong),
        Span::new(format!("  {}", view.note), Style::Dim),
    ])];
    for block in &view.blocks {
        out.push(Line::default());
        match block {
            Block::Heading { text } => {
                out.push(line(vec![Span::new(format!("  {text}"), Style::Strong)]))
            }
            Block::Lines { lines } => out.extend(lines.iter().cloned()),
            Block::Bars { bars: items } => out.extend(bars(items, total_width)),
            Block::Table { header, rows } => out.extend(table(header, rows, total_width)),
            Block::Empty { message } => {
                out.push(line(vec![Span::new(format!("  {message}"), Style::Dim)]))
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(id: &str, key: char) -> PageSpec {
        PageSpec {
            id: id.to_owned(),
            title: id.to_owned(),
            key,
            order: 0,
            input: None,
        }
    }

    fn app() -> App {
        App::new(vec![
            spec("day", 'd'),
            spec("timeline", 't'),
            spec("week", 'w'),
        ])
    }

    fn text(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.text.as_str())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    fn with_search() -> App {
        let mut search = spec("search", 's');
        search.input = Some("找什么".to_owned());
        App::new(vec![spec("day", 'd'), search])
    }

    #[test]
    fn an_input_page_starts_editing_the_first_time() {
        let mut a = with_search();
        assert_eq!(a.editing, None, "第一页没有输入框");
        assert_eq!(a.handle(Key::Char('s')), Effect::Reload);
        assert_eq!(a.editing.as_deref(), Some(""));
        assert_eq!(a.prompt(), Some("找什么"));
    }

    #[test]
    fn while_editing_every_key_goes_into_the_box() {
        let mut a = with_search();
        a.handle(Key::Char('s'));
        for c in "q气象d".chars() {
            assert_eq!(
                a.handle(Key::Char(c)),
                Effect::Redraw,
                "{c} 不是退出也不是换页"
            );
        }
        assert_eq!(a.handle(Key::Backspace), Effect::Redraw);
        assert_eq!(a.current, 1);
        assert_eq!(a.handle(Key::Enter), Effect::Reload);
        assert_eq!((a.editing.as_deref(), a.input()), (None, "q气象"));

        // 改：带着上次的文字进去，Esc 放弃改动
        assert_eq!(a.handle(Key::Char('/')), Effect::Redraw);
        assert_eq!(a.editing.as_deref(), Some("q气象"));
        a.handle(Key::Char('x'));
        assert_eq!(a.handle(Key::Esc), Effect::Redraw);
        assert_eq!((a.editing.as_deref(), a.input()), (None, "q气象"));

        // 切走再回来：提交过就不自动进入输入
        a.handle(Key::Char('d'));
        a.handle(Key::Char('s'));
        assert_eq!(a.editing, None);
        assert_eq!(a.input(), "q气象");
        assert_eq!(
            a.handle(Key::Char('q')),
            Effect::Quit,
            "不在输入时 q 照样退出"
        );
    }

    #[test]
    fn slash_does_nothing_without_an_input_box() {
        let mut a = app();
        assert_eq!(a.handle(Key::Char('/')), Effect::Nothing);
        assert_eq!(a.editing, None);
    }

    #[test]
    fn ctrl_c_quits_even_while_editing() {
        let mut a = with_search();
        a.handle(Key::Char('s'));
        assert_eq!(a.handle(Key::Interrupt), Effect::Quit);
    }

    #[test]
    fn paging_back_and_forth_never_goes_past_now() {
        let mut a = app();
        assert_eq!(a.handle(Key::Right), Effect::Nothing, "已经是现在了");
        assert_eq!(a.handle(Key::Left), Effect::Reload);
        assert_eq!(a.handle(Key::Char('h')), Effect::Reload);
        assert_eq!(a.offset, -2);
        assert_eq!(a.handle(Key::Right), Effect::Reload);
        assert_eq!(a.offset, -1);
        assert_eq!(a.handle(Key::Home), Effect::Reload);
        assert_eq!(a.offset, 0);
    }

    #[test]
    fn switching_pages_resets_offset_and_scroll() {
        let mut a = app();
        a.handle(Key::Left);
        a.handle(Key::Down);
        assert_eq!(a.handle(Key::Char('w')), Effect::Reload);
        assert_eq!((a.current, a.offset, a.scroll), (2, 0, 0));
        assert_eq!(a.handle(Key::Char('w')), Effect::Nothing, "已经在这一页");
        assert_eq!(a.handle(Key::Tab), Effect::Reload);
        assert_eq!(a.current, 0, "循环");
        assert_eq!(a.handle(Key::BackTab), Effect::Reload);
        assert_eq!(a.current, 2);
        assert_eq!(a.handle(Key::Char('z')), Effect::Nothing);
    }

    #[test]
    fn scrolling_and_quitting() {
        let mut a = app();
        assert_eq!(a.handle(Key::Up), Effect::Nothing, "已经在顶上");
        assert_eq!(a.handle(Key::PageDown), Effect::Redraw);
        assert_eq!(a.scroll, 10);
        assert_eq!(a.handle(Key::Char('k')), Effect::Redraw);
        assert_eq!(a.scroll, 9);
        for key in [Key::Char('q'), Key::Esc, Key::Interrupt] {
            assert_eq!(app().handle(key), Effect::Quit);
        }
    }

    #[test]
    fn with_no_pages_only_quitting_works() {
        let mut a = App::new(vec![]);
        assert_eq!(a.handle(Key::Left), Effect::Nothing);
        assert_eq!(a.handle(Key::Tab), Effect::Nothing);
        assert_eq!(a.handle(Key::Char('q')), Effect::Quit);
    }

    #[test]
    fn bars_are_scaled_and_labels_cut_to_width() {
        let view = PageView {
            heading: "2026-10-20 周二".to_owned(),
            note: "共 2 小时".to_owned(),
            blocks: vec![Block::Bars {
                bars: vec![
                    Bar {
                        label: "ghostty".to_owned(),
                        value: 100,
                        value_text: "1 小时 40 分".to_owned(),
                    },
                    Bar {
                        label: "一个非常长的名字".repeat(5),
                        value: 25,
                        value_text: "25 分".to_owned(),
                    },
                ],
            }],
        };
        let lines = text(&layout(&view, 60));
        assert_eq!(lines[0], "  2026-10-20 周二  共 2 小时");
        assert_eq!(lines[1], "");
        assert!(
            lines[2].starts_with("  1 小时 40 分  ████████████"),
            "{:?}",
            lines[2]
        );
        assert!(lines[2].ends_with("ghostty"));
        assert!(
            lines[3].starts_with("         25 分  ████  "),
            "{:?}",
            lines[3]
        );
        assert!(lines[3].ends_with('…'));
        for l in &lines {
            assert!(width(l) <= 60, "{l:?} 占了 {} 列", width(l));
        }
    }

    #[test]
    fn tables_align_numbers_right() {
        let view = PageView {
            heading: "周".to_owned(),
            note: String::new(),
            blocks: vec![Block::Table {
                header: vec![String::new(), "一".to_owned(), "合计".to_owned()],
                rows: vec![
                    vec!["ghostty".to_owned(), "1:45".to_owned(), "12:30".to_owned()],
                    vec!["zen".to_owned(), "·".to_owned(), "0:40".to_owned()],
                ],
            }],
        };
        let lines = text(&layout(&view, 80));
        assert_eq!(
            &lines[2..],
            [
                "             一   合计",
                "  ghostty  1:45  12:30",
                "  zen         ·   0:40"
            ]
        );
    }

    #[test]
    fn empty_and_heading_blocks() {
        let view = PageView {
            heading: "h".to_owned(),
            note: "n".to_owned(),
            blocks: vec![
                Block::Heading {
                    text: "按标题".to_owned(),
                },
                Block::Empty {
                    message: "没有记录".to_owned(),
                },
            ],
        };
        assert_eq!(
            text(&layout(&view, 40)),
            ["  h  n", "", "  按标题", "", "  没有记录"]
        );
    }
}
