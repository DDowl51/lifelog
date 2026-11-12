//! 契约：在一个根目录下读写文本文件。
//!
//! 路径都是根目录下的相对路径，用 `/` 分隔；绝对路径和 `..` 不行。根目录是实现的配置，
//! 使用方不知道文件实际在哪，也碰不到根目录以外的东西。

use kernel_api::{AppError, contract};
use serde::{Deserialize, Serialize};

/// 一个文件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileInfo {
    /// 根目录下的相对路径。
    pub path: String,
    /// 最后修改时刻，UTC 毫秒。
    pub modified_ms: i64,
    pub size: u64,
}

contract! {
    /// 文本文件。
    pub trait Files as FILES = service("files", 1) {
        /// 文件不存在是 `None`。路径不合法是 `validation`。
        async fn read(&self, path: String) -> Result<Option<String>, AppError>;

        /// 整个写入：没有的目录会建好；写到一半崩溃不会留下半个文件。
        async fn write(&self, path: String, content: String) -> Result<(), AppError>;

        /// `dir` 下（含子目录）名字以 `suffix` 结尾的文件，按路径排序。目录不存在时是空的。
        async fn list(&self, dir: String, suffix: String) -> Result<Vec<FileInfo>, AppError>;

        /// 删掉一个文件。本来就不存在返回 `false`。
        async fn remove(&self, path: String) -> Result<bool, AppError>;
    }
}

/// 检查一个相对路径，返回它的各级名字。实现都该用它，保证同样的路径同样地被接受或拒绝。
pub fn path_parts(path: &str) -> Result<Vec<&str>, AppError> {
    if path.starts_with('/') || path.contains('\\') {
        return Err(AppError::validation(format!(
            "`{path}` 要是根目录下的相对路径"
        )));
    }
    let parts: Vec<&str> = path
        .split('/')
        .filter(|p| !p.is_empty() && *p != ".")
        .collect();
    if parts.contains(&"..") {
        return Err(AppError::validation(format!("`{path}` 不能有 `..`")));
    }
    Ok(parts)
}

