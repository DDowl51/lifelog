//! 启动库：各个启动器（`apps/*`）公用的部分。
//!
//! kernel 不读文件、不认识文件格式、没有默认值；这些都在这里。
//! 这个包不认识任何具体组件：组件由启动器链接进来，经 `kernel_api::registered_components` 收集。

mod config;
mod flags;
mod launch;
mod trace;

pub use config::{Settings, config_path, load};
pub use flags::{Flags, launcher_flags};
pub use launch::{App, Shutdown, block_on, socket_path, start, wait};
pub use trace::{StderrFaults, StderrTrace, report, say};
