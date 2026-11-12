//! 启动器（`apps/*`）的构建脚本用。
//!
//! 组件包在 `Cargo.toml` 里写 `[package.metadata.component] id = "…"`。这里据此生成：
//!
//! - 每个被这个启动器依赖的组件包一行 `use 包名 as _;`。只在 `Cargo.toml` 里写依赖而不引用，
//!   链接器会把整个包丢掉，组件就登记不上；
//! - `LINKED_COMPONENTS`：这个启动器链接了哪些组件；
//! - `KNOWN_COMPONENTS`：仓库里有哪些组件。读配置时用它区分「别的进程的组件」和「拼写错误」。
//!
//! 所以加一个组件 = 加一个包、在要用它的启动器的依赖里加一行，不用改任何列表。

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// 一个组件包。
#[derive(Debug, Clone, PartialEq, Eq)]
struct ComponentPackage {
    package: String,
    id: String,
    manifest: PathBuf,
}

fn read_toml(path: &Path) -> Result<toml::Table, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("读不了 {}：{e}", path.display()))?;
    text.parse()
        .map_err(|e| format!("{} 不是合法的 TOML：{e}", path.display()))
}

fn workspace_root(start: &Path) -> Result<PathBuf, String> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        let manifest = d.join("Cargo.toml");
        if manifest.is_file() && read_toml(&manifest)?.contains_key("workspace") {
            return Ok(d.to_owned());
        }
        dir = d.parent();
    }
    Err(format!(
        "从 {} 往上找不到 workspace 的 Cargo.toml",
        start.display()
    ))
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

/// 仓库里所有声明了 `[package.metadata.component]` 的包。
fn scan(root: &Path) -> Result<Vec<ComponentPackage>, String> {
    let mut dirs = subdirs(&root.join("services"));
    dirs.extend(
        subdirs(&root.join("features"))
            .into_iter()
            .map(|d| d.join("component")),
    );

    let mut found = Vec::new();
    for dir in dirs {
        let manifest = dir.join("Cargo.toml");
        if !manifest.is_file() {
            continue;
        }
        let doc = read_toml(&manifest)?;
        let package = doc.get("package").and_then(|p| p.as_table());
        let name = package.and_then(|p| p.get("name")).and_then(|n| n.as_str());
        let id = package
            .and_then(|p| p.get("metadata"))
            .and_then(|m| m.get("component"))
            .and_then(|c| c.get("id"))
            .and_then(|i| i.as_str());
        match (name, id) {
            (Some(name), Some(id)) => {
                found.push(ComponentPackage {
                    package: name.to_owned(),
                    id: id.to_owned(),
                    manifest,
                });
            }
            _ => {
                return Err(format!(
                    "{} 是组件包，但没有写 [package.metadata.component] id = \"…\"",
                    manifest.display()
                ));
            }
        }
    }
    found.sort_by(|a, b| a.id.cmp(&b.id));
    for pair in found.windows(2) {
        if pair[0].id == pair[1].id {
            return Err(format!(
                "{} 和 {} 的组件 id 都是 `{}`",
                pair[0].manifest.display(),
                pair[1].manifest.display(),
                pair[0].id
            ));
        }
    }
    Ok(found)
}

fn list(name: &str, ids: &[&str]) -> String {
    let items: Vec<String> = ids.iter().map(|id| format!("{id:?}")).collect();
    format!("pub const {name}: &[&str] = &[{}];\n", items.join(", "))
}

