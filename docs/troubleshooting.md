# 排错

先看这三样：

```bash
systemctl --user status lifelogd
journalctl --user -u lifelogd -n 50
lifelog today --trace 2>trace.log     # 组件之间的每次请求和事件，一行一条
```

## 没有记录

**`lifelog` 报「打不开数据库 …它由可写的进程（常驻进程）创建」，退出码 69。** `lifelogd` 还没运行过，数据库还没建出来：

```bash
systemctl --user enable --now lifelogd
```

**`lifelogd` 在运行，但窗口一条都没有。** 多半是 systemd 的环境里没有 `NIRI_SOCKET`。用 `niri-session` 启动的
会话会把它导入 systemd；别的方式启动的话：

```bash
systemctl --user import-environment NIRI_SOCKET WAYLAND_DISPLAY
systemctl --user restart lifelogd
```

或者在配置里写死 `[components.wm-niri] socket = "…"`。

**离开的时间没扣掉。** 和上面一样，`idle-wayland` 需要 `WAYLAND_DISPLAY`。

**没有命令。**

- atuin 接进 shell 了吗？`atuin history list | tail` 应该能看到最近的命令。
- 历史库不在默认位置的话，配置 `shell-atuin.path`。
- 同步了好几台机器的历史？默认只读这台机器的（`shell-atuin.all_hosts`）。
- 新命令最多 1 分钟后才会出现（`shell.poll_secs`）。

**没有网页。**

- 默认每 5 分钟读一次（`browsing.poll_secs`），刚装好时要等一会儿。
- 浏览器的数据目录不在默认位置的话，配置 `browser-*.installs`。
- 看看是不是被 `browser-chromium.skip_profiles` 跳过了。

## 时间不对

**一个窗口被切成很多段。** 标题里有一直在变的部分（计时器、光标位置、未读数）。用 `focus.title_rewrites` 去掉它，
默认已经处理了 kakoune 的状态栏。只对之后的记录生效。

**凌晨的事算到了前一天。** 这是故意的：一天从 04:00 开始。改 `query.day_boundary`。

**时长比预期少。** 离开电脑的时间扣掉了。`lifelog timeline` 里能看到每一段「（离开）」。超过 5 分钟没碰键盘鼠标算离开，
看视频的时候也算，改 `idle-wayland.timeout_secs`。

## 项目

**活动没归到项目。**

- 配置了 `project.roots` 吗？默认是空的。
- 改了规则之后，只有新写入的活动按新规则归类，旧记录不会变。

## 摘要

| 报错 | 原因 |
|---|---|
| 「连不上 …/daemon.sock：…提供这个功能的进程没在运行」 | `lifelogd` 没在运行 |
| 「没有配置模型」 | 配置 `llm-openai-compat.model` |
| 「没有配置 key」 | 配置 `key_command` 或 `api_key_env` |
| 退出码 77 | key 不对（接口返回 401 / 403） |
| 退出码 75 | 接口限流，过一会儿再试 |
| 改了配置没变化 | 摘要在 `lifelogd` 里生成，改完要重启它 |

**摘要没有覆盖。** 文件被手改过就不会自动覆盖，用 `--force`。

**那天没有生成。** 在电脑前不到 5 分钟的天不写（`summary.min_active_minutes`）。

## 检索

**搜不到刚才的事。** 全文索引每 30 秒更新一次（`search.index_secs`），只在 `lifelogd` 里做。

**没有「按意思找」的结果。**

- `lifelogd` 要在运行，模型在它里面。
- 第一次用要先下载约 470 MB 的模型，下载完之前没有结果。
- 相似度低于 `vectors.min_score`（默认 0.85）的不算命中。
- 只对摘要和笔记生效，不对窗口、命令、网页。

## 配置

**退出码 78 / 「配置错误」。** 报错信息会指出是哪一段、哪个键。常见的：

- 键名拼错（会被拒绝，不会被悄悄忽略）；
- 组件 id 拼错（报错里列出了所有组件）；
- 程序是旧版本，还不认识新加的键：先 `scripts/install.sh` 升级。

**改了配置不生效。** 配置只在进程启动时读。`lifelogd` 要重启；`lifelog-tui` 和 `lifelog-mcp` 要重开。

**「socket 路径太长」。** Unix socket 的路径最多 107 字节，`XDG_RUNTIME_DIR` 太长了。换一个短一点的。

## 还是不行

带上这些信息开 issue：

- 提交号（`git rev-parse --short HEAD`）；
- `journalctl --user -u lifelogd -n 100`；
- 有关的那段配置（**去掉 key**）；
- `--trace` 的输出里有关的几行（里面可能有你的窗口标题和命令，**发之前看一遍**）。
