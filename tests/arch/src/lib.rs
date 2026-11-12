//! 架构检查的规则。
//!
//! 包的角色由它在仓库里的目录决定，规则只看 `Cargo.toml` 里的依赖，
//! 所以违规的代码连编译都不用等，改完依赖就能查出来。
//! 规则编号对应设计文档 §7 和规范的 `enforcement.md`。

use std::fmt;

/// 一个包在架构里的角色。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Role {
    KernelApi,
    Kernel,
    Boot,
    Testkit,
    /// `contracts/<名>`：系统服务的契约。
    Contract,
    /// `services/<id>`：系统服务组件。
    Service,
    /// `features/<id>/contracts`
    FeatureContracts(String),
    /// `features/<id>/core`
    FeatureCore(String),
    /// `features/<id>/component`
    FeatureComponent(String),
    /// `apps/<名>`：启动器。
    App,
    /// `tests/*`：检查工具自己。
    Tool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepKind {
    Normal,
    Dev,
    Build,
}

#[derive(Debug, Clone)]
pub struct Dep {
    pub name: String,
    pub kind: DepKind,
    pub optional: bool,
    /// 依赖的是仓库里的包时，它的角色。
    pub internal: Option<Role>,
}

#[derive(Debug, Clone)]
pub struct Pkg {
    pub name: String,
    pub role: Role,
    pub deps: Vec<Dep>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub rule: &'static str,
    pub pkg: String,
    pub dep: String,
    pub why: String,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[{}] {} -> {}：{}",
            self.rule, self.pkg, self.dep, self.why
        )
    }
}

/// 会做 IO 的外部 crate。契约和纯逻辑里不能出现。
const IO_CRATES: &[&str] = &[
    "rusqlite",
    "clap",
    "reqwest",
    "ureq",
    "ratatui",
    "crossterm",
    "rmcp",
    "signal-hook",
    "ctrlc",
    "nix",
    "libc",
    "wayland-client",
    "fastembed",
];

/// 纯逻辑里额外不能出现的：运行时和组件登记。
const RUNTIME_CRATES: &[&str] = &["tokio", "tokio-util", "inventory"];

/// kernel 允许的外部依赖。
const KERNEL_EXTERNAL: &[&str] = &["tokio", "tokio-util"];

/// 按相对于仓库根目录的路径判断角色。认不出来的目录返回 `None`。
pub fn classify(rel_path: &str) -> Option<Role> {
    let parts: Vec<&str> = rel_path.trim_matches('/').split('/').collect();
    match parts.as_slice() {
        ["crates", "kernel-api"] => Some(Role::KernelApi),
        ["crates", "kernel"] => Some(Role::Kernel),
        ["crates", "boot"] => Some(Role::Boot),
        ["crates", "testkit"] => Some(Role::Testkit),
        // 构建脚本用的小工具，不进运行时
        ["crates", "boot-build"] => Some(Role::Tool),
        ["contracts", _] => Some(Role::Contract),
        ["services", _] => Some(Role::Service),
        ["features", id, "contracts"] => Some(Role::FeatureContracts((*id).to_owned())),
        ["features", id, "core"] => Some(Role::FeatureCore((*id).to_owned())),
        ["features", id, "component"] => Some(Role::FeatureComponent((*id).to_owned())),
        ["apps", _] => Some(Role::App),
        ["tests", _] => Some(Role::Tool),
        _ => None,
    }
}

/// 检查全部规则。开发依赖（测试用的）不受限制。
pub fn check(pkgs: &[Pkg]) -> Vec<Violation> {
    let mut out = Vec::new();
    for pkg in pkgs {
        for dep in pkg.deps.iter().filter(|d| d.kind != DepKind::Dev) {
            if let Some((rule, why)) = verdict(&pkg.role, dep) {
                out.push(Violation {
                    rule,
                    pkg: pkg.name.clone(),
                    dep: dep.name.clone(),
                    why: why.to_owned(),
                });
            }
        }
    }
    out
}

fn is_any_contract(role: &Role) -> bool {
    matches!(role, Role::Contract | Role::FeatureContracts(_))
}

fn is_feature(role: &Role) -> bool {
    matches!(
        role,
        Role::FeatureContracts(_) | Role::FeatureCore(_) | Role::FeatureComponent(_)
    )
}

fn is_plumbing(role: &Role) -> bool {
    matches!(role, Role::Kernel | Role::Boot | Role::Testkit)
}

