# 架构

lifelog 按微内核的方式组织：**kernel 只提供机制，一切能力都是组件，组件之间只通过契约里的纯数据消息通信。**
设计的来龙去脉见[设计文档](specs/2026-11-12-lifelog-design.md)，每日摘要和跨进程那部分见[摘要设计](specs/2026-11-12-summary-design.md)。

## 分层

```
┌──────────── 启动器（apps/） ───────────────────────────────────────────────┐
│  lifelogd（常驻，唯一写库）   lifelog（命令行）   lifelog-tui   lifelog-mcp  │
│  装哪些组件 = Cargo.toml 里依赖了哪些组件包                                    │
└──────────────────────────────────────────────────────────────────────────────┘
┌──────────── 应用功能（features/） ─────────────────────────────────────────┐
│  采集    focus  afk  shell  browsing  notes                                   │
│  处理    activity ← project、privacy（登记成处理器）                          │
│  查看    query（日、时间线、周、项目；登记命令、页面、MCP 工具）               │
│  检索    search  vectors  documents                                           │
│  摘要    summary  summary-cli                                                 │
└──────────────────────────────────────────────────────────────────────────────┘
┌──────────── 系统服务（services/），不出现业务名词 ─────────────────────────┐
│  wm-niri  idle-wayland  shell-atuin  browser-*      ← 外部数据从这里进来       │
│  storage-sqlite  files-std  clock-system  timer-tokio  log-stderr             │
│  tokenizer-cjk  embed-fastembed  llm-openai-compat                            │
│  cli-clap  tui-ratatui  mcp-stdio                   ← 展示外壳，内容由功能登记 │
└──────────────────────────────────────────────────────────────────────────────┘
┌──────────── kernel（crates/kernel） ───────────────────────────────────────┐
│  生命周期 · 命名 · 请求/响应 · 发布/订阅 · 扩展点 · 监听 · 跨进程导出/导入      │
└──────────────────────────────────────────────────────────────────────────────┘
```

## 几条规则

1. **kernel 不认识任何 lifelog 名词。** 它不知道什么是窗口、活动、数据库，也不读配置文件；除了 `kernel-api` 只依赖 tokio。
2. **依赖只能从应用功能指向系统服务，而且无环。** 系统服务之间、功能之间需要反向调用时，用扩展点倒过来：
   存储服务不认识任何表，有表的组件把迁移登记到它的 `Migrations` 扩展点；命令行服务不认识任何子命令，`query` 把命令登记到 `CliCommands`。
3. **组件只通过契约通信。** 契约用 `contract!` 声明，参数和返回值必须是可序列化的数据，错误也是数据（`AppError`）。不满足就编译失败。
4. **每个组件独立配置、独立启停。** `[components.<id>]` 一段，`enabled = false` 关掉。必需的依赖缺了，启动时报出是谁要它。
5. **替换只有一种方式**：另一个提供同一契约的组件，通过同一份契约测试，改配置或启动器的依赖。

这些规则由三处检查：`tests/arch`（按目录判断包的角色，检查依赖方向）、kernel 的启动校验（声明的依赖和实际使用一致）、编译器（契约只能是纯数据）。

## 一条活动的旅程

以窗口焦点为例：

1. `wm-niri` 从 niri 的 socket 读到焦点变化，以 `Windows` 契约的事件发布出来。
2. `focus` 订阅它，去掉标题里会变的部分，把「在这个窗口上从几点到几点」交给 `activity`。
3. `activity` 按配置的顺序调用登记到 `ActivityProcessors` 的处理器：`project` 归类，`privacy` 脱敏。
4. `activity` 通过 `Sql` 契约写进 `storage-sqlite`。
5. `search` 定时把新活动加进全文索引。
6. 你运行 `lifelog timeline`：`cli-clap` 解析参数，交给 `query` 登记的命令；`query` 通过 `ActivityRead` 读出来、排成时间线，返回纯数据；`cli-clap` 按终端或管道决定输出文本还是 JSON。

每一步都可以用 `--trace` 看到。

## 跨进程

只有 `lifelogd` 写数据库，也只有它加载几百 MB 的嵌入模型。命令行进程需要「生成摘要」「按意思检索」时：

- `lifelogd` 把 `Summarize`、`VectorSearch` 两个契约导出到 `$XDG_RUNTIME_DIR/lifelog/daemon.sock`；
- `lifelog` 里登记一个 `Import` 组件（`summary-remote`、`vectors-remote`），提供同一个契约，里面是 `contract!` 生成的代理；
- `summary-cli` 拿到的句柄和进程内的没有区别，契约和组件一行没改。

## 组件一览