#[cfg(feature = "testing")]
pub mod testing {
    //! 契约测试，和一个放在内存里的假实现。

    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex, PoisonError};

    use kernel_api::{
        AppError, Component, ConfigSection, ErrorKind, Host, Kind, Manifest, async_trait,
    };

    use super::{FILES, FileInfo, Files, path_parts};

    /// 契约测试。`files` 开始时是空的。
    pub async fn files_contract(files: Arc<dyn Files>) -> Result<(), AppError> {
        assert_eq!(files.read("daily/2026-11-12.md".to_owned()).await?, None);
        assert!(
            files
                .list("daily".to_owned(), ".md".to_owned())
                .await?
                .is_empty(),
            "目录不存在是空的"
        );

        files
            .write(
                "daily/2026-11-12.md".to_owned(),
                "# 周四\n气象站".to_owned(),
            )
            .await?;
        files
            .write("daily/2026-11-11.md".to_owned(), "旧的".to_owned())
            .await?;
        files
            .write("daily/sub/x.md".to_owned(), "x".to_owned())
            .await?;
        files
            .write("daily/notes.txt".to_owned(), "不是 md".to_owned())
            .await?;
        assert_eq!(
            files
                .read("daily/2026-11-12.md".to_owned())
                .await?
                .as_deref(),
            Some("# 周四\n气象站")
        );
        assert_eq!(
            files
                .read("./daily//2026-11-12.md".to_owned())
                .await?
                .as_deref(),
            Some("# 周四\n气象站"),
            "多余的 / 和 . 不影响"
        );

        files
            .write("daily/2026-11-12.md".to_owned(), "改过".to_owned())
            .await?;
        assert_eq!(
            files
                .read("daily/2026-11-12.md".to_owned())
                .await?
                .as_deref(),
            Some("改过"),
            "整个覆盖"
        );

        let listed: Vec<(String, u64)> = files
            .list("daily".to_owned(), ".md".to_owned())
            .await?
            .into_iter()
            .map(|f| (f.path, f.size))
            .collect();
        assert_eq!(
            listed,
            [
                ("daily/2026-11-11.md".to_owned(), 6),
                ("daily/2026-11-12.md".to_owned(), 6),
                ("daily/sub/x.md".to_owned(), 1),
            ],
            "含子目录，按路径排序，按后缀筛"
        );

        assert!(files.remove("daily/2026-11-11.md".to_owned()).await?);
        assert!(!files.remove("daily/2026-11-11.md".to_owned()).await?);
        assert_eq!(files.read("daily/2026-11-11.md".to_owned()).await?, None);

        for bad in ["/etc/passwd", "../outside.md", "daily/../../x"] {
            let read = files.read(bad.to_owned()).await.map(|_| ());
            let write = files.write(bad.to_owned(), String::new()).await;
            for result in [read, write] {
                assert!(
                    matches!(&result, Err(e) if e.kind == ErrorKind::Validation),
                    "{bad}: {result:?}"
                );
            }
        }
        Ok(())
    }

    /// 放在内存里的文件。克隆出来的是同一份。
    #[derive(Clone, Default)]
    pub struct MemoryFiles(Arc<Mutex<BTreeMap<String, (String, i64)>>>);

    fn normal(path: &str) -> Result<String, AppError> {
        Ok(path_parts(path)?.join("/"))
    }

    impl MemoryFiles {
        /// 直接放一个文件，假装是用户写的（修改时刻给定）。
        pub fn put(&self, path: &str, content: &str, modified_ms: i64) {
            if let Ok(path) = normal(path) {
                self.0
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(path, (content.to_owned(), modified_ms));
            }
        }

        /// 现在的内容。
        pub fn get(&self, path: &str) -> Option<String> {
            let path = normal(path).ok()?;
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&path)
                .map(|(c, _)| c.clone())
        }
    }

    #[async_trait]
    impl Files for MemoryFiles {
        async fn read(&self, path: String) -> Result<Option<String>, AppError> {
            let path = normal(&path)?;
            Ok(self
                .0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&path)
                .map(|(c, _)| c.clone()))
        }

        async fn write(&self, path: String, content: String) -> Result<(), AppError> {
            let path = normal(&path)?;
            let mut all = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            let next = all.values().map(|(_, m)| *m).max().unwrap_or(0) + 1;
            all.insert(path, (content, next));
            Ok(())
        }

        async fn list(&self, dir: String, suffix: String) -> Result<Vec<FileInfo>, AppError> {
            let prefix = normal(&dir)?;
            let all = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            Ok(all
                .iter()
                .filter(|(p, _)| prefix.is_empty() || p.starts_with(&format!("{prefix}/")))
                .filter(|(p, _)| p.ends_with(&suffix))
                .map(|(p, (c, m))| FileInfo {
                    path: p.clone(),
                    modified_ms: *m,
                    size: c.len() as u64,
                })
                .collect())
        }

        async fn remove(&self, path: String) -> Result<bool, AppError> {
            let path = normal(&path)?;
            Ok(self
                .0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&path)
                .is_some())
        }
    }

    /// 组件 `files-memory`。
    impl Component for MemoryFiles {
        fn manifest(&self) -> Manifest {
            Manifest {
                id: "files-memory",
                kind: Kind::Service,
                summary: "测试用：放在内存里的文件",
                provides: vec![FILES.id()],
                requires: vec![],
                optional_requires: vec![],
            }
        }

        fn start(&self, _config: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
            host.provide(&FILES, Arc::new(self.clone()))
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[tokio::test]
        async fn memory_files_pass_the_contract() {
            files_contract(Arc::new(MemoryFiles::default()))
                .await
                .unwrap();
        }
    }
}
