//! 应用功能 `privacy`：入库之前排除和脱敏。
//!
//! 只做接线：把配置编译成规则（`privacy-core`），登记到 `activity` 的处理器扩展点。
//! 规则是策略，全部来自配置；内置规则可以整体关掉。

use std::sync::Arc;

use activity_contracts::{ACTIVITY_PROCESSORS, Activity, ActivityProcessor};
use kernel_api::{
    AppError, Component, ConfigSection, Host, Kind, Manifest, async_trait, register_component,
};
use privacy_core::{
    DEFAULT_COMMANDS, ExcludeSpec, Rules, builtin_excludes, builtin_secrets, command_exclude,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
struct Config {
    /// 是否带上内置规则（密码管理器、浏览器隐私窗口、常见的密钥格式）。
    builtin_rules: bool,
    /// 额外的排除规则。
    exclude: Vec<ExcludeSpec>,
    /// 额外的密钥正则。有名为 `secret` 的分组时只换掉那一组。
    secrets: Vec<String>,
    /// 执行了这些命令时只记时刻，不记命令内容。默认是命令行的密码管理器；空列表 = 不屏蔽。
    exclude_commands: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            builtin_rules: true,
            exclude: Vec::new(),
            secrets: Vec::new(),
            exclude_commands: DEFAULT_COMMANDS.map(str::to_owned).to_vec(),
        }
    }
}

struct Privacy(Rules);

#[async_trait]
impl ActivityProcessor for Privacy {
    async fn name(&self) -> Result<String, AppError> {
        Ok("privacy".to_owned())
    }

    async fn process(&self, activity: Activity) -> Result<Option<Activity>, AppError> {
        Ok(Some(self.0.apply(activity)))
    }
}

pub struct PrivacyComponent;

impl Component for PrivacyComponent {
    fn config_defaults(&self) -> Option<ConfigSection> {
        ConfigSection::defaults::<Config>()
    }

    fn manifest(&self) -> Manifest {
        Manifest {
            id: "privacy",
            kind: Kind::Feature,
            summary: "入库之前排除和脱敏",
            provides: vec![],
            requires: vec![ACTIVITY_PROCESSORS.id()],
            optional_requires: vec![],
        }
    }

    fn start(&self, config: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
        let config: Config = config.parse()?;
        let (mut excludes, mut secrets) = if config.builtin_rules {
            (builtin_excludes(), builtin_secrets())
        } else {
            (Vec::new(), Vec::new())
        };
        excludes.extend(command_exclude(&config.exclude_commands).map_err(AppError::config)?);
        excludes.extend(config.exclude);
        secrets.extend(config.secrets);
        let rules = Rules::new(&excludes, &secrets).map_err(AppError::config)?;
        host.contribute(&ACTIVITY_PROCESSORS, Arc::new(Privacy(rules)))
    }
}

register_component!(PrivacyComponent);
