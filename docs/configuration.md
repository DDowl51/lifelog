# 配置

全部配置项、默认值和含义。每个组件的默认值都写成一个 TOML 块；这些块和代码里的默认值是否一致，由
`tests/config-doc` 检查。改了配置项却没改这里，`scripts/check.sh` 就会失败。

## 文件

- 位置：`$LIFELOG_CONFIG` → `$XDG_CONFIG_HOME/lifelog/config.toml` → `~/.config/lifelog/config.toml`，
  也可以用 `--config <文件>` 指定。四个程序（`lifelogd`、`lifelog`、`lifelog-tui`、`lifelog-mcp`）读同一个文件。
- 文件可以不存在，等于全部用默认值。只写想改的键就行，没写的键取下面的默认值。
- 一段 `[kernel]`，每个组件一段 `[components.<组件 id>]`。

## 通用规则

| 规则 | 说明 |
|---|---|
| `enabled = false` | 每个组件都可以关掉。必须依赖它的组件会在启动时报错；可选依赖它的组件跳过那部分功能 |
| 写错键名 | 启动时报错，不会被悄悄忽略 |
| 写错组件 id | 启动时报错，并列出所有组件 id |
| 别的进程的组件 | 比如 `lifelog` 读到 `[components.wm-niri]`，直接忽略（它不在这个进程里） |
| `<键>_cmd = "命令"` | 任何键都可以这样写：启动时用 `sh -c` 运行命令，把输出（去掉末尾换行）当作 `<键>` 的值。密钥因此不用写进配置文件。`<键>` 和 `<键>_cmd` 不能同时写；关掉的组件不运行 |
| `~` | 路径里开头的 `~` 换成主目录 |
| 不写的键 | 下面的块里注释掉的键（`# path = …`）默认不写，不写时另有规则，见那一节的说明 |

`lifelog`、`lifelog-tui`、`lifelog-mcp` 总是以只读方式打开数据库（启动器把 `storage-sqlite.read_only`
设成 `true`），配置里写什么都一样。

### 哪个组件在哪个进程里

| 进程 | 组件 |
|---|---|
| `lifelogd`（常驻，记录） | 采集、入库处理、检索索引、摘要、笔记，以及它们用到的系统服务 |
| `lifelog`（命令行） | `cli-clap`、`query`、`search`、`documents`、`summary-cli`，`summary-remote` 和 `vectors-remote` 转给 `lifelogd` |
| `lifelog-tui`（终端界面） | `tui-ratatui`、`query`、`search`、`documents`，`vectors-remote` 转给 `lifelogd` |
| `lifelog-mcp`（给 agent） | `mcp-stdio`、`query`、`search`、`documents`，`vectors-remote` 转给 `lifelogd` |

`query`、`search` 这类几个进程都有的组件，各进程读的是同一段配置。

## kernel

```toml
[kernel]
queue_capacity = 1024
stop_timeout_secs = 5
```

| 键 | 说明 |
|---|---|
| `queue_capacity` | 事件队列能排多少条还没投递的事件。满了发布方会等 |
| `stop_timeout_secs` | 退出时等每个组件收尾多久，超时就不等了 |

## 记录

### wm-niri

从 niri 读窗口焦点和工作区。

```toml
[components.wm-niri]
# socket = "/run/user/1000/niri.….sock"
reconnect_initial_ms = 1000
reconnect_max_ms = 60000
```

| 键 | 说明 |
|---|---|
| `socket` | niri 的 socket。不写就用环境变量 `NIRI_SOCKET` |
| `reconnect_initial_ms` | 断线后第一次重连前等多久，之后每次翻倍 |
| `reconnect_max_ms` | 重连等待的上限 |

### focus

把窗口焦点变化记成「在哪个窗口上待了多久」。

```toml
[components.focus]
flush_interval_secs = 30
title_suffixes = [" — Zen Browser", " — Mozilla Firefox", " - Google Chrome", " - Chromium"]
title_rewrites = [
  { pattern = '^(?<file>.+?) \d+:\d+ .* - \S+@\[[^\]]*\] - Kakoune$', replace = '${file} - Kakoune' },
]
```