/// 一条依赖违反了哪条规则。不违反返回 `None`。
fn verdict(role: &Role, dep: &Dep) -> Option<(&'static str, &'static str)> {
    let ext = dep.internal.is_none();
    let name = dep.name.as_str();
    match role {
        Role::Kernel => match &dep.internal {
            Some(Role::KernelApi) => None,
            Some(_) => Some((
                "R5",
                "kernel 在仓库里只能依赖 kernel-api，它不认识任何组件和契约",
            )),
            None if KERNEL_EXTERNAL.contains(&name) => None,
            None => Some((
                "R5",
                "kernel 的外部依赖只允许 tokio、tokio-util；别的能力应该是系统服务",
            )),
        },

        Role::KernelApi => match &dep.internal {
            Some(_) => Some(("R5", "kernel-api 不依赖仓库里的任何包")),
            None if IO_CRATES.contains(&name) => {
                Some(("R4", "kernel-api 只有定义，不能依赖做 IO 的 crate"))
            }
            None => None,
        },

        Role::Contract => match &dep.internal {
            Some(Role::KernelApi | Role::Contract) => None,
            Some(r) if is_feature(r) => Some((
                "R3",
                "系统服务的契约不能依赖应用功能；需要业务信息时声明扩展点",
            )),
            Some(_) => Some(("R4", "契约只能依赖 kernel-api 和别的契约")),
            None => contract_external(name, dep.optional),
        },

        Role::FeatureContracts(_) => match &dep.internal {
            Some(r) if matches!(r, Role::KernelApi) || is_any_contract(r) => None,
            Some(_) => Some(("R4", "契约只能依赖 kernel-api 和别的契约")),
            None => contract_external(name, dep.optional),
        },

        Role::Service => match &dep.internal {
            Some(Role::KernelApi | Role::Contract) => None,
            Some(r) if is_feature(r) => Some((
                "R3",
                "系统服务永远不依赖应用功能；需要业务信息时声明扩展点让功能来登记",
            )),
            Some(r) if is_plumbing(r) => Some((
                "R6",
                "组件不能依赖 kernel 的实现、启动库、testkit（testkit 只能是测试依赖）",
            )),
            Some(_) => Some(("R1", "系统服务只能依赖 kernel-api 和 contracts/* 里的契约")),
            None => None,
        },

        Role::FeatureCore(me) => match &dep.internal {
            Some(r) if matches!(r, Role::KernelApi) || is_any_contract(r) => None,
            Some(Role::FeatureCore(other)) if other == me => None,
            Some(r) if is_plumbing(r) => Some((
                "R6",
                "组件不能依赖 kernel 的实现、启动库、testkit（testkit 只能是测试依赖）",
            )),
            Some(_) => Some(("R8", "纯逻辑在仓库里只能依赖 kernel-api 和契约包")),
            None if IO_CRATES.contains(&name) || RUNTIME_CRATES.contains(&name) => Some((
                "R8",
                "纯逻辑不碰 IO 和运行时；把这部分挪到 component 包，或者改成调系统服务的契约",
            )),
            None => None,
        },

        Role::FeatureComponent(me) => match &dep.internal {
            Some(r) if matches!(r, Role::KernelApi) || is_any_contract(r) => None,
            Some(Role::FeatureCore(other)) if other == me => None,
            Some(Role::FeatureCore(_) | Role::FeatureComponent(_)) => Some((
                "R1",
                "别的功能只能通过它的 contracts 包来用，不能依赖它的实现",
            )),
            Some(r) if is_plumbing(r) => Some((
                "R6",
                "组件不能依赖 kernel 的实现、启动库、testkit（testkit 只能是测试依赖）",
            )),
            Some(_) => Some(("R1", "功能组件只能依赖 kernel-api、契约包和自己的 core")),
            None => None,
        },

        Role::Boot => match &dep.internal {
            Some(Role::KernelApi | Role::Kernel) => None,
            Some(_) => Some((
                "R6",
                "启动库不认识具体组件；组件由各个启动器（apps/*）链接进来",
            )),
            None => None,
        },

        Role::Testkit => match &dep.internal {
            Some(r) if is_feature(r) || matches!(r, Role::Service | Role::App) => {
                Some(("R6", "testkit 不认识具体组件；测试自己把要装的组件传进去"))
            }
            _ => None,
        },

        // 启动器是唯一知道有哪些具体组件的地方；检查工具不受限制。
        Role::App | Role::Tool => {
            let _ = ext;
            None
        }
    }
}

