//! `activity` 的纯逻辑：校验、处理器排序、提交用例。
//!
//! 这里不碰数据库。存储是一个接口（[`Repo`]），由 `component` 包用 `Sql` 契约实现。

mod order;
mod submit;
mod validate;

pub use order::order_processors;
pub use submit::{Repo, Submit};
pub use validate::validate;