| 键 | 说明 |
|---|---|
| `flush_interval_secs` | 还没结束的时段每隔多久写一次。崩溃时最多丢这么长 |
| `title_suffixes` | 去掉标题末尾的应用名。按顺序试，只去掉第一个匹配的 |
| `title_rewrites` | 标题改写规则，按顺序各替换一次。`pattern` 是正则，`replace` 里可以用 `$1`、`${名字}` 引用分组。用来去掉标题里一直在变、但不代表换了一件事的部分。默认这条把 kakoune 带状态栏的标题改成「文件 - Kakoune」 |

改写和去后缀之后标题相同的，算同一个窗口时段。

### idle-wayland

用 Wayland 的空闲通知判断人在不在电脑前。

```toml
[components.idle-wayland]
timeout_secs = 300
```

| 键 | 说明 |
|---|---|
| `timeout_secs` | 多少秒没有键盘鼠标操作算离开 |

### afk

把离开的时段记下来。统计在电脑前的时长时会扣掉这些时段。

```toml
[components.afk]
flush_interval_secs = 30
```

| 键 | 说明 |
|---|---|
| `flush_interval_secs` | 还没结束的离开时段每隔多久写一次 |

### shell-atuin

从 atuin 的历史库读执行过的命令。

```toml
[components.shell-atuin]
# path = "~/.atuin/data/history.db"
all_hosts = false
running_grace_secs = 21600
```

| 键 | 说明 |
|---|---|
| `path` | atuin 的历史库。不写时按 atuin 自己的规则找，用第一个存在的：`$ATUIN_HOME/data/history.db`、`~/.atuin/data/history.db`、`$XDG_DATA_HOME/atuin/history.db`、`~/.local/share/atuin/history.db` |
| `all_hosts` | atuin 会同步别的机器的历史。默认只读这台机器的，为 `true` 时全部都读 |
| `running_grace_secs` | 还在执行的命令（还没有耗时）最多等多久，过了就当它已经结束，照样记下来 |

### shell

把命令记成活动。

```toml
[components.shell]
poll_secs = 60
batch = 500
max_batches = 20
```

| 键 | 说明 |
|---|---|
| `poll_secs` | 多久看一次有没有新命令 |
| `batch` | 一次读多少条 |
| `max_batches` | 一轮最多读几次。第一次导入大量历史时分几轮做完，不长时间占着事件投递 |

### browser-firefox

读 Firefox 系浏览器（Zen、Firefox）的历史。先把库复制出来再读，不锁浏览器正在用的文件。

```toml
[components.browser-firefox]
installs = [{ name = "zen", dir = "~/.zen" }, { name = "firefox", dir = "~/.mozilla/firefox" }]
```

| 键 | 说明 |
|---|---|
| `installs` | 每项是一个浏览器：`name` 是记录里的浏览器名，`dir` 是 `profiles.ini` 所在的目录。不存在的目录跳过 |

### browser-chromium

读 Chrome 系浏览器的历史。

```toml
[components.browser-chromium]
installs = [{ name = "chrome", dir = "~/.config/google-chrome" }, { name = "chromium", dir = "~/.config/chromium" }]
skip_profiles = []
```

| 键 | 说明 |
|---|---|
| `installs` | 每项是一个浏览器：`name` 是记录里的浏览器名，`dir` 是 `Local State` 所在的目录。不存在的目录跳过 |
| `skip_profiles` | 不读的 profile，写显示名或目录名（`Default`、`Profile 3`）。别人也在用的 profile 放在这里 |

### browser-qutebrowser

读 qutebrowser 的历史，包括用 `--basedir` 开的独立实例。

```toml
[components.browser-qutebrowser]
data_dir = "~/.local/share/qutebrowser"
basedirs = ["~/.local/share/qutebrowser-profiles"]
```

| 键 | 说明 |
|---|---|
| `data_dir` | 默认实例的数据目录。不存在就跳过 |
| `basedirs` | 放独立实例的目录：每个子目录是一个实例，子目录名就是 profile 名。不存在的跳过 |

### browser-history

把上面几个浏览器组件汇总成一个历史来源。没有配置项。

```toml
[components.browser-history]
```

### browsing

把网页访问记成活动。

```toml
[components.browsing]
poll_secs = 300
batch = 1000
max_batches = 20
```

