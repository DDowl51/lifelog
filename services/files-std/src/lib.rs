//! 系统服务 `files-std`：用本机的一个目录提供 `Files`。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use files_contract::{FILES, FileInfo, Files, path_parts};
use kernel_api::{
    AppError, Component, ConfigSection, Host, Kind, Manifest, async_trait, register_component,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
struct Config {
    /// 根目录。不存在时第一次写入会建出来。
    root: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            root: "~/lifelog".to_owned(),
        }
    }
}

fn expand(path: &str) -> Result<PathBuf, AppError> {
    match path.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join(rest))
            .ok_or_else(|| AppError::config(format!("`{path}` 用了 ~，但环境里没有 HOME"))),
        None if Path::new(path).is_absolute() => Ok(PathBuf::from(path)),
        None => Err(AppError::config(format!(
            "root `{path}` 要是绝对路径或以 ~ 开头"
        ))),
    }
}

fn io(path: &Path, e: &std::io::Error) -> AppError {
    AppError::unavailable(format!("{}：{e}", path.display()))
}

#[derive(Clone)]
struct Dir {
    root: PathBuf,
}

impl Dir {
    fn full(&self, path: &str) -> Result<PathBuf, AppError> {
        Ok(path_parts(path)?
            .iter()
            .fold(self.root.clone(), |p, part| p.join(part)))
    }

    fn walk(&self, dir: &Path, suffix: &str, out: &mut Vec<FileInfo>) -> Result<(), AppError> {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(io(dir, &e)),
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                self.walk(&path, suffix, out)?;
            } else if meta.is_file()
                && let Some(name) = path.to_str()
                && name.ends_with(suffix)
                && let Ok(relative) = path.strip_prefix(&self.root)
            {
                let modified_ms = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .and_then(|d| i64::try_from(d.as_millis()).ok())
                    .unwrap_or(0);
                out.push(FileInfo {
                    path: relative.to_string_lossy().into_owned(),
                    modified_ms,
                    size: meta.len(),
                });
            }
        }
        Ok(())
    }
}

/// 在阻塞线程上做文件操作。
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, AppError> + Send + 'static,
) -> Result<T, AppError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| AppError::unavailable(format!("读写文件的线程崩了：{e}")))?
}

#[async_trait]
impl Files for Dir {
    async fn read(&self, path: String) -> Result<Option<String>, AppError> {
        let full = self.full(&path)?;
        blocking(move || match std::fs::read_to_string(&full) {
            Ok(text) => Ok(Some(text)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io(&full, &e)),
        })
        .await
    }

    async fn write(&self, path: String, content: String) -> Result<(), AppError> {
        let full = self.full(&path)?;
        blocking(move || {
            if let Some(dir) = full.parent() {
                std::fs::create_dir_all(dir).map_err(|e| io(dir, &e))?;
            }
            // 先写旁边的临时文件再改名：改名是原子的
            let temp = full.with_extension(format!(
                "{}.tmp-{}",
                full.extension().and_then(|e| e.to_str()).unwrap_or(""),
                std::process::id()
            ));
            std::fs::write(&temp, content).map_err(|e| io(&temp, &e))?;
            std::fs::rename(&temp, &full).map_err(|e| io(&full, &e))
        })
        .await
    }

    async fn list(&self, dir: String, suffix: String) -> Result<Vec<FileInfo>, AppError> {
        let full = self.full(&dir)?;
        let me = self.clone();
        blocking(move || {
            let mut out = Vec::new();
            me.walk(&full, &suffix, &mut out)?;
            out.sort_by(|a, b| a.path.cmp(&b.path));
            Ok(out)
        })
        .await
    }

    async fn remove(&self, path: String) -> Result<bool, AppError> {
        let full = self.full(&path)?;
        blocking(move || match std::fs::remove_file(&full) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(io(&full, &e)),
        })
        .await
    }
}

pub struct FilesStd;

impl Component for FilesStd {
    fn config_defaults(&self) -> Option<ConfigSection> {
        ConfigSection::defaults::<Config>()
    }

    fn manifest(&self) -> Manifest {
        Manifest {
            id: "files-std",
            kind: Kind::Service,
            summary: "用本机的一个目录读写文本文件",
            provides: vec![FILES.id()],
            requires: vec![],
            optional_requires: vec![],
        }
    }

    fn start(&self, config: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
        let config: Config = config.parse()?;
        let dir = Dir {
            root: expand(&config.root)?,
        };
        host.provide(&FILES, Arc::new(dir))
    }
}

register_component!(FilesStd);
