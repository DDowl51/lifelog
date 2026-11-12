# lifelog 设计（微内核版）

- 日期：2026-11-12
- 状态：已确认（2026-11-12）
- 取代：更早一个实现的设计（2026-10-19，只作对照，不在这个仓库里）

## 1. 背景和目标

在本机自动记录电脑上的活动，统一存进一个 SQLite 文件。目标按优先级：

1. **首要：我自己能查阅每天做了什么。** 通过命令（`lifelog day`、`lifelog week`、`lifelog timeline`）和一个单独的终端界面程序（`lifelog-tui`）翻看：哪天在哪个应用、哪个项目上花了多久，按时间顺序做了什么。
2. 次要：让 Claude Code 等 agent 通过 `lifelog … --json` 和 MCP 回答「我之前做过 / 看过什么」。
3. 次要：本地 LLM 生成每日 / 每周摘要。

交付顺序按这个优先级排（§8）。旧设计把 agent 和摘要放在前面，这里调整了。

除优先级外功能和旧设计相同，旧设计 §2「已查证的技术事实」、§6 的表结构、§7 的查询接口、§8 摘要流程、§9 隐私规则继续有效，本文不重复，只写架构和有变化的地方。

第二个目标：这是第一个按 `microkernel-architecture` 规范做的项目。做的过程中规范哪里不好用，记进 `docs/plans/PROGRESS.md` 的「对规范的反馈」，告一段落时回写到 skill。

**环境**：Arch + niri 26.04（Wayland）、Ghostty、fish；Rust 1.99、edition 2024；数据是中 / 日 / 英 / 代码混合。

### 不做的事

- 录屏、录音、OCR、多设备同步、图形界面（终端界面要做）。
- 不迁移旧仓库的提交历史。旧仓库里已经写好并有测试的纯逻辑（焦点状态机、标题规范化、逻辑日计算、活动校验、SQL）搬过来改造，不重新发明。
- 不做进程内的定时调度器（见 §3.8）。
- 不做动态库插件。跨语言扩展走外部命令。

## 2. 档位和配套

| | 选择 | 理由 |
|---|---|---|
| 档位 | **中等** | 有两个入口（`lifelogd`、`lifelog`）；窗口管理器、向量化、LLM 都需要可换的实现；要长期用、持续加数据源 |
| 配套 | 数据应用 | SQLite |
| | 命令行 | `lifelog` 是人和 agent 的主要接口 |
| | 常驻进程 / 桌面工具 | `lifelogd` 常驻，要和 niri、Wayland 空闲协议、浏览器、atuin 打交道 |
| | 进程外扩展（轻量部分） | `xxx_cmd`、外部命令数据源 |

**不用的做法**：事务 outbox。lifelog 里的发布 / 订阅只有两类：窗口和空闲的状态变化（丢一条可以接受，下一条会纠正），以及「有新活动入库」（索引是按「还没处理的」拉取的，本身幂等）。进程内投递够用。

**预计什么时候升到复杂档**：`lifelog summarize`、`lifelog reindex` 这类写命令要交给 `lifelogd` 执行时（§3.8）。届时命令行进程里需要一个「同名契约的远程代理」，这正是复杂档的机制。那一步单独写设计文档。

**用到的例外**：无。终端界面的页面内容是纯数据（表格、条形图、列表、文本块），由功能登记、外壳绘制，所以不需要前端配套的「界面插槽」例外；也没有共享数据面。

## 3. 架构

### 3.1 技术选型

| 方面 | 选型 |
|---|---|
| 语言 | Rust，edition 2024，cargo workspace |
| 并发 | **tokio**。契约的方法都是 `async fn`（用 `async-trait` 做成可以放进 trait 对象的形式）；阻塞的库（rusqlite）在组件内部用 `spawn_blocking` |
| 序列化 | serde、serde_json、toml |
| 时间 | jiff |
| 错误 | thiserror；`AppError` 种类用规范里的固定集合 |
| 组件收集 | inventory |
| 测试 | cargo test；契约测试放在契约包的 `testing` feature 后面 |
| 规范 | rustfmt、clippy（禁止直接打印、`unwrap`）、架构检查测试、lefthook |

### 3.2 仓库结构

```
apps/
  lifelogd/            启动器：常驻进程
  lifelog/             启动器：命令行
  lifelog-tui/         启动器：终端界面（阶段 6）
crates/
  kernel-api/          组件能看到的全部 kernel 接口。只有定义
  kernel/              生命周期、命名、消息传递、监听点
  boot/                读配置、收集组件、信号、把启动错误打印出来
  testkit/             boot_test(组件…)：假时钟、手动定时器、内存数据库、手动投递
contracts/             系统服务的契约，一个一包：trait + 数据类型 + 契约测试 + 假实现（testing feature）
  clock/  log/  sql/  timer/  window/  cli/  …
services/              系统服务组件，一个一包
  clock-system/  log-stderr/  storage-sqlite/  timer-thread/  wm-niri/  cli-clap/  …
features/              应用功能组件，一个三包
  activity/{contracts, core, component}
  focus/{contracts, core, component}
  query/{contracts, core, component}
  …
tests/arch/            架构检查（读 cargo metadata）
docs/
```