| 键 | 说明 |
|---|---|
| `poll_secs` | 多久看一次。每次都要复制浏览器的库（几十 MB），不要太频繁 |
| `batch` | 一次读多少条 |
| `max_batches` | 每个来源一轮最多读几次。第一次导入大量历史时分几轮做完 |

## 入库处理

活动写进数据库之前，依次经过各个处理器。

### activity

活动的写入和读取，以及处理器的先后。

```toml
[components.activity]
processors = ["project", "privacy"]
```

| 键 | 说明 |
|---|---|
| `processors` | 处理器的执行顺序。启用了的处理器都要列在这里，没列的启动时报错；列了但没启用的跳过。默认先归类（要看原始的路径和标题），最后脱敏（之后没有谁再往里写文字） |

### project

给活动归到项目。

```toml
[components.project]
roots = []
rules = []
```

| 键 | 说明 |
|---|---|
| `roots` | 项目根，例如 `["~/work/projects"]`：下面每个子目录是一个项目。窗口标题、命令的工作目录、网址里出现这些路径就归到对应项目 |
| `rules` | 归类规则，比路径先试，按顺序用第一条命中的。每条规则的条件见下表，写了的条件都要满足 |

规则的字段：

| 字段 | 说明 |
|---|---|
| `app` | 应用标识，正则 |
| `title` | 标题，正则 |
| `workspace` | 窗口所在 niri 工作区的名字，正则 |
| `profile` | 浏览器 profile 或独立实例的名字，正则 |
| `project` | 命中时归到哪个项目。不写就用正则里名为 `project` 的分组 |

```toml
# 例子：用命名工作区和每个项目一个的 qutebrowser 实例归类
[components.project]
roots = ["~/work/projects"]
rules = [
  { workspace = '^work-(?<project>.+)$' },
  { app = '^qutebrowser$', profile = '^(?<project>.+)$' },
  { app = '^qutebrowser-(?<project>.+)$' },
]
```

### privacy

入库之前排除和脱敏。

```toml
[components.privacy]
builtin_rules = true
exclude = []
secrets = []
exclude_commands = ["pass", "gopass", "passage"]
```

| 键 | 说明 |
|---|---|
| `builtin_rules` | 是否带上内置规则：密码管理器和输密码的弹窗（KeePassXC、Bitwarden、1Password、gcr-prompter、pinentry）、浏览器隐私窗口，以及常见的密钥格式（`sk-…`、GitHub/GitLab/Slack token、AWS/Google key、JWT、私钥文件头、`Bearer …`、`password=…`、`--token …`、网址里的 `user:pass@`） |
| `exclude` | 额外的排除规则。命中的活动只记时刻和时长，标题、网址、命令等内容都换成 `[redacted]`。字段见下表，写了的条件都要满足 |
| `secrets` | 额外的密钥正则，命中的部分换成 `[redacted]`。有名为 `secret` 的分组时只换掉那一组 |
| `exclude_commands` | 执行了这些命令时只记时刻，不记命令内容。写命令名（不能有空格）。命令开头，或者管道、`;`、`&&`、`$( )`、反引号之后调用的都算，前面可以有 `sudo`。默认是命令行的密码管理器：`pass show 银行/招行` 这样的命令本身就暴露了密码库里有什么。`[]` = 不屏蔽。和 `builtin_rules` 无关，各管各的 |

排除规则的字段：

| 字段 | 说明 |
|---|---|
| `source` | 来源（`focus`、`afk`、`shell`、`browsing`），完全相等 |
| `app` | 应用标识，正则 |
| `title` | 标题，正则 |
| `domain` | 网址的域名：等于它，或是它的子域名 |

```toml
# 例子：再屏蔽 Bitwarden 命令行和网银
[components.privacy]
exclude_commands = ["pass", "gopass", "passage", "bw"]
exclude = [{ domain = "bank.example" }, { app = "signal" }]
```

规则只对之后写入的活动生效。已经在库里的记录不会跟着改。

## 查看

### query

按天、时间线、按周、按项目的统计，以及给界面和 agent 用的查询。

```toml
[components.query]
day_boundary = "04:00"
hidden_sources = []
agent_hidden_sources = []
timeline_min_secs = 10
timeline_merge_gap_secs = 60
timeline_absorb_secs = 60
```

