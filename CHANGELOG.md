# 更新记录

格式参照 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)。

## [未发布]

还没有发布过版本。下面是到现在为止的全部内容，发布时会成为 0.1.0。

按 MIT 或 Apache-2.0 双许可证发布。

### 记录

- 窗口焦点时段（niri）：去掉标题里一直在变的部分，比如 spinner、未读数、浏览器名后缀、kakoune 的光标位置。
- 离开电脑的时段（Wayland 空闲通知），所有时长都扣掉离开的时间。
- shell 命令（atuin）：命令、目录、耗时、退出码，第一次启动时导入已有的历史。
- 浏览历史：Zen / Firefox、Chrome / Chromium、qutebrowser（包括 `--basedir` 实例），可以跳过指定的 profile。
- 笔记：`~/lifelog` 下的 Markdown 同步进文档库。

### 处理

- 归到项目：按项目根目录、niri 工作区名、浏览器 profile、正则规则。
- 隐私：密码管理器和隐私窗口只记时长；`pass`、`gopass`、`passage` 命令只记时刻（`privacy.exclude_commands`）；常见的密钥格式换成 `[redacted]`；排除规则和密钥正则都可以自己加。

### 查看

- `lifelog today` / `day` / `week`，可以按应用、标题、项目统计。
- `lifelog timeline`：按时间顺序，每段带着期间的命令和网页，短暂切换的窗口并进前面那一段。
- `lifelog projects`、`commands`、`visits`。
- `lifelog-tui`：日、时间线、命令、网页、周、搜索。
- 管道里输出 JSON，退出码遵循 sysexits，`--trace` 能看到组件之间的消息。

### 检索

- `lifelog search`：窗口、命令、网页的全文检索，中文按相邻两个字切，不需要词典。
- 摘要和笔记可以按意思找（本机 `multilingual-e5-small`），和全文检索的结果合并。

### 摘要

- `lifelog summarize`：每天把前一天的记录交给 OpenAI 兼容的接口，写成 `~/lifelog/daily/<日期>.md`；`--dry-run` 先看要发出去什么；手改过的不覆盖。
- systemd 定时器，每天 04:30 生成，自动补上缺的几天。

### 给 agent

- Claude Code skill。
- `lifelog-mcp`：7 个 MCP 工具；`query.agent_hidden_sources` 可以只对 agent 隐藏一些来源。

### 架构

- 微内核：kernel 只有机制，32 个组件通过契约通信，每个组件都能单独配置、启用或关掉。
- 跨进程：`lifelogd` 导出契约，其他进程通过 `contract!` 生成的代理调用。
- 配置文档和代码里的默认值由测试保证一致（`tests/config-doc`）。
