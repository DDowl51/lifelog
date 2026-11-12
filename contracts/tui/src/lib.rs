//! 契约：终端界面。扩展点：各组件登记自己的页面。
//!
//! 外壳负责页面切换、按键、绘制；页面的内容是纯数据（文字行、条形图、表格），
//! 由登记页面的组件按请求生成。和命令行子命令是同一个模式：登记方不碰终端。
//!
//! 外壳不知道页面里是什么。「翻页」只是一个偏移：0 是现在，-1 是上一个，
//! 页面自己决定一格是一天还是一周。「输入」也一样：页面声明要一个输入框，
//! 外壳负责编辑，页面只收到提交的文字，自己决定拿它做什么。

use kernel_api::{AppError, contract};
use serde::{Deserialize, Serialize};

/// 一个页面的描述。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageSpec {
    /// 在登记它的组件里唯一。
    pub id: String,
    /// 显示在页签上。
    pub title: String,
    /// 按这个键切到这个页面。
    pub key: char,
    /// 页签的先后，小的在前。
    pub order: i32,
    /// 页面要一个输入框时，这是它的提示语（比如「找什么」）。`None` 表示没有输入框。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
}

/// 要一页内容。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageRequest {
    /// 0 是现在，-1 是上一个（前一天、前一周……由页面决定），以此类推。不会大于 0。
    pub offset: i64,
    /// 可用的列数。
    pub width: u16,
    /// 输入框里提交的文字；页面没有输入框，或者还没提交过，是空的。
    #[serde(default)]
    pub input: String,
}

/// 文字的样式。具体颜色由外壳决定。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Style {
    Normal,
    /// 次要信息。
    Dim,
    /// 要突出的名字。
    Accent,
    /// 强调。
    Strong,
}

/// 一段同样式的文字。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub text: String,
    pub style: Style,
}

impl Span {
    pub fn new(text: impl Into<String>, style: Style) -> Self {
        Self {
            text: text.into(),
            style,
        }
    }
}

/// 一行。超出宽度的部分由外壳截掉。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Line {
    pub spans: Vec<Span>,
}

/// 条形图里的一条。条的长短按 `value` 相对于这一组里的最大值。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bar {
    pub label: String,
    pub value: i64,
    /// 写在条旁边的数值。
    pub value_text: String,
}

/// 页面里的一块。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    /// 一块小标题。
    Heading {
        text: String,
    },
    Lines {
        lines: Vec<Line>,
    },
    Bars {
        bars: Vec<Bar>,
    },
    /// 表格：第一列左对齐，其余右对齐。
    Table {
        header: Vec<String>,
        rows: Vec<Vec<String>>,
    },
    /// 什么都没有时给的一句话。
    Empty {
        message: String,
    },
}

/// 一页的内容。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageView {
    /// 页面最上面的一行，比如日期。
    pub heading: String,
    /// 紧跟在后面、样式弱一点的说明，比如合计。
    pub note: String,
    pub blocks: Vec<Block>,
}

contract! {
    /// 扩展点：有页面的组件来登记。
    pub trait PageProvider as TUI_PAGES = point("tui.pages", 1) {
        /// 这个组件提供哪些页面。
        async fn pages(&self) -> Result<Vec<PageSpec>, AppError>;

        /// 生成一页的内容。
        async fn render(&self, page: String, request: PageRequest) -> Result<PageView, AppError>;
    }
}

contract! {
    /// 终端界面。
    pub trait Tui as TUI = service("tui", 1) {
        /// 接管终端，直到用户退出。返回进程应该用的退出码。
        async fn run(&self) -> Result<u8, AppError>;
    }
}

#[cfg(feature = "testing")]
pub mod testing {
    //! 假的界面外壳：不碰终端，直接把请求交给登记了那个页面的组件。

    use std::sync::{Arc, OnceLock};

    use kernel_api::{AppError, Collector, Component, ConfigSection, Host, Kind, Manifest};

    use super::{PageProvider, PageRequest, PageSpec, PageView, TUI_PAGES};

    #[derive(Clone, Default)]
    pub struct FakeTui(Arc<OnceLock<Collector<dyn PageProvider>>>);

    impl FakeTui {
        fn providers(&self) -> Result<Vec<Arc<dyn PageProvider>>, AppError> {
            Ok(self
                .0
                .get()
                .ok_or_else(|| AppError::unavailable("tui-fake 还没启动"))?
                .all())
        }

        /// 所有登记上来的页面，按 `order` 排。
        pub async fn pages(&self) -> Result<Vec<PageSpec>, AppError> {
            let mut all = Vec::new();
            for provider in self.providers()? {
                all.extend(provider.pages().await?);
            }
            all.sort_by_key(|p| p.order);
            Ok(all)
        }

        /// 要一页内容。
        pub async fn render(&self, page: &str, request: PageRequest) -> Result<PageView, AppError> {
            for provider in self.providers()? {
                if provider.pages().await?.iter().any(|p| p.id == page) {
                    return provider.render(page.to_owned(), request).await;
                }
            }
            Err(AppError::not_found(format!("没有组件登记页面 `{page}`")))
        }
    }

    /// 组件 `tui-fake`：只拥有扩展点，不提供 `Tui`。
    impl Component for FakeTui {
        fn manifest(&self) -> Manifest {
            Manifest {
                id: "tui-fake",
                kind: Kind::Service,
                summary: "测试用：假的界面外壳",
                provides: vec![TUI_PAGES.id()],
                requires: vec![],
                optional_requires: vec![],
            }
        }

        fn start(&self, _config: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
            let _ = self.0.set(host.collector(&TUI_PAGES)?);
            Ok(())
        }
    }
}