每个应用功能三个 crate：`contracts`（对外）、`core`（领域和用例，`Cargo.toml` 里没有任何 IO 依赖）、`component`（适配和组件定义）。这是规范 Rust 落地页的建议，第一次实际用，嫌重的话记进反馈。

### 3.3 组件清单（按最终完整范围）

**系统服务**

| 契约 | 一句话职责 | 第一个实现 | 假实现 | 做的阶段 |
|---|---|---|---|---|
| `Clock` | 现在几点、本地时区偏移 | `clock-system` | 可拨动的假时钟 | 1 |
| `Log` | 写一条日志 | `log-stderr` | 收集到内存 | 1 |
| `Sql` | 执行 SQL；扩展点 `Migrations` | `storage-sqlite` | 同一实现开内存库 | 2 |
| `Timer` | 按周期触发；事件 `TIMER_FIRED` | `timer-tokio` | 手动触发 | 3 |
| `Windows` | 报告焦点窗口；事件 `FOCUS_CHANGED` | `wm-niri` | 回放录制文件 | 3 |
| `Cli` | 解析命令行、分派、输出、退出码；扩展点 `CliCommands` | `cli-clap` | — | 4 |
| `Idle` | 报告用户离开 / 回来；事件 `IDLE_CHANGED` | `idle-wayland` | 手动触发 | 5 |
| `Tui` | 终端界面的外壳：页面切换、按键、绘制；扩展点 `TuiPages` | `tui-ratatui` | — | 6 |
| `Files` | 读写目录里的文本文件 | `files-std` | 内存 | 之后 |
| `ShellHistory` | 读 shell 历史（增量） | `shell-atuin` | 内存 | 8 |
| `BrowserHistory` + 扩展点 `BrowserAdapters` | 读浏览历史（增量）；各浏览器的适配登记到扩展点 | `browser-history`（汇总）+ `browser-firefox`、`browser-chromium`、`browser-qutebrowser` | 内存 | 9 |
| `Subprocess` | 运行外部命令，按行读输出 | `subprocess-std` | 脚本化输出 | 之后 |
| `Tokenizer` | 把文本切成可索引的词 | `tokenizer-cjk` | — | 11 |
| `Embedder` | 文本变向量 | `embed-fastembed` | 固定向量 | 之后 |
| `Llm` | 文本补全 | `llm-openai-compat` | 固定回答 | 之后 |
| `Control` | 常驻进程的本地 socket 接口；扩展点 `ControlCommands` | `control-socket` | — | 之后 |
| `Mcp` | MCP stdio 服务；扩展点 `McpTools` | `mcp-stdio` | 不碰标准输入输出的假服务 | 12 |

契约里不出现 niri、atuin、Firefox 这些名字，它们只出现在实现组件的 id 和内部。

**应用功能**

| 组件 | 一句话职责 | 提供 | 需要 | 可选需要 | 做的阶段 |
|---|---|---|---|---|---|
| `activity` | 活动记录的入库和读取 | `ActivityWrite`、`ActivityRead`；扩展点 `ActivityProcessors`；事件 `ACTIVITY_COMMITTED` | `Sql`、`Clock`、`Log` | | 2 |
| `focus` | 把焦点变化变成「窗口使用时段」 | — | `Windows`、`Timer`、`Clock`、`ActivityWrite`、`Log` | | 3 |
| `query` | 对外的只读查询，统一执行隐藏规则 | `Memory` | `ActivityRead`、`Clock`、`CliCommands` | `TuiPages`、`Search`、`DocumentRead`、`McpTools` | 4（按天的时长统计），5、6 补全 |
| `afk` | 把离开 / 回来变成「离开时段」 | — | `Idle`、`ActivityWrite` | | 5 |
| `privacy` | 排除和脱敏 | 登记到 `ActivityProcessors` | `ActivityProcessors` | | 7 |
| `project` | 把活动归到项目 | 登记到 `ActivityProcessors` | `ActivityProcessors` | | 7 |
| `shell` | 导入 shell 命令 | — | `ShellHistory`、`Timer`、`Clock`、`Sql`、`Migrations`、`ActivityWrite`、`Log` | | 8 |
| `browsing` | 导入网页访问 | — | `BrowserHistory`、`Timer`、`Clock`、`Sql`、`Migrations`、`ActivityWrite`、`Log` | | 9 |
| `notes` | 把 Markdown 日记同步进文档库 | — | `Files`、`Timer`、`DocumentWrite` | | 之后 |
| `exec-import` | 运行外部命令导入活动 | — | `Subprocess`、`ActivityWrite` | | 之后 |
| `documents` | 文档、分块、全文索引 | `DocumentWrite`、`DocumentRead` | `Sql`、`Tokenizer` | | 之后 |
| `vectors` | 向量索引 | `VectorSearch` | `Sql`、`Embedder`、`DocumentRead` | | 之后 |
| `search` | 检索（先全文，以后加向量） | `Search` | `Sql`、`Migrations`、`ActivityRead`、`Tokenizer`、`Log` | `Timer`（建索引）、以后 `VectorSearch` | 11 |
| `summary` | 每日 / 每周摘要 | `Summarize` | `ActivityRead`、`DocumentWrite`、`Llm`、`Files`、`Clock` | | 之后 |