| 键 | 说明 |
|---|---|
| `day_boundary` | 一天从本地时间几点开始，`HH:MM`。熬夜到两点算前一天 |
| `hidden_sources` | 不展示的来源，比如 `["afk"]`。只是不显示，离开的时间照样扣 |
| `agent_hidden_sources` | 只对 agent（MCP 工具）额外隐藏的来源。agent 读到的内容会发到它的模型服务商那里 |
| `timeline_min_secs` | 时间线里短于这么久的窗口段不列出来 |
| `timeline_merge_gap_secs` | 时间线里同一个窗口中间只隔了这么久以内的，合成一项 |
| `timeline_absorb_secs` | 时间线里短于这么久的项并进前面的长项，列在「其间」里。`0` = 逐项列出 |

### cli-clap

命令行界面（`lifelog`）。没有配置项。

```toml
[components.cli-clap]
```

### tui-ratatui

终端界面（`lifelog-tui`）。

```toml
[components.tui-ratatui]
refresh_secs = 30
```

| 键 | 说明 |
|---|---|
| `refresh_secs` | 停在今天那一页时，每隔多久自动刷新。`0` = 不自动刷新 |

### mcp-stdio

给 agent 用的 MCP 服务（`lifelog-mcp`），走标准输入输出。

```toml
[components.mcp-stdio]
# name = "lifelog-mcp"
# instructions = "…"
```

| 键 | 说明 |
|---|---|
| `name` | 告诉客户端的服务名。不写就用程序名 |
| `instructions` | 握手时给客户端的说明（什么时候用这些工具）。不写就不给 |

## 检索

### search

全文检索：活动和文档（摘要、笔记）。

```toml
[components.search]
index_secs = 30
batch = 2000
max_batches = 25
```

| 键 | 说明 |
|---|---|
| `index_secs` | 多久给新活动建一次索引。只有 `lifelogd` 建索引，其他进程只查 |
| `batch` | 一次索引多少条 |
| `max_batches` | 一轮最多索引几批。第一次建索引时分几轮做完 |

### tokenizer-cjk

全文检索用的分词：中日韩文字按字切，其余按词。没有配置项。

```toml
[components.tokenizer-cjk]
```

### vectors

按意思检索文档：把文档切块、算向量。`lifelog search` 的结果里「相关文档」一节来自这里。

```toml
[components.vectors]
index_secs = 60
chunk_chars = 500
min_score = 0.85
batch = 50
```

| 键 | 说明 |
|---|---|
| `index_secs` | 多久看一次有没有新文档要算向量 |
| `chunk_chars` | 一块最多多少字 |
| `min_score` | 相似度低于这个的不算命中。不同模型的相似度范围不一样，换模型时要跟着调 |
| `batch` | 一轮最多处理几篇文档 |

### embed-fastembed

本机的嵌入模型，内容不出本机。第一次用时下载模型。

```toml
[components.embed-fastembed]
model = "multilingual-e5-small"
cache_dir = "~/.cache/lifelog/models"
```

| 键 | 说明 |
|---|---|
| `model` | `multilingual-e5-small`（约 470 MB）、`multilingual-e5-base`、`bge-m3`（约 2.3 GB）。换模型后要跟着调 `vectors.min_score` |
| `cache_dir` | 模型放在哪 |

不想要按意思检索，关掉 `vectors` 和 `embed-fastembed` 即可，全文检索不受影响。

## 摘要和笔记

### summary

每天写一份摘要：把那天的活动整理成原料，交给模型，写成 Markdown 文件。

```toml
[components.summary]
hidden_sources = []
prompt = "…"
path = "daily/{date}.md"
catch_up_days = 3
max_digest_chars = 12000
max_tokens = 1500
min_active_minutes = 5
```

| 键 | 说明 |
|---|---|
| `hidden_sources` | 不发给模型的来源（`focus`、`shell`、`browsing`）。发出去的内容会到模型服务商那里 |
| `prompt` | 给模型的说明。默认的很长，见 `features/summary/core/src/lib.rs` 里的 `DEFAULT_PROMPT` |
| `path` | 摘要文件放在哪（`files-std` 的根目录下），`{date}` 换成日期 |
| `catch_up_days` | 不给日期时，往回补几天还没有摘要的（含昨天） |
| `max_digest_chars` | 原料最多多少字，超过的部分截掉 |
| `max_tokens` | 摘要最多多少 token |
| `min_active_minutes` | 一天在电脑前少于这么多分钟就不写 |

