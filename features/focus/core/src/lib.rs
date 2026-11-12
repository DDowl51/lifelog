//! `focus` 的纯逻辑：把「焦点在哪个窗口上」变成「窗口使用时段」。

mod title;
mod tracker;

pub use title::TitleRules;
pub use tracker::FocusTracker;