第一版（阶段 0–4）做完得到：niri 焦点时段 → SQLite → `lifelog day`。

终端界面的做法：`Tui` 外壳只负责页面切换、按键和绘制，不认识任何 lifelog 名词。页面由 `query` 登记到 `TuiPages`：页面描述是数据（标题、快捷键），内容通过一次请求取得（参数数据 → 表格 / 条形图 / 列表 / 文本块）。和命令行子命令是同一个模式。

### 3.4 依赖图（第一版）

```
应用功能      focus ──────────────┐            query
               │  │  │            ▼              │  │
               │  │  │        activity ◀─────────┘  │
               │  │  │         │    │               │
系统服务    Windows Timer    Sql   Clock  Log     Cli(CliCommands)
           (wm-niri)(timer-tokio)(storage-sqlite)(clock-system)(log-stderr)(cli-clap)
```

- 实线都是 `requires`。系统服务之间：`wm-niri`、`timer-tokio`、`storage-sqlite`、`cli-clap` 需要 `Log`；`wm-niri` 需要 `Clock`。
- 没有环。两处用扩展点反转：`storage-sqlite` 不认识任何表，`activity` 把迁移登记到 `Migrations`；`cli-clap` 不认识任何子命令，`query` 把命令登记到 `CliCommands`。
- 最终范围里的第三处：`activity` 不认识隐私和项目归类，它们登记到 `ActivityProcessors`。处理器的先后顺序是策略，写在 `activity` 的配置里。

### 3.5 通信

| 提供方 | 使用方 | 原语 | 名字 |
|---|---|---|---|
| `wm-niri` | `focus` | 事件 | `FOCUS_CHANGED { at, window: Option<{ id, app_id, title }> }` |
| `timer-tokio` | `focus` | 请求 + 事件 | `Timer::every(period_ms) → TimerId`、`TIMER_FIRED { id, at }` |
| `activity` | `focus` | 请求 | `ActivityWrite::submit(Vec<Activity>) → Vec<ActivityId>` |
| `activity` | `query` | 请求 | `ActivityRead::durations(query, group_by)`、`list(query)` |
| `storage-sqlite` | `activity` | 请求 | `Sql::query(Statement)`、`Sql::execute(Vec<Statement>)`（一批一个事务） |
| `storage-sqlite` | `activity` | 扩展点 | `Migrations` |
| `cli-clap` | `query` | 扩展点 | `CliCommands`：命令描述（数据）+ 执行（参数数据 → 输出数据或错误） |
| `activity` | 将来的索引 | 事件 | `ACTIVITY_COMMITTED { ids }` |

`Sql` 契约先只有「单条查询」和「一批语句一个事务」两个操作，都是纯数据进出，不需要事务句柄。以后出现「读了再决定怎么写」的事务时再加句柄式操作（只加不改，次版本 +1）。

拆分举例：旧设计的 `ll_source_niri` 一个 crate 里既连 niri 又统计焦点时段。现在连 niri、解析它的消息、维护窗口表属于 `wm-niri`；标题规范化、时段状态机、定时刷新、去重键属于 `focus`。换窗口管理器只写一个新的系统服务。

### 3.6 契约和可替换性

| 契约 | 现有实现 | 契约测试 | 换掉它要做什么 |
|---|---|---|---|
| `Clock` | 系统、假 | 有 | 写组件 → 跑契约测试 → 改配置 |
| `Log` | stderr、内存 | 有 | 同上 |
| `Sql` | SQLite（文件 / 内存） | 有 | 同上；**已知限制**：各功能适配层里的 SQL 是 SQLite 方言，换数据库引擎还要改这些 SQL |
| `Timer` | 线程、手动 | 有 | 同上 |
| `Windows` | niri、回放 | 有 | 同上 |
| `Cli` | clap | 入口测试 | 同上 |

测试里换系统服务用的是同一种方式：testkit 往组件列表里放假实现。

### 3.7 配置

`~/.config/lifelog/config.toml`，可以不存在，全部有默认值。

```toml
[components.storage-sqlite]
path = "~/.local/share/lifelog/lifelog.db"

[components.focus]
flush_interval_secs = 30
title_suffixes = [" — Zen Browser", " — Mozilla Firefox", " - Google Chrome", " - Chromium"]

[components.query]
day_boundary = "04:00"
hidden_sources = []

[components.wm-niri]
reconnect_initial_secs = 1
reconnect_max_secs = 60

[components.activity]
processors = ["project", "privacy"]   # 默认值；先归类，最后脱敏

[components.project]
roots = ["~/work/projects"]

[components.privacy]
builtin_rules = true
```

- 每个组件一段，都有 `enabled`（默认 `true`）。未知的键启动失败。
- 两个进程读同一个文件。一个组件只有在「编译进了这个进程」且「配置里启用」时才启动。配置里出现本进程没有、但仓库里存在的组件段，忽略；出现仓库里根本不存在的段，报错（防拼写错误）。「仓库里有哪些组件」由构建脚本从各包的元数据生成，不手写。
- 策略都在配置里：刷新间隔、重连退避、处理器顺序、日界。

### 3.8 kernel 和启动器

**kernel**（`crates/kernel`，依赖只有 `kernel-api`、tokio、tokio-util）

