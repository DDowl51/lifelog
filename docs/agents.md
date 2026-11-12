# 给 agent 用

让 Claude Code 之类的 agent 能回答「我昨天做了什么」「那条命令是什么来着」「我在这个项目上花了多久」。

有两种接法，查到的内容一样：

| | 适合 | 怎么接 |
|---|---|---|
| skill + 命令行 | 能执行 shell 的 agent（Claude Code） | `install.sh` 已经放好 |
| MCP | 不能执行 shell 的客户端 | `lifelog-mcp` |

## Claude Code skill

`scripts/install.sh` 把 `skills/lifelog/SKILL.md` 放到 `~/.claude/skills/lifelog/`。里面告诉 agent：

- 什么时候用（问到「我做过什么」「花了多久」「之前在哪看过」）；
- 用哪个命令、加 `--json`；
- 怎么读结果：时长是毫秒、`[redacted]` 不要试图还原、`project` 为空不代表没在做项目；
- 不要直接读数据库文件。

装好之后直接问就行：

> 我上周在 weather-station 上花了多少时间？主要在干什么？

## MCP

```bash
claude mcp add lifelog -- lifelog-mcp
```

其他客户端按它们的格式配置一个 stdio 服务，命令是 `lifelog-mcp`，不需要参数：

```json
{
  "mcpServers": {
    "lifelog": { "command": "lifelog-mcp" }
  }
}
```

### 工具

| 工具 | 作用 | 参数 |
|---|---|---|
| `lifelog_day` | 一天在各个应用 / 标题 / 项目上的时长 | `date`、`by` |
| `lifelog_timeline` | 按时间顺序，每段带着期间的命令和网页 | `date` |
| `lifelog_projects` | 每个项目的时长、命令数、网页数 | `date` |
| `lifelog_week` | 一周每天的分布 | `date`、`by` |
| `lifelog_commands` | 执行过的命令 | `date`、`limit`（默认 100） |
| `lifelog_visits` | 访问过的网页 | `date`、`limit`（默认 100） |
| `lifelog_search` | 在所有记录里找，加上摘要和笔记 | `query`（必填，空格分开的词）、`since`、`until`、`source`、`limit`（默认 30） |

日期的写法和命令行一样（`yesterday`、`3d`、`2026-11-12`）。结果和 `lifelog … --json` 是同一份 JSON，只是限制了规模，
免得一次塞满 agent 的上下文：很长的网址截到 200 字；列表截断时带上 `"truncated": true` 和总数 `total`。

## 控制 agent 能看到什么

agent 读到的内容会发给它的模型服务商。

| 想要 | 怎么配 | 管得到 |
|---|---|---|
| 只对 MCP 隐藏一些来源，自己用命令行照常能看 | `query.agent_hidden_sources = ["browsing"]` | `lifelog-mcp` |
| 谁都看不到（包括你自己和通过 skill 调命令行的 agent） | `query.hidden_sources = ["browsing"]` | 所有程序 |
| 根本不记录 | `privacy` 的排除规则 | 数据库 |

通过 skill 的 agent 调用的就是 `lifelog` 命令，和你自己敲的没有区别，所以 `agent_hidden_sources` 管不到它。
skill 要求 agent 不要直接读数据库文件，这样 `hidden_sources` 和隐私规则才算数。见[隐私](privacy.md)。

## 自定义 MCP 服务的说明

```toml
[components.mcp-stdio]
name = "lifelog"
instructions = "用户问到自己过去在电脑上做过什么时，用这些工具查。"
```