/// 生成的代码，以及构建脚本应该盯着的文件。
fn render(app_manifest: &Path) -> Result<(String, Vec<PathBuf>), String> {
    let app_dir = app_manifest
        .parent()
        .ok_or("启动器的 Cargo.toml 没有上级目录")?;
    let root = workspace_root(app_dir)?;
    let all = scan(&root)?;

    let app = read_toml(app_manifest)?;
    let deps = app.get("dependencies").and_then(|d| d.as_table());
    let linked: Vec<&ComponentPackage> = all
        .iter()
        .filter(|c| deps.is_some_and(|d| d.contains_key(&c.package)))
        .collect();

    let mut code = String::from("// 由 boot-build 生成，不要手改。\n\n");
    for component in &linked {
        let _ = writeln!(code, "use {} as _;", component.package.replace('-', "_"));
    }
    code.push('\n');
    code.push_str(&list(
        "LINKED_COMPONENTS",
        &linked.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
    ));
    code.push_str(&list(
        "KNOWN_COMPONENTS",
        &all.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
    ));

    let mut watch: Vec<PathBuf> = all.into_iter().map(|c| c.manifest).collect();
    watch.push(app_manifest.to_owned());
    // 新增组件包时目录会变
    watch.push(root.join("services"));
    watch.push(root.join("features"));
    Ok((code, watch))
}

/// 在启动器的 `build.rs` 里调用。生成 `$OUT_DIR/components.rs`，启动器用 `include!` 引进来。
pub fn generate() -> Result<(), String> {
    let var = |name: &str| std::env::var(name).map_err(|_| format!("环境变量 {name} 没有设置"));
    let manifest = PathBuf::from(var("CARGO_MANIFEST_DIR")?).join("Cargo.toml");
    let out = PathBuf::from(var("OUT_DIR")?).join("components.rs");

    let (code, watch) = render(&manifest)?;
    std::fs::write(&out, code).map_err(|e| format!("写不了 {}：{e}", out.display()))?;

    let mut stdout = std::io::stdout().lock();
    for path in watch {
        let _ = writeln!(stdout, "cargo::rerun-if-changed={}", path.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, text: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn component(name: &str, id: &str) -> String {
        format!("[package]\nname = \"{name}\"\n\n[package.metadata.component]\nid = \"{id}\"\n")
    }

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "Cargo.toml", "[workspace]\nmembers = []\n");
        write(
            root,
            "services/clock-system/Cargo.toml",
            &component("clock-system", "clock-system"),
        );
        write(
            root,
            "services/wm-niri/Cargo.toml",
            &component("wm-niri", "wm-niri"),
        );
        write(
            root,
            "features/focus/component/Cargo.toml",
            &component("focus", "focus"),
        );
        // 功能的契约包和纯逻辑包不是组件
        write(
            root,
            "features/focus/core/Cargo.toml",
            "[package]\nname = \"focus-core\"\n",
        );
        write(
            root,
            "apps/cli/Cargo.toml",
            "[package]\nname = \"cli\"\n\n[dependencies]\nboot = {}\nclock-system = {}\nfocus = {}\n",
        );
        dir
    }

    #[test]
    fn links_what_the_app_depends_on_and_knows_everything() {
        let dir = repo();
        let (code, watch) = render(&dir.path().join("apps/cli/Cargo.toml")).unwrap();
        let expected = "\
// 由 boot-build 生成，不要手改。

use clock_system as _;
use focus as _;

pub const LINKED_COMPONENTS: &[&str] = &[\"clock-system\", \"focus\"];
pub const KNOWN_COMPONENTS: &[&str] = &[\"clock-system\", \"focus\", \"wm-niri\"];
";
        assert_eq!(code, expected);
        assert!(
            watch
                .iter()
                .any(|p| p.ends_with("services/wm-niri/Cargo.toml"))
        );
        assert!(watch.iter().any(|p| p.ends_with("features")));
    }

    #[test]
    fn a_component_package_without_metadata_is_an_error() {
        let dir = repo();
        write(
            dir.path(),
            "services/oops/Cargo.toml",
            "[package]\nname = \"oops\"\n",
        );
        let err = render(&dir.path().join("apps/cli/Cargo.toml")).unwrap_err();
        assert!(
            err.contains("services/oops/Cargo.toml") && err.contains("metadata.component"),
            "{err}"
        );
    }

    #[test]
    fn duplicate_ids_are_an_error() {
        let dir = repo();
        write(
            dir.path(),
            "services/clock-two/Cargo.toml",
            &component("clock-two", "clock-system"),
        );
        let err = render(&dir.path().join("apps/cli/Cargo.toml")).unwrap_err();
        assert!(err.contains("`clock-system`"), "{err}");
    }
}
