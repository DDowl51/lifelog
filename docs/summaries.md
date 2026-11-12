# 每日摘要、笔记和检索

## 每日摘要

每天凌晨，`lifelogd` 把前一天的记录整理成一份原料，交给你配置的模型，写成一份 Markdown 回顾：

```markdown
<!-- lifelog: 由 deepseek-chat 生成，hash=53ac6293181aed3e。手改之后不会被自动覆盖；想重新生成就删掉这个文件，或者用 --force -->
# 2026-11-11 周三

## 概览
这一天主要在调气象站固件，下午整理了 lifelog 的配置……

## 按项目
- **weather-station**：反复烧录固件，排查串口权限……
- **lifelog**：……

## 时间线要点
- 09:02–09:48 固件环境配置、串口排查。
- ……

## 未完成 / 明天继续
- 串口权限刚配好，还要确认设备能稳定通信。
```

文件放在 `~/lifelog/daily/<日期>.md`（`files-std.root` + `summary.path`），同时存进文档库，可以被检索。

### 配置模型

任何 OpenAI 兼容的接口都可以（OpenAI、DeepSeek、本机的 Ollama / llama.cpp server……）：

```toml
[components.llm-openai-compat]
base_url = "https://api.deepseek.com/v1"
model = "deepseek-chat"
key_command = "pass show api/llm"
```

key 有三种给法，从好到差：

1. `key_command`：第一次调用模型时才运行。gpg 还没解锁也不影响 `lifelogd` 启动。
2. `api_key_env = "DEEPSEEK_API_KEY"`：从环境变量读。
3. `api_key_cmd = "…"`：启动时运行。命令失败的话，`lifelogd` 起不来。

不要把 key 直接写进配置文件。本机不要 key 的接口（Ollama 等）随便给一个占位的，比如 `api_key = "local"`。

### 生成

```bash
lifelog summarize --dry-run      # 先看要发出去什么
lifelog summarize                # 昨天的，再往回补最近 3 天缺的
lifelog summarize 2026-11-10
systemctl --user enable --now lifelog-summarize.timer   # 每天 04:30 自动跑
```

- 生成在 `lifelogd` 里做，`lifelog summarize` 只是通过本地 socket 发个请求。所以 `lifelogd` 要在运行，改了摘要和模型的配置要重启它。
- 在电脑前不到 5 分钟的天不写（`summary.min_active_minutes`）。
- 定时器错过了（比如关机），开机后会补上。

### 发出去什么

原料包括：每个项目的时长、较长的窗口时段、常用的命令、网页的标题和域名。不包括完整网址；被隐私规则排除的活动只剩下时长。原料最多 12000 字（`summary.max_digest_chars`）。

**每次都可以先用 `--dry-run` 看一遍。** 想少发一些：

```toml
[components.summary]
hidden_sources = ["browsing"]    # 不发网页；可选 focus、shell、browsing
```

也可以在 `privacy` 里加排除规则，那样连数据库里都只记时长。见[隐私](privacy.md)。

### 手改

摘要文件可以随便改。文件头里的 hash 是生成时内容的指纹：内容和它对不上，就说明是手改过的，之后不会被自动覆盖。改过的内容会同步回文档库，检索到的就是你改过的版本。

想重新生成：删掉文件，或者 `lifelog summarize <日期> --force`。

### 换提示词

```toml
[components.summary]
prompt = """
你是帮我回顾一天的助手……
"""
```

默认的提示词在 `features/summary/core/src/lib.rs` 的 `DEFAULT_PROMPT`，可以从那里抄一份来改。

## 笔记

`~/lifelog` 下的 `.md` 文件每 5 分钟同步一次进文档库：

- 第一个 `# ` 标题当作文档标题；文件名里有日期的，记下日期。
- 改了的重新同步，删了的从文档库删掉。
- `daily/<日期>.md` 是摘要，同步回那一天的摘要，不算笔记。

只想同步一部分目录：

```toml
[components.notes]
dirs = ["notes", "journal"]
```

## 检索

`lifelog search` 有两种结果：

1. **记录**：窗口标题、命令、网页的全文检索。每个词都要出现，最近的在前。中文按相邻两个字切，不需要词典。
2. **文档**：摘要和笔记，按相关度排列。

### 按意思找

文档还可以按意思找：搜「命令行历史记录工具」，能找到写着 atuin 的那份摘要，即使摘要里一个字都没对上。

- 用的是本机模型 `multilingual-e5-small`（约 470 MB），第一次用时下载到 `~/.cache/lifelog/models`。内容不出本机。
- 模型只在 `lifelogd` 里加载一次；`lifelog`、`lifelog-tui`、`lifelog-mcp` 通过本地 socket 用它。`lifelogd` 没在运行时只有全文检索。
- 结果和全文检索的合并在一起排序（RRF）。相似度低于 0.85 的不算命中（`vectors.min_score`）。
- 换更大的模型（`embed-fastembed.model = "bge-m3"`）要跟着调 `min_score`。
- 不需要的话，关掉 `vectors` 和 `embed-fastembed`，省下内存和磁盘。
