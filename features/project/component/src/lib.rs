//! 应用功能 `project`：把活动归到项目。
//!
//! 只做接线：把配置编译成规则（`project-core`），登记到 `activity` 的处理器扩展点。
//! 哪些目录算项目根、哪些标题算哪个项目，都是配置。

use std::sync::Arc;

use activity_contracts::{ACTIVITY_PROCESSORS, Activity, ActivityProcessor};
use kernel_api::{
    AppError, Component, ConfigSection, Host, Kind, Manifest, async_trait, register_component,
};
use project_core::{Projects, RuleSpec};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
struct Config {
    /// 项目根：这些目录下面的每个子目录是一个项目。例如 `["~/work/projects"]`。
    roots: Vec<String>,
    /// 归类规则，比路径先试。
    rules: Vec<RuleSpec>,
}

struct Project(Projects);

#[async_trait]
impl ActivityProcessor for Project {
    async fn name(&self) -> Result<String, AppError> {
        Ok("project".to_owned())
    }

    async fn process(&self, activity: Activity) -> Result<Option<Activity>, AppError> {
        Ok(Some(self.0.apply(activity)))
    }
}

pub struct ProjectComponent;

impl Component for ProjectComponent {
    fn config_defaults(&self) -> Option<ConfigSection> {
        ConfigSection::defaults::<Config>()
    }

    fn manifest(&self) -> Manifest {
        Manifest {
            id: "project",
            kind: Kind::Feature,
            summary: "把活动归到项目",
            provides: vec![],
            requires: vec![ACTIVITY_PROCESSORS.id()],
            optional_requires: vec![],
        }
    }

    fn start(&self, config: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
        let config: Config = config.parse()?;
        // 只有用到 ~ 时才需要 HOME
        let home = std::env::var("HOME").unwrap_or_default();
        if home.is_empty() && config.roots.iter().any(|r| r.starts_with('~')) {
            return Err(AppError::config("roots 里用了 ~，但环境里没有 HOME"));
        }
        let projects =
            Projects::new(&config.rules, &config.roots, &home).map_err(AppError::config)?;
        host.contribute(&ACTIVITY_PROCESSORS, Arc::new(Project(projects)))
    }
}

register_component!(ProjectComponent);
