//! 对真实的 workspace 跑架构检查。

// 集成测试文件里的辅助函数不算「测试函数」，clippy 的测试豁免管不到，这里整体放开。
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::Command;

use arch::{Dep, DepKind, Pkg, Role, check, classify};
use serde::Deserialize;

#[derive(Deserialize)]
struct Metadata {
    packages: Vec<MetaPackage>,
    workspace_root: PathBuf,
}

#[derive(Deserialize)]
struct MetaPackage {
    name: String,
    manifest_path: PathBuf,
    dependencies: Vec<MetaDep>,
}

#[derive(Deserialize)]
struct MetaDep {
    name: String,
    kind: Option<String>,
    optional: bool,
    path: Option<PathBuf>,
}

/// 包相对于它所在仓库根目录的路径。
///
/// 本仓库的包相对于 workspace 根；path 依赖进来的共享仓库（mk-kit）里的包，
/// 相对于离它最近的、`Cargo.toml` 里有 `[workspace]` 的上级目录。这样共享仓库里的包也按目录得到角色。
fn rel_dir(root: &Path, dir: &Path) -> String {
    let base = if dir.starts_with(root) {
        root.to_path_buf()
    } else {
        dir.ancestors()
            .skip(1)
            .find(|a| {
                std::fs::read_to_string(a.join("Cargo.toml"))
                    .is_ok_and(|t| t.lines().any(|l| l.trim() == "[workspace]"))
            })
            .map_or_else(|| dir.to_path_buf(), Path::to_path_buf)
    };
    dir.strip_prefix(&base)
        .unwrap_or(dir)
        .to_string_lossy()
        .replace('\\', "/")
}

fn load() -> Vec<Pkg> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let out = Command::new(cargo)
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("运行 cargo metadata");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let meta: Metadata = serde_json::from_slice(&out.stdout).expect("解析 cargo metadata");

    let role_of = |dir: &Path| -> Role {
        let rel = rel_dir(&meta.workspace_root, dir);
        classify(&rel).unwrap_or_else(|| {
            panic!("包所在的目录 `{rel}` 不属于任何角色。把它放到 crates/ contracts/ services/ features/<id>/{{contracts,core,component}} apps/ 之一")
        })
    };

    meta.packages
        .iter()
        .map(|p| {
            let dir = p.manifest_path.parent().expect("manifest 有父目录");
            let deps = p
                .dependencies
                .iter()
                .map(|d| Dep {
                    name: d.name.clone(),
                    kind: match d.kind.as_deref() {
                        Some("dev") => DepKind::Dev,
                        Some("build") => DepKind::Build,
                        _ => DepKind::Normal,
                    },
                    optional: d.optional,
                    internal: d.path.as_deref().map(role_of),
                })
                .collect();
            Pkg {
                name: p.name.clone(),
                role: role_of(dir),
                deps,
            }
        })
        .collect()
}

#[test]
fn workspace_obeys_architecture_rules() {
    let pkgs = load();
    assert!(!pkgs.is_empty());
    let violations = check(&pkgs);
    let text: Vec<String> = violations.iter().map(ToString::to_string).collect();
    assert!(violations.is_empty(), "违反架构规则：\n{}", text.join("\n"));
}