- 生命周期：去掉停用的 → 按契约找到提供者 → 拓扑排序 → 逐个校验配置并 `start` → 停止时逆序。缺提供者、成环、同一契约两个提供者、配置不合法：启动失败，报错写明是哪个组件、哪个契约。
- 命名：`provide` / `resolve` / `try_resolve`，`contribute` / `collect`。
- 请求：`resolve` 返回 `Arc<dyn 契约>`，之后直接 `.await` 调用。
- 事件：一个投递任务加一个有界通道。`publish` 入队（满则等待），投递任务按订阅登记的顺序依次 `.await` 处理函数。处理函数返回错误时记到监听点和日志，不重试。测试里换成手动投递，结果可重复。
- 后台任务：组件用 `host.spawn(name, f)` 起任务，`f` 拿到一个取消信号；kernel 记下任务，停止时发取消信号并等待结束（超时由启动器给）。重连、退避这些由组件自己做，参数在它自己的配置里。
- 监听点：开启后，每次请求（契约、方法、结果是成功还是哪种错误、耗时）和每次发布、投递都产生一条记录。`lifelogd --trace` 和 `lifelog --trace` 把记录按行写到 stderr。

**契约怎么声明**：用 `kernel-api` 里的 `contract!` 宏。它生成 trait、契约的静态标识、一个带记录点的包装（开启追踪时 `resolve` 返回包装后的句柄），并在编译期检查每个参数和返回值都是「可序列化、拥有所有权」的类型。原则 5 因此由编译器检查，不靠约定。

**启动器**

| 进程 | 装哪些组件 | 做什么 |
|---|---|---|
| `lifelogd` | 全部系统服务和采集类功能、`activity` | 启动 → 等 SIGINT / SIGTERM → 停止。systemd 用户服务，崩溃由 systemd 重启 |
| `lifelog` | `clock-system`、`log-stderr`、`storage-sqlite`（只读）、`cli-clap`、`activity`、`query` | 启动 → 调 `Cli::run(args)` → 输出 → 退出码 |
| `lifelog-tui` | 同 `lifelog`，`cli-clap` 换成 `tui-ratatui` | 启动 → 调 `Tui::run()`，退出界面后停止 |

`lifelog` 把存储覆盖成只读是启动器的事（命令行覆盖配置），组件只看到最终配置。只读模式下存储服务只核对迁移版本，不执行迁移；版本落后时报错并提示先启动 `lifelogd`。

**写入和定时**

- `lifelogd` 是唯一写者，沿用旧设计。
- 轮询型数据源（shell、浏览器、笔记）在 `lifelogd` 里用 `Timer` 周期触发，间隔在各自配置里。
- 每日摘要这类「到点做一次、错过要补」的任务不在进程里调度：systemd timer（`OnCalendar` + `Persistent=true`）到点执行 `lifelog summarize`，命令经本地 socket 交给 `lifelogd` 执行。这一步做到时就是 §2 说的升档点。

## 4. 数据设计

- 表名前缀是组件 id。第一版只有 `activity_records`，列和旧设计 §6 的 `events` 相同（`source`、`kind`、`ts_start`、`ts_end`、`tz_offset_min`、`app`、`title`、`project`、`payload`、`dedup_key`、`redacted`、`ingested_at`）。
- 迁移由各组件登记到 `Migrations`，版本从 1 递增，执行记录带 SHA-256；已执行的迁移被改动时拒绝启动。旧设计用 `PRAGMA user_version`，改成这个。
- 时间存 UTC 毫秒，另存 `tz_offset_min`。
- 时段的写入方式沿用旧设计：打开时写一次，之后每个刷新间隔写一次，关闭时写最后一次，同一时段用同一个 `dedup_key`，存储层按它更新成一行。
- 之后的表（文档、分块、全文索引、向量、游标）到对应阶段再定，各归各的组件。给 agent 用的 `agent_*` 视图归 `query`。

## 5. 对外接口

查阅每天做了什么是首要目标，对应的命令：

```
lifelog day [<日期>] [--by app|title] [--json]    # 某一天的时长统计，默认今天；阶段 4
lifelog today                                      # = lifelog day today；阶段 4
lifelog week [<周>] [--by app|project]             # 一周每天的分布；阶段 5
lifelog timeline [<日期>]                          # 按时间顺序列出这一天的时段；阶段 5
lifelog-tui                                        # 终端界面：按天翻看统计和时间线；阶段 6
```

日期接受 `2026-11-12`、`today`、`yesterday`、`3d`；按逻辑日（日界默认 04:00）解释。所有命令都有 `--trace`。

沿用旧设计 §7.2 的约定，有两处按规范调整：

| | 旧设计 | 现在 |
|---|---|---|
| 输出格式 | 默认表格，`--json` 才是 JSON | stdout 是终端 → 表格；不是终端，或给了 `--json` → JSON。默认行为对管道友好 |
| 退出码 | 0 / 1 / 2 | sysexits，由错误种类决定（输入不合法 65、找不到 66、不可用 69、配置错 78 等） |

日志和错误走 stderr；没事不输出。

## 6. 错误处理

