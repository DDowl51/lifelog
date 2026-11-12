# 参与开发

## 准备

```bash
git clone https://github.com/DDowl51/lifelog lifelog && cd lifelog
lefthook install          # 可选：提交前自动跑全量检查
scripts/check.sh          # 格式、clippy、架构规则、全部测试
```

需要 stable Rust（edition 2024）。端到端测试会起真实的 `lifelogd` 和 `lifelog` 进程，对面是一个假的 niri，
不碰你正在运行的 `lifelogd` 和真实数据库。需要真的 Wayland 会话、真的模型的测试默认跳过（`#[ignore]`）。

## 先读这几样

- [架构](docs/architecture.md)：分层、规则、怎么加组件。
- [设计文档](docs/specs/2026-11-12-lifelog-design.md)：为什么这样设计；§11「实现中的调整」记下了和最初设计不一样的地方。
- [进度](docs/plans/PROGRESS.md)：做到哪了、还有什么待定。

## 改动放在哪

| 想做的事 | 放在哪 |
|---|---|
| 新的数据来源、新的统计 | 新的应用功能 `features/<id>/` |
| 接一个新的外部系统（窗口管理器、浏览器、模型接口） | 新的系统服务 `services/<id>/`，提供已有的契约 |
| 系统服务需要新的通用能力 | 改 `contracts/<名字>/` 里的契约，所有实现跟着改，契约测试先写 |
| 新的命令、页面、MCP 工具 | 在对应的功能里登记到 `CliCommands`、`TuiPages`、`McpTools` |
| 改 kernel | 几乎不需要。先确认缺的不是一个系统服务或扩展点 |

**系统服务里不能出现业务名词**（活动、项目、摘要……），`tests/arch` 会检查依赖方向。

## 每个改动都要

- **测试**：纯逻辑放在 `core`，单元测试；接线放在 `component`，用 `testkit` 装上真实或假的依赖测；替换实现要通过契约测试。
- **配置项**：新加或改了配置键，同时改 `config_defaults()` 和 [docs/configuration.md](docs/configuration.md)，`tests/config-doc` 会检查两边一致。
- **出错尽早**：配置写错在启动时报错；`Config` 加 `#[serde(deny_unknown_fields)]`；没有配置项的组件用 `kernel_api::NoConfig`。
- **用户能看到的文字**用简体中文；标识符用英文；注释写「为什么」。
- **跑一遍** `scripts/check.sh`，全部通过再提交。

## 提交

[Conventional Commits](https://www.conventionalcommits.org/zh-hans/)，scope 用组件 id：

```
feat(privacy): 命令屏蔽可配置
fix(shell-atuin): 按真实 atuin 库修正
docs: 配置文档
```

一个提交做一件事。行为有变化的，在 [CHANGELOG.md](CHANGELOG.md) 的「未发布」里加一行。

## 许可证

除非你另外声明，你提交的贡献按 [MIT](LICENSE-MIT) 或 [Apache-2.0](LICENSE-APACHE) 双许可证发布。

## 隐私

这个项目处理的是个人活动记录。提 issue、写测试、贴日志时：

- 测试数据用编出来的，不要用自己的真实记录；
- 贴 `--trace` 输出和数据库内容之前看一遍，去掉窗口标题、命令、网址里的私人信息；
- 永远不要贴 API key。
