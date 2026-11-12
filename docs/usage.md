# 使用

## 日期和「一天」

所有接受日期的命令都可以这样写：

| 写法 | 意思 |
|---|---|
| `today`（默认） | 今天 |
| `yesterday` | 昨天 |
| `3d` | 3 天前 |
| `2026-11-12` | 这一天 |

「一天」从本地时间 04:00 开始（`query.day_boundary`），所以凌晨 1 点还在写代码，算前一天的事。

所有时长都已经扣掉了离开电脑的时间：窗口开着但人不在，不算。

## 命令行

### 一天花在哪

```console
$ lifelog today
2026-11-12 周四（04:00 起）  共 6 小时 12 分

  3 小时 40 分  ████████████████████  ghostty
  1 小时 55 分  ███████████           zen
         37 分  ███                   org.telegram.desktop
```

```bash
lifelog day yesterday --by title     # 按窗口标题
lifelog day 2026-11-01 --by project  # 按项目
lifelog week                         # 这一周，周一到周日
lifelog week 1d --by project
```

`--by` 可以是 `app`（默认）、`title`、`project`。

### 按时间顺序

```console
$ lifelog timeline
2026-11-12 周四（04:00 起）

  09:00–09:40         40 分  ghostty  nvim ~/work
                      其间：nvim 40 秒 · yazi 30 秒 · shell 20 秒 等 4 项
                      $ cargo test ×3
                      $ git status
                      … 还有 2 条命令
                      ↗ jiff - Rust  docs.rs
  09:50–10:00         10 分  （离开）
  10:00–11:05  1 小时 05 分  zen  Rust 程序设计语言
```

- 每一段下面列出这段时间里执行的命令（`$`）和看过的网页（`↗`）。
- 在几个窗口之间来回切的时候，不到 1 分钟的那些不单独列，并进前面那一段，写在「其间」里。
- 短于 10 秒的窗口不列，同一个窗口中间只隔了 1 分钟以内的合成一段。这三个阈值都可以配置（`query.timeline_*`）。

### 项目

```console
$ lifelog projects
2026-11-12 周四（04:00 起）  （另有离开 20 分）

  1 小时 05 分  lifelog     40 条命令 · 5 个网页
             ·  weather     3 条命令
         23 分  （未归类）
```

### 命令和网页

```bash
lifelog commands yesterday     # 时刻、耗时、退出码、目录、项目
lifelog visits                 # 时刻、标题、网址
```

`commands` 要装 atuin。

### 检索

```bash
lifelog search 气象站
lifelog search cargo release --source shell
lifelog search jif                          # 最后一个英文词按前缀匹配，能找到 jiff
lifelog search 部署 --since 7d --until yesterday --limit 50
```

- 每个词都要出现。中文不需要分词，`象站固` 也能找到「气象站固件调试」。
- 在窗口标题、命令、网页标题和网址里找，最近的在前；同一个东西出现多次的合成一条，带着次数。
- `--source` 只找一种：`focus`（窗口）、`shell`（命令）、`browsing`（网页）。
- 有匹配的摘要和笔记时，最前面单独一节「摘要和笔记」，按相关度排列；`lifelogd` 在运行时，还会加上按意思找到的（见[检索](summaries.md#按意思找)）。

### 每日摘要

```bash
lifelog summarize                 # 生成昨天的，再补最近几天缺的
lifelog summarize 2026-11-10      # 指定一天
lifelog summarize --dry-run       # 只印出要发给模型的内容
lifelog summarize --force         # 文件手改过也重新生成
```

要 `lifelogd` 在运行，而且配置好了模型，见[每日摘要](summaries.md)。

### 通用选项

| 选项 | 作用 |
|---|---|
| `--json` / `--text` | 输出格式。默认：stdout 是终端时输出文本，在管道里输出 JSON |
| `--color auto\|always\|never` | 文本要不要上色。设置了 `NO_COLOR` 时 `auto` 不上色 |
| `--config <文件>` | 用另一个配置文件 |
| `--trace` | 把组件之间的每次请求和事件按行写到 stderr，排错用 |

## 给程序读

在管道里，或者加了 `--json`，每个命令输出一行 JSON。字段只增不改，可以放心写脚本：

```bash
# 昨天用了 10 分钟以上的应用
lifelog day yesterday | jq -r '.rows[] | select(.total_ms > 600000) | .key'

# 这周每天在 lifelog 上的时间（分钟）
lifelog week --by project | jq '.rows[] | select(.key == "lifelog") | .days_ms | map(. / 60000 | floor)'
```

- 时长是毫秒；时刻是 UTC 毫秒，`tz_offset_min` 是当时的本地时区偏移。
- 被隐私规则排除的活动，标题是 `[redacted]`。
- `project` 是 `null` 表示没归上类。

### 退出码

遵循 `sysexits.h`：

| 退出码 | 意思 | 例子 |
|---|---|---|
| 0 | 成功 | |
| 64 | 用法不对 | 没有这个子命令、选项写错 |
| 65 | 输入不对 | 日期写错 |
| 69 | 不可用 | 数据库不存在（`lifelogd` 没运行过）、`summarize` 时 `lifelogd` 没在运行、没配置模型 |
| 75 | 暂时失败，可以重试 | 模型接口限流（429） |
| 77 | 没有权限 | 模型接口的 key 不对 |
| 78 | 配置错误 | 配置文件里键名写错 |

错误信息写到 stderr。

## 终端界面

```bash
lifelog-tui
```

| 按键 | 作用 |
|---|---|
| `d` | 日：按应用、按项目、按标题 |
| `t` | 时间线 |
| `c` | 命令 |
| `v` | 网页 |
| `w` | 周 |
| `s` | 搜索：按 `/` 输入，`Enter` 确定，`Esc` 取消 |
| `←` / `→` | 前一天 / 后一天（周页面是前一周 / 后一周） |
| `.` | 回到现在 |
| `↑` / `↓` | 滚动 |
| `q` | 退出 |

停在今天那一页时每 30 秒自动刷新（`tui-ratatui.refresh_secs`）。输入框打开时，按键都进输入框。
