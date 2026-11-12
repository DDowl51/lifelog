# 隐私

lifelog 记录的是你在电脑上的一举一动，所以默认的设计是：**数据只在本机，入库之前先处理，离开本机的只有你明确开启的那一部分。**

## 记什么

| 来源 | 记什么 | 不记什么 |
|---|---|---|
| 窗口（niri） | 应用标识、窗口标题、所在工作区、开始和结束时刻 | 窗口里的内容、截图 |
| 离开（Wayland 空闲通知） | 什么时候开始、结束离开 | 键盘、鼠标的具体操作 |
| 命令（atuin） | 命令、工作目录、耗时、退出码 | 命令的输出 |
| 网页（浏览器历史） | 标题、网址、浏览器和 profile、访问时刻 | 页面内容、Cookie、表单 |
| 笔记（`~/lifelog`） | 你自己放进去的 Markdown | 这个目录以外的文件 |

不记录按键、剪贴板、屏幕、麦克风。命令和网页是从 atuin 和浏览器已经保存的历史里读的，lifelog 不会额外采集它们没有的东西。

## 存在哪

| 内容 | 位置 |
|---|---|
| 数据库（全部记录、索引、文档） | `~/.local/share/lifelog/lifelog.db` |
| 摘要和笔记 | `~/lifelog/` |
| 本机嵌入模型 | `~/.cache/lifelog/models/` |
| 进程之间的 socket | `$XDG_RUNTIME_DIR/lifelog/daemon.sock`（只有你自己能访问） |
| 配置 | `~/.config/lifelog/config.toml` |

数据库只是一个 SQLite 文件，可以用 `sqlite3` 打开查看。删掉它，就删掉了所有记录。

## 入库之前的处理

每条活动写进数据库之前，依次经过 `project`（归类）和 `privacy`（排除、脱敏）。脱敏后的原文不会写进数据库。

### 默认就开着的

- **只记时长**：密码管理器和输入密码的弹窗（KeePassXC、Bitwarden、1Password、gcr-prompter、pinentry），浏览器的隐私 / 无痕窗口。
- **只记时刻**：执行了 `pass`、`gopass`、`passage` 的命令。`pass show 银行/招行` 这种命令本身就暴露了密码库里有什么。
- **把密钥换成 `[redacted]`**：`sk-…`、GitHub / GitLab / Slack 的 token、AWS 和 Google 的 key、JWT、私钥文件头、`Bearer …`、`password=…`、`--token …`，以及网址里的 `user:pass@`。标题、命令、网址里都会处理。

### 自己加

```toml
[components.privacy]
exclude_commands = ["pass", "gopass", "passage", "bw", "op"]   # 再加 Bitwarden、1Password 的命令行
exclude = [
  { domain = "bank.example" },          # 这个网站及其子域名
  { app = "signal" },                   # 这个应用的窗口
  { source = "browsing", title = "(?i)病历" },
]
secrets = ['工号(?<secret>\d+)']        # 只换掉分组 secret 那部分
```

浏览器里别人也在用的 profile，可以整个不读：

```toml
[components.browser-chromium]
skip_profiles = ["家人"]
```

完整的规则写法见[配置 · privacy](configuration.md#privacy)。

### 规则只管之后的记录

改了规则之后，只有新写入的活动按新规则处理，已经在库里的记录不会跟着改。要清理旧记录，先停掉 `lifelogd`、备份数据库，再用 `sqlite3` 处理。

## 什么会离开本机

| 什么 | 什么时候 | 发给谁 | 怎么控制 |
|---|---|---|---|
| 一天的原料 | 生成每日摘要时 | 你在 `llm-openai-compat` 里配置的接口 | 不配置就不发；`lifelog summarize --dry-run` 先看；`summary.hidden_sources` 少发几类 |
| agent 查到的内容 | agent 调用 `lifelog` 或 `lifelog-mcp` 时 | 那个 agent 的模型服务商 | `query.agent_hidden_sources` 对 MCP 隐藏，`query.hidden_sources` 对所有程序隐藏，见[给 agent 用](agents.md#控制-agent-能看到什么) |
| 下载模型 | 第一次按意思检索时 | 从 Hugging Face 下载，不上传任何东西 | 关掉 `vectors` 和 `embed-fastembed` |

除此之外，lifelog 不联网，没有遥测，也不检查更新。

发给摘要模型的原料里只有网页的标题和域名，没有完整网址；被排除的活动只剩下时长。

## 只读的进程

`lifelog`、`lifelog-tui`、`lifelog-mcp` 以只读方式打开数据库，只有 `lifelogd` 写入。给 agent 用的说明（skill）也要求 agent 通过 `lifelog` 查询、不要直接读数据库文件，这样 `hidden_sources` 这类规则才会生效。
