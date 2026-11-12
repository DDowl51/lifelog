//! kernel 的实现：生命周期、命名、消息传递、监听点。
//!
//! 只有机制，没有策略：队列多大、停止等多久、导出哪些契约都由启动器给。
//! 不读配置文件、不打日志、不认识任何契约和组件。
//! 唯一的 IO 是跨进程调用用的本地 socket（`remote`）：IPC 是微内核的本职。

mod plan;
mod remote;
mod runtime;

pub use remote::Export;
pub use runtime::{Delivery, Options, Running, start};
