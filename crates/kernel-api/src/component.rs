//! 组件：唯一的构造单位。

use std::sync::Arc;

use crate::{AppError, ConfigSection, ContractId, Host};

/// 组件的角色。依赖只能从应用功能指向系统服务。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// 系统服务：与业务无关的能力。
    Service,
    /// 应用功能：业务。
    Feature,
}

/// 组件的自我描述。纯数据，kernel 靠它排序和校验，不用先启动组件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// 也是配置段名：`[components.<id>]`。
    pub id: &'static str,
    pub kind: Kind,
    /// 一句话说清做什么。
    pub summary: &'static str,
    /// 提供的契约。扩展点的所有者也把扩展点列在这里。
    pub provides: Vec<ContractId>,
    /// 必须有提供者的契约。
    pub requires: Vec<ContractId>,
    /// 有就先启动它；没有不报错，使用方自己降级。
    pub optional_requires: Vec<ContractId>,
}

/// 一个组件。
pub trait Component: Send + Sync + 'static {
    fn manifest(&self) -> Manifest;

    /// 唯一入口。只做登记：解析配置、`provide`、`resolve`、`contribute`、`subscribe`、
    /// `on_ready`、`spawn`、`on_stop`。**不在这里干活**：别的组件可能还没启动完。
    fn start(&self, config: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError>;

    /// 全部配置项和默认值，通常是 `ConfigSection::defaults::<Config>()`。
    /// 配置文档和它对得上由测试检查；`None` 表示没有声明。
    fn config_defaults(&self) -> Option<ConfigSection> {
        None
    }
}

/// 组件的构造函数。组件包用 [`register_component!`](crate::register_component) 登记，
/// 启动器用 [`registered_components`] 收集，所以不需要手写组件列表。
pub struct ComponentFactory(pub fn() -> Arc<dyn Component>);

inventory::collect!(ComponentFactory);

/// 登记一个组件。参数是构造它的表达式。
#[macro_export]
macro_rules! register_component {
    ($make:expr) => {
        $crate::inventory::submit! {
            $crate::ComponentFactory(|| ::std::sync::Arc::new($make))
        }
    };
}

/// 链接进当前进程的全部组件，按 id 排序（收集顺序本身不确定）。
pub fn registered_components() -> Vec<Arc<dyn Component>> {
    let mut all: Vec<Arc<dyn Component>> = inventory::iter::<ComponentFactory>
        .into_iter()
        .map(|f| (f.0)())
        .collect();
    all.sort_by_key(|c| c.manifest().id);
    all
}