| 组件 | 类别 | 提供 | 需要 |
|---|---|---|---|
| `clock-system` | 系统服务 | `Clock` | |
| `log-stderr` | 系统服务 | `Log` | |
| `timer-tokio` | 系统服务 | `Timer` | |
| `storage-sqlite` | 系统服务 | `Sql`、扩展点 `Migrations` | |
| `files-std` | 系统服务 | `Files` | |
| `wm-niri` | 系统服务 | `Windows` | `Clock`、`Log` |
| `idle-wayland` | 系统服务 | `Idle` | `Clock`、`Log` |
| `shell-atuin` | 系统服务 | `ShellHistory` | `Clock` |
| `browser-history` | 系统服务 | `BrowserHistory`、扩展点 `BrowserAdapters` | `Log` |
| `browser-firefox`、`browser-chromium`、`browser-qutebrowser` | 系统服务 | 登记到 `BrowserAdapters` | `BrowserAdapters` |
| `tokenizer-cjk` | 系统服务 | `Tokenizer` | |
| `embed-fastembed` | 系统服务 | `Embedder` | |
| `llm-openai-compat` | 系统服务 | `Llm` | |
| `cli-clap` | 系统服务 | `Cli`、扩展点 `CliCommands` | |
| `tui-ratatui` | 系统服务 | `Tui`、扩展点 `TuiPages` | |
| `mcp-stdio` | 系统服务 | `Mcp`、扩展点 `McpTools` | |
| `activity` | 应用功能 | `ActivityWrite`、`ActivityRead`、扩展点 `ActivityProcessors` | `Sql`、`Migrations`、`Clock`、`Log` |
| `focus` | 应用功能 | | `Windows`、`Timer`、`Clock`、`ActivityWrite`、`Log` |
| `afk` | 应用功能 | | `Idle`、`Timer`、`Clock`、`ActivityWrite`、`Log` |
| `shell` | 应用功能 | | `ShellHistory`、`Timer`、`Clock`、`Sql`、`Migrations`、`ActivityWrite`、`Log` |
| `browsing` | 应用功能 | | `BrowserHistory`、`Timer`、`Clock`、`Sql`、`Migrations`、`ActivityWrite`、`Log` |
| `project` | 应用功能 | 登记到 `ActivityProcessors` | `ActivityProcessors` |
| `privacy` | 应用功能 | 登记到 `ActivityProcessors` | `ActivityProcessors` |
| `query` | 应用功能 | `Memory` | `ActivityRead`、`Clock`；可选 `CliCommands`、`TuiPages`、`McpTools`、`Search` |
| `search` | 应用功能 | `Search` | `Sql`、`Migrations`、`ActivityRead`、`Tokenizer`、`Log`；可选 `Timer`（有它才建索引） |
| `documents` | 应用功能 | `DocumentWrite`、`DocumentRead` | `Sql`、`Migrations`、`Clock` |
| `notes` | 应用功能 | | `Files`、`DocumentWrite`、`Sql`、`Migrations`、`Timer`、`Log` |
| `vectors` | 应用功能 | `VectorSearch`（常驻进程导出） | `Sql`、`Migrations`、`DocumentRead`、`Embedder`、`Log`；可选 `Timer` |
| `summary` | 应用功能 | `Summarize`（常驻进程导出） | `Memory`、`Llm`、`Files`、`DocumentWrite`、`Log` |
| `summary-cli` | 应用功能 | | `Summarize`、`CliCommands` |

每个组件的配置项见[配置](configuration.md)。

## 仓库结构

```
apps/        启动器：lifelogd、lifelog、lifelog-tui、lifelog-mcp。装哪些组件由 Cargo.toml 的依赖决定
crates/      kernel-api（组件能看到的全部接口）、kernel、boot（启动库）、boot-build、testkit
contracts/   系统服务的契约，一个一包，带假实现和契约测试（testing feature）
services/    系统服务组件，一个一包
features/    应用功能，每个最多三包：contracts（对外契约，没有就不建）/ core（纯逻辑）/ component（接线）
tests/       arch（架构规则）、e2e（真实进程 + 假的 niri）、config-doc（配置文档和默认值一致）
dist/        systemd 用户服务
skills/      给 agent 的说明
docs/        文档、设计、计划
```

## 怎么扩展

### 加一个功能

1. 在 `features/<id>/` 下建 `core`（纯逻辑，好测）和 `component`（接线），需要对外提供契约时再建 `contracts`。
2. `component/Cargo.toml` 里写 `[package.metadata.component] id = "<id>"`，用 `register_component!` 登记。
3. 实现 `config_defaults()`，在 [配置文档](configuration.md)里加一节。
4. 在要用它的启动器的 `Cargo.toml` 里加一行依赖，再加进 `tests/config-doc` 的依赖。

不用改 kernel、不用改别的组件、不用手写组件列表（`boot-build` 在构建时生成）。

### 关掉一个功能

配置里 `enabled = false`。比如不在 Wayland 会话里跑时，关掉 `idle-wayland` 和依赖它的 `afk`；只关前者，启动时会被告知 `afk` 缺了它。

### 换一个实现

比如支持另一个窗口管理器：

1. 在 `services/` 下写一个提供 `Windows` 契约（`contracts/window`）的组件；
2. 跑通 `window-contract` 里的契约测试；
3. 在 `apps/lifelogd/Cargo.toml` 里把 `wm-niri` 换成它，或者两个都装、用 `enabled` 选一个。

`focus` 一行不改。测试里的假时钟、内存日志、手动定时器、回放用的窗口管理器，用的都是这一种方式。

### 想改 kernel 的时候

先停一下：缺的通常是一个系统服务，或者某个契约里的一个扩展点。