- 预期内的失败一律返回 `Result<T, AppError>`，种类是规范里的七种。panic 只用于 bug，由 systemd 重启进程。
- 到退出码的映射只在 `cli-clap` 里写一次。
- `wm-niri` 断线：发布一次「没有焦点窗口」，按配置退避重连，重连后用新快照重建窗口表。
- 活动没通过校验：只丢这一条，记日志。一批写入失败：整批不写，记日志；时段类活动的下一次刷新带同一个去重键，会补回来。
- 未知的 niri 消息跳过。

## 7. 检查手段和测试

**架构检查**（`tests/arch`，读 `cargo metadata`，阶段 0 就要能报错）：

| 规则 | 内容 |
|---|---|
| R1 | 组件包只能依赖 `kernel-api`、`contracts/*`、别的功能的 `*-contracts`、自己的 `core` |
| R3 | `services/*` 不依赖任何 `features/*` |
| R4 | 契约包只依赖 `kernel-api`、serde、jiff 和别的契约包 |
| R5 | 内部包里 `kernel` 只依赖 `kernel-api`；外部依赖只有 tokio、tokio-util |
| R6 | 组件包不依赖 `kernel`、`boot` |
| R8 | `features/*/core` 不依赖任何 IO crate（黑名单：rusqlite、clap、tokio、inventory 等；tokio 只允许出现在测试依赖里）；内部包里只能依赖 `kernel-api` 和各种契约包 |

R2（用到的契约都在 `requires` 里声明）由 kernel 在运行时检查：`resolve` 一个没声明的契约直接启动失败。原则 5（消息是纯数据）由 `contract!` 宏在编译期检查。R10：`clippy::print_stdout`、`print_stderr` 设为 deny，只有 `boot` 和 `cli-clap` 例外。

**测试分层**

| 层 | 起什么 | 重点 |
|---|---|---|
| 单元 | 什么都不起 | `focus/core` 的状态机和标题规范化、`query/core` 的日界和格式化、`activity/core` 的校验 |
| 契约 | 一个实现 | `Sql`、`Clock`、`Timer`、`Windows`、`Log` 各一套 |
| 组件 | testkit | 回放录制的焦点变化 → 活动表里的时段；关掉 `focus` 后其余照常；缺提供者时的报错 |
| 入口 | 真实进程 | `lifelog today --json` 的输出结构和退出码；`lifelogd` 用回放实现跑一遍 |

覆盖率门槛（`core` 包 ≥ 90%，整体 ≥ 80%）第一版先量出来，不设为硬门槛，等数字稳定后再打开。

## 8. 交付顺序

每个阶段结束都跑全量检查（格式、clippy、架构检查、全部测试）并把命令和结果写进 `PROGRESS.md`。

| 阶段 | 内容 | 验收 |
|---|---|---|
| 0 | workspace 骨架、lint 配置、架构检查、检查脚本、提交钩子 | 故意加一条违规依赖，架构检查报错并说明规则 |
| 1 | `kernel-api`（含 `contract!`）、`kernel`、`boot`、`testkit`；`Clock`、`Log` 及实现 | 用假组件验证：排序、缺提供者、成环、双提供者、停用、事件投递顺序、追踪记录；`contract!` 拒绝非数据参数（编译失败测试） |
| 2 | `Sql` + `storage-sqlite`；`activity` | 契约测试；迁移校验和被改动时拒绝启动；按去重键更新；时长统计（搬旧仓库的用例） |
| 3 | `Windows` + `wm-niri` + 回放实现；`Timer`；`focus` | 回放旧仓库的 `scratch/m1-4` 录制，得到的时段和旧实现一致 |
| 4 | `Cli` + `cli-clap`；`query`；两个启动器；systemd 单元 | 真机：`lifelogd` 跑 10 分钟后 `lifelog day` 有合理输出；`--trace` 能看到谁在调谁 |
| 5 | `Idle` + `afk`（扣除离开时间，时长可信）；`lifelog week`、`lifelog timeline` | 离开期间不计入时长；三个查阅命令覆盖「某天 / 某周 / 按时间顺序」 |
| 6 | `Tui` + `tui-ratatui`；`query` 登记页面；`lifelog-tui` | 在终端界面里按天翻看统计和时间线 |
| 7 | `privacy`、`project`；`--by project` | 入库前脱敏、归类；原标题不进数据库；按项目统计 |
| 8 | `ShellHistory` + `shell-atuin`；`shell`；`lifelog commands` | 读 atuin 库增量导入，还在执行的命令等结束；按目录归类、脱敏 |
| 9 | 浏览历史：汇总服务 + 三个浏览器适配；`browsing`；`lifelog visits` | 三个浏览器、多个 profile 增量导入；按 profile 归类；按域名排除 |
| 11 | 全文检索：`Tokenizer` + `tokenizer-cjk`；`search`；`lifelog search` | 中日英、命令、网址都能搜到；按时间和来源过滤；隐藏来源不出现 |
| 12 | 给 agent：`Mcp` + `mcp-stdio`；`query` 登记工具；`lifelog-mcp`；skill | 真实进程按 MCP 握手、列工具、调用；stdout 只有协议消息；`agent_hidden_sources` 生效 |
| 之后 | 笔记导入（shell、浏览器、笔记）→ 文档和检索 → agent 接口补全 + MCP → 摘要（含升档）→ 外部命令数据源 | 每个阶段开始前补进计划 |

