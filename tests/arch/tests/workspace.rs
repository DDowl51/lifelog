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

fn rel_dir(root: &Path, dir: &Path) -> String {
    dir.strip_prefix(root)
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
