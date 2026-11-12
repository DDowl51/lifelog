# 安装

## 需要什么

| | 必需 | 说明 |
|---|---|---|
| Linux + [niri](https://github.com/YaLTeR/niri) | 是 | 窗口焦点从 niri 的 IPC 读。别的合成器目前不支持 |
| Wayland 的 `ext-idle-notify-v1` | 是 | 判断离开电脑。niri 支持 |
| Rust（stable，edition 2024） | 是 | 从源码编译。SQLite 已经打包在内，不用另装 |
| systemd 用户服务 | 推荐 | 让 `lifelogd` 跟着图形会话启动 |
| [atuin](https://atuin.sh) | 否 | 有它才能记录 shell 命令 |
| Zen / Firefox、Chrome / Chromium、qutebrowser | 否 | 装了哪个就读哪个的历史，没装的跳过 |
| OpenAI 兼容的模型接口 | 否 | 每日摘要用。不配置就不生成 |
| 约 500 MB 磁盘 | 否 | 按意思检索用的本机模型，第一次用时下载 |

## 安装

```bash
git clone https://github.com/DDowl51/lifelog lifelog && cd lifelog
scripts/install.sh
```

`install.sh` 做这几件事：

1. `cargo install` 四个程序到 `~/.cargo/bin`：
   - `lifelogd`：常驻进程，负责记录，唯一写数据库的进程；
   - `lifelog`：命令行，只读；
   - `lifelog-tui`：终端界面，只读；
   - `lifelog-mcp`：给 agent 的 MCP 服务，只读。
2. 把 `dist/` 里的 systemd 用户服务装到 `~/.config/systemd/user/`。只是放好，不会自动启用。
3. 把给 Claude Code 的 skill 放到 `~/.claude/skills/lifelog/`。

## 开始记录

```bash
systemctl --user enable --now lifelogd
journalctl --user -u lifelogd -f      # 看日志；正常运行时几乎没有输出
```

`lifelogd` 跟着图形会话启动，崩溃了 systemd 会把它拉起来。过几分钟就能看到记录：

```bash
lifelog today
```

## 推荐的几步配置

配置文件是 `~/.config/lifelog/config.toml`，可以不存在。改完要重启 `lifelogd` 才生效，见[配置](configuration.md)。

### 归到项目

```toml
[components.project]
roots = ["~/work/projects"]          # 下面每个子目录是一个项目
```

窗口标题里、命令的工作目录、网址里出现 `~/work/projects/<项目>` 的活动，会归到那个项目。fish 缩写成的
`~/w/p/<项目>` 也认得。还可以按 niri 工作区名、浏览器 profile 归类，见[配置 · project](configuration.md#project)。

### 记录 shell 命令

装好 atuin，把它接进 shell（`atuin init`），再把已有的历史导入进去：

```bash
atuin import auto
```

`lifelogd` 每分钟读一次 atuin 的库，第一次会把已有的历史一起导入。

### 每日摘要

```toml
[components.llm-openai-compat]
base_url = "https://api.deepseek.com/v1"
model = "deepseek-chat"
key_command = "pass show api/llm"   # 第一次调用时运行，输出就是 key
```

```bash
systemctl --user restart lifelogd
lifelog summarize --dry-run          # 先看看要发给模型的内容
lifelog summarize                    # 生成昨天的
systemctl --user enable --now lifelog-summarize.timer   # 以后每天 04:30 自动生成
```

摘要会发到你配置的模型服务商那里，发什么、怎么少发一些，见[每日摘要](summaries.md)和[隐私](privacy.md)。

### 给 agent 用

Claude Code 的 skill 已经装好了。没有 shell 的客户端用 MCP：

```bash
claude mcp add lifelog -- lifelog-mcp
```

见[给 agent 用](agents.md)。

## 升级

```bash
git pull
scripts/install.sh
systemctl --user restart lifelogd
```

数据库的结构由各个组件自己迁移，启动时自动完成，不用手动操作。老版本不认识新加的配置键，会在启动时报错，所以要先升级，再在配置里用新的键。

## 卸载

```bash
systemctl --user disable --now lifelogd lifelog-summarize.timer
rm ~/.config/systemd/user/{lifelogd.service,lifelog-summarize.service,lifelog-summarize.timer}
systemctl --user daemon-reload
cargo uninstall lifelogd lifelog lifelog-tui lifelog-mcp
rm -r ~/.claude/skills/lifelog
claude mcp remove lifelog            # 如果加过
```

数据不会被删掉。想一起删：

```bash
rm -r ~/.local/share/lifelog         # 数据库
rm -r ~/.cache/lifelog               # 模型
rm -r ~/.config/lifelog              # 配置
# ~/lifelog 里是摘要和你自己的笔记，确认之后再删
```
