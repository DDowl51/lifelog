# 实现计划

对应设计：[2026-11-12-lifelog-design.md](../specs/2026-11-12-lifelog-design.md)。进度见 [PROGRESS.md](PROGRESS.md)。

每个阶段结束跑 `scripts/check.sh`（格式、clippy、架构规则、全部测试），把结果写进 PROGRESS.md 再进入下一阶段。

| 阶段 | 做什么 | 涉及的包 | 验收 |
|---|---|---|---|
| 0 | workspace、lint 配置、架构检查、检查脚本、提交钩子 | `tests/arch`、`scripts/` | 临时加一条违规依赖，`cargo test -p arch` 报错并写明规则和改法 |
| 1 | kernel 接口和实现、启动库、测试工具；时钟、日志 | `crates/*`、`contracts/{clock,log}`、`services/{clock-system,log-stderr}` | 假组件测试覆盖：排序、缺提供者、成环、双提供者、停用、未声明的契约、事件顺序、追踪记录、后台任务；`contract!` 拒绝非数据参数 |
| 2 | SQL 存储；活动入库和读取 | `contracts/sql`、`services/storage-sqlite`、`features/activity/*` | `Sql` 契约测试；迁移校验和被改动时拒绝启动；按去重键更新；时长统计（搬旧仓库的用例） |
| 3 | 窗口管理器、定时器；焦点时段 | `contracts/{window,timer}`、`services/{wm-niri,timer-tokio}`、`features/focus/*` | 回放旧仓库 `scratch/m1-4` 的录制，得到的时段和旧实现一致 |
| 4 | 命令行框架；查询；两个启动器；systemd 单元 | `contracts/cli`、`services/cli-clap`、`features/query/*`、`apps/{lifelogd,lifelog}` | 真机：`lifelogd` 跑一段时间后 `lifelog day` 有合理输出；`--trace` 能看到谁在调谁 |
| 5 | 离开检测；`lifelog week`、`lifelog timeline` | `contracts/idle`、`services/idle-wayland`、`features/afk/{core,component}`、`features/query/*` | 离开期间不计入时长 |
| 6 | 终端界面 | `contracts/tui`、`services/tui-ratatui`、`apps/lifelog-tui` | 在终端界面里按天翻看统计和时间线 |
| 7 | 隐私、项目归类；按项目统计 | `features/{privacy,project}/{core,component}`、`features/query/*`、`apps/lifelogd` | 端到端：原标题不进数据库（含 WAL），`--by project` 能分出项目；真机标题归类 |
| 8 | 导入 shell 命令 | `contracts/shell`、`services/shell-atuin`、`features/shell/{core,component}`、`features/query/*` | 端到端：读临时的 atuin 库，`lifelog commands` 查到命令，项目按目录归好、密钥已脱敏；atuin 不存在时常驻进程照常运行 |
| 9 | 导入浏览历史 | `contracts/browser`、`services/browser-{history,firefox,chromium,qutebrowser}`、`features/browsing/{core,component}`、`features/query/*` | 端到端：读临时的 qutebrowser 项目实例，`lifelog visits` 查到网页，按实例归到项目，排除的域名已脱敏；真机三个浏览器全量导入 |
| 10 | 按项目的一天：时间线挂命令和网页，`lifelog projects` | `features/query/*` | 时间线每段下有命令和网页；按项目有时长、命令数、网页数；真机数据上可读 |
| 11 | 全文检索 | `contracts/tokenizer`、`services/tokenizer-cjk`、`features/search/*`、`features/activity/*`（`since`、`get`）、`features/query/*`、`contracts/cli`（`Rest`） | 端到端：常驻进程建索引、命令行检索，脱敏前的文字不在索引里；真机：全量数据建索引的时间和检索的结果 |
| 12 | 给 agent 的接口 | `contracts/mcp`、`services/mcp-stdio`、`apps/lifelog-mcp`、`features/query/*`（工具）、`skills/lifelog` | 端到端：真实进程走一遍 MCP 握手和调用；真机：各工具结果的正确性和大小 |
| 13 | 升到复杂档（kernel 的远程代理）；每日摘要 | 见 [摘要设计](../specs/2026-11-12-summary-design.md) | 见摘要设计 §2 |
| 14 | 向量检索 | 同上 | 同上 |
| 15 | 笔记导入 | `features/notes/{core,component}` | 笔记和手改的摘要能被检索；改了、删了跟着变 |
| 之后 | 外部命令数据源；`lifelog reprocess`（用新规则重新归类旧记录） | | 每个阶段开始前补进本表 | → 文档和检索 → agent 接口和 MCP → 摘要（含升档）→ 外部命令数据源 | | 每个阶段开始前补进本表 |
