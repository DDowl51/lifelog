//! 契约：增量读 shell 执行过的命令。
//!
//! 提供者是某个记录 shell 历史的程序的适配（契约里不出现它的名字）。
//! 读的一方保存提供者给的游标，下次从那里接着读。

use kernel_api::{AppError, contract};
use serde::{Deserialize, Serialize};

/// 执行过的一条命令。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellCommand {
    /// 提供者给的标识，同一条命令每次读到都一样。
    pub id: String,
    /// 开始时刻，UTC 毫秒。
    pub started_at: i64,
    /// 执行了多久。不知道时是 `None`（比如从别的历史导入的、没等到结束的）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    /// 退出码。不知道时是 `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<i32>,
    /// 命令原文，可能有多行。
    pub command: String,
    /// 在哪个目录执行的，绝对路径。不知道时是 `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// 同一个 shell 会话里的命令有相同的会话标识。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
}

/// 读到的一页。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryPage {
    /// 按开始时刻升序。
    pub commands: Vec<ShellCommand>,
    /// 下次从这里接着读。内容由提供者决定，读的一方只保存、原样传回。
    pub next: String,
    /// 现在马上再读一次，可能还有。
    pub more: bool,
}

contract! {
    /// 增量读 shell 历史。
    pub trait ShellHistory as SHELL_HISTORY = service("shell.history", 1) {
        /// 从游标 `after` 之后读最多 `limit` 条（`None` 表示从头读）。
        ///
        /// 还在执行的命令先不给；为了等它，提供者可能把它之后已经给过的命令再给一次。
        /// 读的一方按 `id` 去重。
        async fn read(&self, after: Option<String>, limit: u32) -> Result<HistoryPage, AppError>;
    }
}

#[cfg(feature = "testing")]
pub mod testing {
    //! 契约测试，和一个放在内存里的假实现。

    use std::future::Future;
    use std::sync::{Arc, Mutex, PoisonError};

    use kernel_api::{AppError, Component, ConfigSection, Host, Kind, Manifest, async_trait};

    use super::{HistoryPage, SHELL_HISTORY, ShellCommand, ShellHistory};

    /// 一条已经结束的命令，测试里用。
    pub fn finished(id: &str, started_at: i64, command: &str) -> ShellCommand {
        ShellCommand {
            id: id.to_owned(),
            started_at,
            duration_ms: Some(1_500),
            exit: Some(0),
            command: command.to_owned(),
            cwd: Some("/srv/projects/demo".to_owned()),
            session: Some("s1".to_owned()),
        }
    }

    /// 契约测试。`history` 开始时是空的；`add` 往它的数据源里追加已经结束的命令。
    pub async fn shell_history_contract<F, Fut>(
        history: Arc<dyn ShellHistory>,
        add: F,
    ) -> Result<(), AppError>
    where
        F: Fn(Vec<ShellCommand>) -> Fut,
        Fut: Future<Output = ()>,
    {
        let empty = history.read(None, 10).await?;
        assert!(empty.commands.is_empty() && !empty.more, "{empty:?}");

        let first = vec![
            finished("a", 1_000, "ls"),
            finished("b", 2_000, "cargo test\n  --all"),
            ShellCommand {
                exit: Some(101),
                cwd: None,
                session: None,
                ..finished("c", 3_000, "make")
            },
        ];
        add(first.clone()).await;
        let page = history.read(None, 10).await?;
        assert_eq!(page.commands, first, "字段原样带回，按开始时刻升序");
        assert!(!page.more);
        let rest = history.read(Some(page.next.clone()), 10).await?;
        assert!(rest.commands.is_empty(), "接着读没有新的：{rest:?}");

        // 分页
        let one = history.read(None, 2).await?;
        assert_eq!(one.commands, first[..2]);
        assert!(one.more, "一页装满了，应该提示还有");
        let two = history.read(Some(one.next), 2).await?;
        assert_eq!(two.commands, first[2..]);
        assert!(!two.more);

        // 新追加的只读到新的
        add(vec![finished("d", 4_000, "git status")]).await;
        let new = history.read(Some(page.next), 10).await?;
        assert_eq!(new.commands, [finished("d", 4_000, "git status")]);
        Ok(())
    }

    /// 放在内存里的历史。游标是已经读过的条数。克隆出来的是同一份。
    #[derive(Clone, Default)]
    pub struct MemoryShell(Arc<Mutex<Vec<ShellCommand>>>);

    impl MemoryShell {
        pub fn push(&self, commands: impl IntoIterator<Item = ShellCommand>) {
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .extend(commands);
        }
    }

    #[async_trait]
    impl ShellHistory for MemoryShell {
        async fn read(&self, after: Option<String>, limit: u32) -> Result<HistoryPage, AppError> {
            let all = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            let start = match after {
                Some(cursor) => cursor
                    .parse::<usize>()
                    .map_err(|_| AppError::validation(format!("看不懂的游标 `{cursor}`")))?,
                None => 0,
            };
            let end = all.len().min(start + limit as usize);
            let commands = all.get(start..end).unwrap_or_default().to_vec();
            Ok(HistoryPage {
                next: end.max(start).to_string(),
                more: end < all.len(),
                commands,
            })
        }
    }

    /// 组件 `shell-memory`。
    impl Component for MemoryShell {
        fn manifest(&self) -> Manifest {
            Manifest {
                id: "shell-memory",
                kind: Kind::Service,
                summary: "测试用：放在内存里的 shell 历史",
                provides: vec![SHELL_HISTORY.id()],
                requires: vec![],
                optional_requires: vec![],
            }
        }

        fn start(&self, _config: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
            host.provide(&SHELL_HISTORY, Arc::new(self.clone()))
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[tokio::test]
        async fn memory_shell_passes_the_contract() {
            let shell = MemoryShell::default();
            let source = shell.clone();
            shell_history_contract(Arc::new(shell), |commands| {
                source.push(commands);
                async {}
            })
            .await
            .unwrap();
        }
    }
}