手改过的摘要文件不会被自动覆盖。

### summary-cli

`lifelog summarize` 命令。没有配置项。

```toml
[components.summary-cli]
```

### llm-openai-compat

OpenAI 兼容接口的模型，摘要用它。

```toml
[components.llm-openai-compat]
base_url = "https://api.openai.com/v1"
# model = "gpt-…"
# api_key = "sk-…"
# api_key_env = "OPENAI_API_KEY"
# key_command = "pass show openai/api-key"
timeout_secs = 180
max_tokens = 2000
temperature = 0.3
```

| 键 | 说明 |
|---|---|
| `base_url` | 接口地址，到 `/v1` 为止。DeepSeek 是 `https://api.deepseek.com/v1` |
| `model` | 模型名。不写时调用会报错 |
| `api_key` | key 本身。不要直接写进文件，用 `api_key_cmd`（启动时运行）或下面两种 |
| `api_key_env` | 从这个环境变量读 key |
| `key_command` | 第一次调用模型时才运行这条命令（`sh -c`），输出就是 key。和 `api_key_cmd` 的区别：gpg 还没解锁也不影响 `lifelogd` 启动 |
| `timeout_secs` | 一次请求最多等多久 |
| `max_tokens` | 调用方没指定时，最多生成多少 token |
| `temperature` | 越低越稳定 |

```toml
# 例子：DeepSeek，key 放在 pass 里
[components.llm-openai-compat]
base_url = "https://api.deepseek.com/v1"
model = "deepseek-chat"
key_command = "pass show api/llm"
```

### notes

把文件根目录下的 Markdown 笔记同步成文档，可以检索；摘要文件改了也同步回去。

```toml
[components.notes]
dirs = [""]
poll_secs = 300
summary_path = "daily/{date}.md"
```

| 键 | 说明 |
|---|---|
| `dirs` | 文件根目录下的哪些目录，`""` 是整个根目录 |
| `poll_secs` | 多久看一次 |
| `summary_path` | 摘要文件的格式，要和 `summary.path` 一致：这些文件同步回那一天的摘要，不当作笔记 |

### documents

文档（摘要、笔记）的存取。没有配置项。

```toml
[components.documents]
```

## 系统服务

### storage-sqlite

SQLite 数据库。

```toml
[components.storage-sqlite]
# path = "~/.local/share/lifelog/lifelog.db"
read_only = false
busy_timeout_ms = 5000
```

| 键 | 说明 |
|---|---|
| `path` | 数据库文件，`:memory:` 是内存数据库。不写就用 `$XDG_DATA_HOME/lifelog/lifelog.db`（通常是 `~/.local/share/lifelog/lifelog.db`） |
| `read_only` | 只读打开：不建文件、不执行迁移、拒绝写入。只查询的进程由启动器设成 `true` |
| `busy_timeout_ms` | 别的连接占着锁时最多等多久 |

### files-std

文件读写，摘要和笔记放在这里。

```toml
[components.files-std]
root = "~/lifelog"
```

| 键 | 说明 |
|---|---|
| `root` | 根目录。不存在时第一次写入会建出来 |

### log-stderr

日志写到 stderr，`lifelogd` 的日志因此在 `journalctl --user -u lifelogd` 里。

```toml
[components.log-stderr]
level = "info"
```

| 键 | 说明 |
|---|---|
| `level` | 低于这个级别的不写：`debug`、`info`、`warn`、`error` |

### clock-system

系统时钟和时区。没有配置项。

```toml
[components.clock-system]
```

### timer-tokio

定时器。没有配置项。

```toml
[components.timer-tokio]
```

### summary-remote、vectors-remote

`lifelog` 等进程里的转发组件：把请求经 `$XDG_RUNTIME_DIR/lifelog/daemon.sock` 转给 `lifelogd`，模型和
数据库写入都留在常驻进程里。它们不是单独的包，不能写配置段；`lifelogd` 没在运行时，用到它们的命令会报错。
