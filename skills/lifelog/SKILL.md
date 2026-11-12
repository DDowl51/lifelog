---
name: lifelog
description: 查用户在这台电脑上做过什么：哪天在哪个应用、窗口、项目上花了多久，按时间顺序做了什么，执行过哪些命令，看过哪些网页，以及在这些记录里检索。用户问「我昨天 / 上周 / 那天做了什么」「我在这个项目上花了多久」「我之前在哪看过 / 跑过 X」「那条命令是什么来着」时使用。
---

# lifelog：查用户做过什么

`lifelogd` 在后台记录窗口焦点、离开电脑的时段、shell 命令（atuin）、浏览器访问（Zen、Chrome、qutebrowser），入库前已经脱敏、归到项目。用 `lifelog` 命令只读地查。

## 怎么查

在管道里（或加 `--json`）输出是一行 JSON，结构稳定、字段只增不改。优先用命令行；没有 shell 时用 MCP 工具 `lifelog_*`（参数同下）。

| 想知道 | 命令 |
|---|---|
| 某天在哪些应用 / 标题 / 项目上花了多久 | `lifelog day [<日期>] --by app\|title\|project --json` |
| 某天按时间顺序做了什么（每段带着期间的命令和网页） | `lifelog timeline [<日期>] --json` |
| 某天每个项目：时长、命令数、网页数 | `lifelog projects [<日期>] --json` |
| 一周每天的分布 | `lifelog week [<日期>] --by app\|project --json` |
| 某天执行过的命令 | `lifelog commands [<日期>] --json` |
| 某天访问过的网页 | `lifelog visits [<日期>] --json` |
| 在所有记录里找 | `lifelog search <词…> [--since <日期>] [--until <日期>] [--source focus\|shell\|browsing] [--limit N] --json` |

日期：`today`（默认）、`yesterday`、`3d`（3 天前）、`2026-11-12`。按逻辑日解释：默认凌晨 4 点换日，凌晨 1 点的事算前一天。

## 读结果时注意

- 时长单位是毫秒，已经扣掉了离开电脑的时间。时间是 UTC 毫秒，`tz_offset_min` 是当时的本地偏移。
- `title` 是 `[redacted]` 的是被隐私规则排除的（密码管理器、隐私窗口、指定的网站），不要试图还原。
- `project` 为空表示没归上类，不代表没在做项目。
- 检索：每个词都要出现，最后一个英文词按前缀匹配；结果最近的在前，同一个东西多次出现合成一条（`count`）。没找到时换个说法、去掉一个词再试。
- 退出码遵循 sysexits：65 参数不对（比如日期写错），69 数据库不可用（`lifelogd` 没在跑或没装）。

## 不要做

- 不要直接读数据库文件（`~/.local/share/lifelog/lifelog.db`）：用户配置的隐藏规则只在 `lifelog` 里执行。
- 不要把查到的内容原样大段贴给用户；先回答用户的问题，需要时再引用几条。