## 9. 和旧设计相比变了什么

| 方面 | 旧 | 新 | 对应原则 |
|---|---|---|---|
| 公共包 | `ll_core`：领域类型、所有 trait、注册表、管道、调度器、配置 | `kernel-api` + `kernel` 不认识任何 lifelog 名词；领域类型归 `activity` | 1 |
| 契约的位置 | 全在 `ll_core` | 各归各：系统服务的在 `contracts/*`，功能的在自己的 `contracts` | 3、6 |
| 替换 | 采集器靠配置；`Embedder` 等靠 `bins/*` 注入 | 都是「换一个提供同一契约的组件，改配置」 | 6 |
| 入库管道 | core 里的「采集 → 处理器链 → 存储」 | `activity` 的用例；处理器是它的扩展点 | 1、4 |
| niri | 一个插件连 socket 并统计 | `wm-niri`（系统服务）+ `focus`（应用功能） | 2、3 |
| 并发 | tokio，trait 返回装箱 future | tokio，契约方法用 `async-trait` | — |
| 目标优先级 | agent 查询和摘要在前 | 自己查阅在前，终端界面进入范围；agent 和摘要靠后 | — |
| 领域记录的名字 | `RawEvent`、表 `events` | `Activity`、表 `activity_records` | —（见 Q1） |
| 退避、批量大小 | 写在 core 里 | 各组件自己的配置 | 9 |
| 调度器 | core 里的周期任务 + 补跑 | `Timer` 系统服务管周期；到点任务交给 systemd timer | 1 |

## 10. 已决定（2026-11-12）

| | 问题 | 决定 |
|---|---|---|
| Q1 | 领域记录和 kernel 的「事件」撞名 | 改叫「活动」（`Activity`），表 `activity_records` |
| Q2 | 并发底座 | **用 tokio**（另一个选项是同步 + 线程）。kernel 因此依赖 tokio |
| Q3 | 每个应用功能几个 crate | 三个（契约 / 纯逻辑 / 适配），实际感受记进反馈 |
| Q4 | 仓库名 | `lifelog`，可执行文件 `lifelog`、`lifelogd`、`lifelog-tui` |
| Q5 | 存储契约 | 通用 SQL 执行，表和 SQL 归各功能 |
| — | 目标优先级 | 首要是自己查阅（命令 + 终端界面），agent 和 LLM 摘要次要 |

## 11. 实现中的调整

