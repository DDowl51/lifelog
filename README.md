<div align="center">

# lifelog

**自动记下你在电脑上做了什么，随时回看「今天 / 那天 / 上周」。**

窗口、离开电脑的时段、shell 命令、浏览历史，全部存在本机的一个 SQLite 文件里。
入库前脱敏、归到项目；可以按天、按时间顺序、按项目看，可以检索，每天自动写一份回顾。

[安装](docs/installation.md) · [使用](docs/usage.md) · [配置](docs/configuration.md) · [隐私](docs/privacy.md) · [架构](docs/architecture.md) · [介绍页](site/index.html)

</div>

---

```console
$ lifelog timeline yesterday
2026-11-11 周三（04:00 起）

  09:02–09:48         46 分  ghostty  ~/w/p/weather-station
                      其间：zen 3 分 · Claude Code 1 分
                      $ cargo run --release ×6
                      $ ls /dev/ttyACM*
                      ↗ ESP32-S3 deep sleep current  docs.espressif.com
  09:48–10:05         17 分  （离开）
  10:05–11:30  1 小时 25 分  zen  ESP32-S3 Technical Reference Manual
  11:30–12:10         40 分  ghostty  ~/w/p/lifelog
                      $ cargo test
                      … 还有 4 条命令
```

## 特点

- **不用手动打卡**：常驻进程从窗口管理器读焦点变化，多久没碰键盘鼠标算离开，离开的时间自动扣掉。
- **一个地方看全**：窗口时段下面直接列出这段时间里执行的命令和看过的网页，知道当时在干什么，而不只是开着哪个窗口。
- **按项目算**：窗口标题、命令的工作目录、浏览器 profile、niri 工作区名，都可以把活动归到项目；`lifelog projects` 看每个项目花了多久。
- **默认就注意隐私**：密码管理器、隐私窗口只记时长；`pass` 这类命令只记时刻；标题、命令、网址里像密钥的片段换成 `[redacted]`。数据只在本机，只有你开启的每日摘要会发给你配置的模型。
- **找得回来**：`lifelog search` 在窗口标题、命令、网页里全文检索，中文、英文都行；摘要和笔记还可以按意思找（本机模型）。
- **每日回顾**：每天凌晨把前一天整理成 Markdown，放在 `~/lifelog/daily/`，可以手改，可以被检索。
- **给程序和 agent 用**：管道里输出稳定的 JSON，退出码遵循 sysexits；附带 Claude Code 的 skill 和一个 MCP 服务。
- **每样能力都是可以关掉、可以替换的组件**：不用的浏览器、不想要的模型，配置里 `enabled = false`。

## 快速开始

需要 Linux + [niri](https://github.com/YaLTeR/niri)、stable Rust（edition 2024）。命令历史需要 [atuin](https://atuin.sh)（可选）。

```bash
git clone https://github.com/DDowl51/lifelog lifelog && cd lifelog
scripts/install.sh                        # 装到 ~/.cargo/bin，放好 systemd 用户服务
systemctl --user enable --now lifelogd    # 开始记录
```

过几分钟：

```bash
lifelog today            # 今天在各个窗口上花了多久
lifelog timeline         # 今天按时间顺序做了什么
lifelog-tui              # 终端界面，←/→ 翻天，q 退出
```

详细步骤（项目归类、每日摘要、给 agent 用）见[安装](docs/installation.md)。

## 能做什么

| 命令 | 作用 |
|---|---|
| `lifelog today` / `day [<日期>]` | 一天在各个窗口上花的时间，`--by app\|title\|project` |
| `lifelog timeline [<日期>]` | 按时间顺序，每段带着期间的命令和网页 |
| `lifelog projects [<日期>]` | 每个项目：窗口时长、命令数、网页数 |
| `lifelog week [<日期>]` | 一周每天的分布 |
| `lifelog commands [<日期>]` | 执行过的命令（目录、耗时、退出码） |
| `lifelog visits [<日期>]` | 访问过的网页 |
| `lifelog search <词…>` | 在所有记录里找；摘要和笔记另有按意思找的结果 |
| `lifelog summarize [<日期>]` | 生成每日回顾，`--dry-run` 先看要发出去的内容 |
| `lifelog-tui` | 终端界面：日、时间线、命令、网页、周、搜索 |
| `lifelog-mcp` | 给没有 shell 的 agent 用的 MCP 服务 |

日期可以写 `today`、`yesterday`、`3d`（3 天前）、`2026-11-12`。「一天」默认从凌晨 4 点开始，熬夜到两点算前一天。完整用法见[使用](docs/usage.md)。

## 记录哪些东西

| 来源 | 从哪里来 | 记什么 |
|---|---|---|
| 窗口 | niri | 应用、标题、工作区、持续多久 |
| 离开 | Wayland 空闲通知 | 多久没有键盘鼠标操作（默认 5 分钟） |
| 命令 | atuin 的历史库 | 命令、目录、耗时、退出码 |
| 网页 | Zen / Firefox、Chrome / Chromium、qutebrowser 的历史 | 标题、网址、profile |
| 笔记 | `~/lifelog` 下的 Markdown | 内容，用来检索 |

不记录键盘输入、屏幕内容、剪贴板。存在哪、怎么删、什么会离开本机，见[隐私](docs/privacy.md)。

## 文档

| | |
|---|---|
| [安装](docs/installation.md) | 依赖、安装、systemd、升级、卸载 |
| [使用](docs/usage.md) | 命令行、终端界面、JSON 输出、退出码 |
| [每日摘要、笔记和检索](docs/summaries.md) | 接模型、定时生成、手改、按意思找 |
| [给 agent 用](docs/agents.md) | Claude Code skill、MCP |
| [配置](docs/configuration.md) | 全部配置项和默认值 |
| [隐私](docs/privacy.md) | 记什么、不记什么、什么会离开本机 |
| [排错](docs/troubleshooting.md) | 常见问题 |
| [架构](docs/architecture.md) | 微内核 + 组件，怎么扩展、怎么替换 |
| [参与开发](CONTRIBUTING.md) · [更新记录](CHANGELOG.md) | |

## 架构一句话

kernel 只管生命周期、命名和消息传递；窗口管理器、数据库、模型、浏览器、命令行界面，以及每一个功能，都是通过契约通信的组件。加功能 = 加一个组件，换实现 = 换一个提供同一契约的组件。详见[架构](docs/architecture.md)。

## 现状

个人项目，0.1，目前在 Linux + niri 上开发和使用。窗口焦点目前只支持 niri；其他合成器需要写一个提供 `Windows` 契约的组件（[怎么做](docs/architecture.md#换一个实现)）。

还没有的：用新规则重新处理旧记录（`lifelog reprocess`）、外部命令作为数据源。

## 许可证

按 [MIT](LICENSE-MIT) 或 [Apache-2.0](LICENSE-APACHE) 任选其一使用，和 Rust 生态的惯例一样。

除非你另外声明，你提交的贡献按同样的双许可证发布。