fn contract_external(name: &str, optional: bool) -> Option<(&'static str, &'static str)> {
    if IO_CRATES.contains(&name) {
        return Some(("R4", "契约里没有实现，不能依赖做 IO 的 crate"));
    }
    if RUNTIME_CRATES.contains(&name) && !optional {
        return Some((
            "R4",
            "契约本身不依赖运行时；假实现需要的话放在 testing feature 后面（optional）",
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn internal(name: &str, role: Role) -> Dep {
        Dep {
            name: name.to_owned(),
            kind: DepKind::Normal,
            optional: false,
            internal: Some(role),
        }
    }

    fn external(name: &str) -> Dep {
        Dep {
            name: name.to_owned(),
            kind: DepKind::Normal,
            optional: false,
            internal: None,
        }
    }

    fn rules(role: Role, dep: Dep) -> Vec<&'static str> {
        let pkg = Pkg {
            name: "p".to_owned(),
            role,
            deps: vec![dep],
        };
        check(&[pkg]).into_iter().map(|v| v.rule).collect()
    }

    fn feature(id: &str) -> String {
        id.to_owned()
    }

    #[test]
    fn classify_by_directory() {
        assert_eq!(classify("crates/kernel"), Some(Role::Kernel));
        assert_eq!(classify("contracts/clock"), Some(Role::Contract));
        assert_eq!(classify("services/wm-niri"), Some(Role::Service));
        assert_eq!(
            classify("features/focus/core"),
            Some(Role::FeatureCore(feature("focus")))
        );
        assert_eq!(classify("apps/lifelogd"), Some(Role::App));
        assert_eq!(classify("somewhere/else"), None);
    }

    #[test]
    fn r5_kernel_knows_nothing_but_kernel_api() {
        assert!(rules(Role::Kernel, internal("kernel-api", Role::KernelApi)).is_empty());
        assert!(rules(Role::Kernel, external("tokio")).is_empty());
        assert_eq!(
            rules(Role::Kernel, internal("clock-contract", Role::Contract)),
            ["R5"]
        );
        assert_eq!(
            rules(Role::Kernel, internal("storage-sqlite", Role::Service)),
            ["R5"]
        );
        assert_eq!(rules(Role::Kernel, external("rusqlite")), ["R5"]);
        assert_eq!(rules(Role::Kernel, external("toml")), ["R5"]);
    }

    #[test]
    fn r3_services_never_depend_on_features() {
        let dep = internal(
            "activity-contracts",
            Role::FeatureContracts(feature("activity")),
        );
        assert_eq!(rules(Role::Service, dep.clone()), ["R3"]);
        assert_eq!(rules(Role::Contract, dep), ["R3"]);
        assert!(rules(Role::Service, internal("sql-contract", Role::Contract)).is_empty());
        assert!(rules(Role::Service, external("rusqlite")).is_empty());
    }

    #[test]
    fn r1_components_only_see_contracts() {
        let me = Role::FeatureComponent(feature("focus"));
        assert!(
            rules(
                me.clone(),
                internal("focus-core", Role::FeatureCore(feature("focus")))
            )
            .is_empty()
        );
        assert!(
            rules(
                me.clone(),
                internal("a-contracts", Role::FeatureContracts(feature("activity")))
            )
            .is_empty()
        );
        assert_eq!(
            rules(
                me.clone(),
                internal("activity-core", Role::FeatureCore(feature("activity")))
            ),
            ["R1"]
        );
        assert_eq!(
            rules(
                me.clone(),
                internal("activity", Role::FeatureComponent(feature("activity")))
            ),
            ["R1"]
        );
        assert_eq!(rules(me, internal("storage-sqlite", Role::Service)), ["R1"]);
        assert_eq!(
            rules(Role::Service, internal("clock-system", Role::Service)),
            ["R1"]
        );
    }

    #[test]
    fn r6_components_do_not_see_kernel_or_boot() {
        let me = Role::FeatureComponent(feature("focus"));
        assert_eq!(rules(me.clone(), internal("kernel", Role::Kernel)), ["R6"]);
        assert_eq!(rules(me, internal("boot", Role::Boot)), ["R6"]);
        assert_eq!(
            rules(Role::Service, internal("testkit", Role::Testkit)),
            ["R6"]
        );
        assert_eq!(
            rules(Role::Boot, internal("storage-sqlite", Role::Service)),
            ["R6"]
        );
    }

    #[test]
    fn r4_contracts_have_no_io() {
        assert_eq!(rules(Role::Contract, external("rusqlite")), ["R4"]);
        assert_eq!(rules(Role::Contract, external("tokio")), ["R4"]);
        assert_eq!(
            rules(Role::FeatureContracts(feature("a")), external("clap")),
            ["R4"]
        );
        assert!(rules(Role::Contract, external("serde")).is_empty());
        let optional_tokio = Dep {
            name: "tokio".to_owned(),
            kind: DepKind::Normal,
            optional: true,
            internal: None,
        };
        assert!(rules(Role::Contract, optional_tokio).is_empty());
    }

    #[test]
    fn r8_core_is_pure() {
        let me = Role::FeatureCore(feature("focus"));
        assert_eq!(rules(me.clone(), external("rusqlite")), ["R8"]);
        assert_eq!(rules(me.clone(), external("tokio")), ["R8"]);
        assert_eq!(
            rules(me.clone(), internal("storage-sqlite", Role::Service)),
            ["R8"]
        );
        assert!(rules(me.clone(), external("jiff")).is_empty());
        assert!(rules(me, internal("clock-contract", Role::Contract)).is_empty());
    }

    #[test]
    fn dev_dependencies_are_exempt() {
        let dep = Dep {
            name: "testkit".to_owned(),
            kind: DepKind::Dev,
            optional: false,
            internal: Some(Role::Testkit),
        };
        assert!(rules(Role::Service, dep).is_empty());
    }

    #[test]
    fn message_names_rule_and_fix() {
        let pkg = Pkg {
            name: "wm-niri".to_owned(),
            role: Role::Service,
            deps: vec![internal(
                "focus-contracts",
                Role::FeatureContracts(feature("focus")),
            )],
        };
        let text = check(&[pkg])[0].to_string();
        assert!(
            text.starts_with("[R3] wm-niri -> focus-contracts："),
            "{text}"
        );
        assert!(text.contains("扩展点"), "{text}");
    }
}