| 方面 | 设计原文 | 实际 | 原因 |
|---|---|---|---|
| `start` | 组件只有一个入口 `start` | `start` 只做登记；要调别的组件的事放进 `host.on_ready`，后台任务在全部就绪后才开始 | `start` 时别的组件可能还没启动，扩展点的登记也不全 |
| 事件的归属 | 没写 | 每个事件属于一个契约；提供它的组件才能发布，声明了需要它的组件才能订阅 | 让事件的依赖也能在启动时检查 |
| 投递失败 | 「记到监听点和日志」 | 记到监听点和一个单独的「故障」出口（启动器提供，写到 stderr） | kernel 不认识 `Log` 契约 |
| 定时器实现的名字 | `timer-thread` | `timer-tokio` | 并发底座改成了 tokio |
| 活动的来源名 | 沿用旧设计的 `niri` | 窗口时段的 `source` 是 `focus`，去重键是 `focus:<启动毫秒>:<序号>` | 记录活动的是 `focus` 功能，它不知道窗口管理器是哪一个 |
| 只读进程的存储错误 | 「只读模式下……版本落后时报错」（启动时） | 只读进程照常启动，存储的问题（文件不存在、结构落后、校验和不符）在第一次查询时报；可写进程仍然启动时失败 | `lifelog --help` 不该因为数据库还没建好就跑不了 |
| 启动失败时的退出码 | 「到退出码的映射只在 `cli-clap` 里写一次」 | 映射表 `exit_code` 放在 `cli-contract`，`cli-clap` 和两个启动器都用它 | 启动失败时命令行服务还没起来，启动器也要映射 |
| 回放用的窗口管理器 | 契约包里的假实现 | `wm-niri` 包里的第二个组件 `wm-niri-replay`（`wm_niri::replay`）；契约包里另有手动控制的 `FakeWindows` | 回放要复用 niri 的消息解析和窗口表，这些不能出现在契约包里 |
| 组件列表的生成 | 「由构建脚本从各包的元数据生成」 | 单独的小包 `crates/boot-build`，两个启动器的 `build.rs` 各一行调用 | 两个启动器共用，且能单独测试 |
| 端到端测试 | 入口测试放在各启动器里 | 单独的包 `tests/e2e`：编译两个程序，起一个假的 niri socket | 要同时用到两个程序 |
| 定时器事件 | `TIMER_FIRED { id, at }` | `Fired { id }`，不带时刻；所有订阅者都收到，各自按 `id` 认领 | 时刻由订阅方问 `Clock`，定时器不必依赖时钟 |
| 帮助信息 | — | 子命令和参数的说明是中文；`Usage:`、`Commands:` 这些框架文字是 clap 默认的英文 | 没有本地化，记为已知问题 |
| 空闲契约的事件 | `IDLE_CHANGED`（未定内容） | `Away { since, at }` / `Back { at }`。离开的起点 `since` = 发现的时刻 - 判定时长，由提供者算 | 只有提供者知道判定时长 |
| 离开时间怎么扣 | 旧设计用数据库视图 `v_activity` | `query` 读出窗口时段和离开时段，在纯逻辑里扣（`query-core::effective`）；`ActivityRead::durations` 不再被 `query` 使用 | 扣除要逐段切开，放在纯逻辑里好测；表和 SQL 不需要知道「离开」的含义 |
| 标题规范化 | 固定规则 | 固定规则之后再按配置的正则改写；默认带一条 kakoune 状态栏规则 | 真机发现 kakoune 把光标位置写进终端标题，十几秒就切出几十个时段。各应用的标题格式是策略 |
| `afk` 的包 | 三个包 | 两个（`core`、`component`），没有 `contracts` | 它不对外提供任何契约 |
| 终端界面的翻页 | 没写 | 外壳只有一个通用的「偏移」（0 是现在，-1 是上一个），页面自己决定一格是一天还是一周 | 外壳不该认识「日期」这种业务名词 |
| 页面内容 | 「表格 / 条形图 / 列表 / 文本块」 | `Heading`、`Lines`、`Bars`、`Table`、`Empty` 五种块；样式只有四种语义（普通、次要、突出、强调），颜色由外壳决定 | 够画三个页面；样式用语义而不是颜色，换外壳不用改页面 |
| 页面的排版 | 没写 | `query-core::pages`：把报告变成页面数据，纯函数 | 和命令行的文本排版放在一起，都能不起界面就测 |
| `privacy`、`project` 的包 | 三个包 | 各两个（`core`、`component`）；`requires` 写 `ActivityProcessors` | 不对外提供契约；登记扩展点要先声明需要它 |
| `payload` 里的键 | 各来源自定 | `activity-contracts::payload_keys` 约定 `cwd`、`url` 两个跨组件用到的键 | 数据源写、`project` 和 `privacy` 读，名字要有一个地方定 |
| 处理器的默认顺序 | 配置示例里是 `[]` | 默认 `["project", "privacy"]` | 两个都启用时空列表会让常驻进程起不来；顺序本身仍然可以改 |
| 排除规则命中时 | 旧设计：标题和网址换成 `[redacted]` | 标题和 `payload` 里所有字符串（含嵌套）都换掉；应用、时长、数字保留 | 不用认识每个来源的 `payload` 有哪些键 |
| 项目推断 | 旧设计：「根据 cwd 与窗口标题推断」 | 规则 → 工作目录 → 标题里的路径；认 fish 的缩写路径；只看文字，不读文件系统 | 缩写路径里项目名后面还有下级时项目名本身也被缩写了，不猜 |
| 时间线的粒度 | 「按时间顺序列出这一天的时段」 | 短于 `timeline_absorb_secs`（默认 60）的项并进前面的长项，记在 `during` 里；被短暂打断的同一件事合成一项 | 真机上绝大多数时段不到 1 分钟（在 agent 会话、shell、编辑器之间来回切），逐项列出看不出在做什么 |
| 不到 1 分钟的时长 | 「不到 1 分」 | 显示秒 | 合并后「其间」里多是几十秒的项 |
| shell 历史的游标 | 旧设计：`cursors` 表，提交成功后推进 | 游标的内容由 `ShellHistory` 的提供者决定（不透明字符串），`shell` 存在自己的 `shell_cursor` 表；活动先提交、再存游标，中途失败下次重读，靠去重键 `shell:<id>` 不重复 | 「还在执行的命令」只有提供者知道怎么判断（atuin 开始时写一行、结束时补耗时），所以游标停在哪由它算 |
| 查看命令 | 没写 | `Memory::commands`、`lifelog commands`、终端界面 `c` 页 | 导入了就要能看到；和其它查询同一个模式 |
| atuin 不存在时 | 没写 | `shell-atuin` 照常启动，读的时候报「不可用」；`shell` 只提醒一次，下一轮再试 | 常驻进程不该因为一个可选的数据源还没装就起不来 |
| atuin 历史库的位置和内容 | 没写 | 默认依次找 `$ATUIN_HOME/data`、`~/.atuin/data`、`$XDG_DATA_HOME/atuin`、`~/.local/share/atuin`；游标是（时间戳，id）；没有耗时但目录不是绝对路径的（导入的）不等；`: 秒:秒;命令` 这种 zsh 扩展历史格式取出真实时刻 | 真机：atuin 18 的库可能在 `~/.atuin/data`；导入的历史大量共用一个时间戳，只用时间戳做游标丢了几百条；导入的 zsh 历史时间全是导入那一刻 |
| 窗口的工作区 | 没写 | `WindowInfo` 加 `workspace`（所在工作区的名字，只加字段）；`wm-niri` 解析 `WorkspacesChanged` 和窗口的 `workspace_id`；`focus` 换工作区也切段，名字记进 `payload.workspace`；`project` 的规则可以按工作区名归类 | 真机：agent 会话窗口的标题是任务名，归不到项目；每个项目一个命名工作区时，工作区名就能归类 |
| 「可多个提供者」的浏览历史 | 一个契约多个提供者 | 系统服务 `browser-history` 拥有扩展点 `BrowserAdapters` 并提供请求契约 `BrowserHistory`；三个浏览器适配登记到扩展点；`browsing` 只用请求契约 | 一个请求契约不能有两个提供者；扩展点若由应用功能拥有，登记它的系统服务就依赖了应用功能，kernel 启动时拒绝 |
| 读浏览器的库 | 没写 | 每次先把库和 `-wal` 复制到临时目录再读；默认 5 分钟一次 | 浏览器开着时独占锁住自己的库 |
| profile 归类 | 没写 | `payload_keys::PROFILE`；`project` 的规则加 `profile` 条件；qutebrowser 每个项目一个实例，实例名就是项目名 | |
| 把一天合在一起看 | 时长统计和时间线只看窗口 | 时间线每一段带着这段时间里的命令和网页（`commands`、`visits`，相同的合并计数；挂到「开始不晚于它的最后一段」，离开期间的不挂）；`Memory::projects` / `lifelog projects`：每个项目的窗口时长、命令数、网页数；终端界面日页的「按项目」用它 | 笔记少，先做「按项目的一天」，仍属于首要目标 |
| 检索的范围 | `search` 依赖 `documents`（文档、分块） | 先直接索引活动（标题、网址、项目），表 `search_*` 归 `search`；`documents` 等有长文本（摘要、笔记）时再做 | 现在只有短文本；先全文、向量以后加 |
| 谁建索引 | 没写 | 同一个 `search` 组件：进程里有 `Timer` 才建索引（常驻进程），没有就只检索（命令行） | 不用为了「写的一半」和「读的一半」拆两个组件 |
| 索引怎么增量 | 「按还没处理的拉取」 | `ActivityRead` 加 `since(编号, 条数)`、`get(编号们)`；索引行和游标在同一个事务里写 | 活动表的编号递增，比订阅事件简单，也能补建历史 |
| 检索结果的顺序 | 旧设计：RRF 融合 | 最近的在前；同一来源里标题和网址都一样的合成一条（带次数） | 「那天看过的那个」是按时间找；短标题之间的相关度差别不大 |
| 命令行的多词参数 | 没写 | `cli` 契约加 `ArgKind::Rest`：剩下的位置参数用空格连起来 | `lifelog search jiff 文档` 不用加引号 |
| 终端界面的输入 | 没写 | `PageSpec.input`（提示语）+ `PageRequest.input`（提交的文字）：页面声明要输入框，外壳负责编辑（`/` 开始、`Enter` 提交、`Esc` 放弃，输入时按键都进输入框），页面只收到提交的文字。`query` 在启用了检索时登记「搜索」页 | 和「翻页只是偏移」同一个思路：外壳不知道输入的是什么 |
| MCP 的实现 | `mcp-rmcp`（用 rmcp） | `mcp-stdio`：自己用 serde_json 实现协议（`initialize`、`ping`、`tools/list`、`tools/call`），参数的 JSON Schema 由工具描述生成；参数不对、工具出错作为工具的错误结果，工具不存在、方法不认识是协议错误 | 只用到这几个方法，自己写好测、依赖少 |
| MCP 从哪启动 | 旧设计：`lifelog mcp` 子命令 | 单独的启动器 `lifelog-mcp` | 命令行服务在命令结束后才写输出，不适合长时间占着标准输入输出 |
| 给 agent 的隐藏规则 | 旧设计：`hidden_sources` 对 agent 严格执行，命令行另有 `--include-hidden` | `hidden_sources` 对所有出口都生效；`agent_hidden_sources` 只对 MCP 工具额外隐藏（`query` 给工具用的是另一份合并了它的查询服务） | 「自己看得到、agent 看不到」比「默认都看不到、加参数才看到」简单 |
| 给 agent 的输出规模 | 旧设计：默认限制条数，`truncated` | 命令、网页列表默认最近 100 条（`truncated`、`total`）；时间线每段最多 10 条命令、10 个网页；网址最多 200 字符 | 真机一天几十个网页原本有几万字符，网址里的查询参数占了大头 |
| 给 agent 的说明 | 旧设计：`skills/lifelog/SKILL.md` | 照做；安装脚本放到 `~/.claude/skills/lifelog` | |
| 检索的候选 | 先取「条数 × 20」个候选再合并 | 一页一页地取，直到合并后够了或者没有了（最多 2 万个） | 真机：同一个窗口出现几十次，候选全是它，要 3 条只给了 1 条 |
| 已有的记录 | 没写 | 处理器只在入库时运行；改规则不影响已经存下的记录 | 重新加工是写操作，要交给常驻进程，等升档时一起做（见 PROGRESS「还没做的」） |
| 命令的屏蔽 | 内置规则里写死 `pass` 等 | `privacy.exclude_commands`，默认 `pass`、`gopass`、`passage`，和 `builtin_rules` 分开 | 命令行密码管理器各人不同（`bw`、`op`），要能配置 |
| 配置文档 | README 里一段示例 | `docs/configuration.md` 列全部配置项；每个组件实现 `config_defaults()`，`tests/config-doc` 对照 | README 那段已经和代码不一致（`storage-sqlite.path`），手写的文档没人检查就会过时 |

