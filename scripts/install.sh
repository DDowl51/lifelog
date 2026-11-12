#!/usr/bin/env bash
# 把 lifelogd、lifelog、lifelog-tui、lifelog-mcp 装到 ~/.cargo/bin，放好 systemd 用户服务（不自动启用）
# 和给 Claude Code 的 skill。
set -euo pipefail
cd "$(dirname "$0")/.."

cargo install --locked --path apps/lifelogd
cargo install --locked --path apps/lifelog
cargo install --locked --path apps/lifelog-tui
cargo install --locked --path apps/lifelog-mcp

unit_dir="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
mkdir -p "$unit_dir"
install -m 644 dist/lifelogd.service "$unit_dir/lifelogd.service"
install -m 644 dist/lifelog-summarize.service "$unit_dir/lifelog-summarize.service"
install -m 644 dist/lifelog-summarize.timer "$unit_dir/lifelog-summarize.timer"
systemctl --user daemon-reload

skill_dir="$HOME/.claude/skills/lifelog"
mkdir -p "$skill_dir"
install -m 644 skills/lifelog/SKILL.md "$skill_dir/SKILL.md"

cat <<'MSG'

装好了。开始记录：

    systemctl --user enable --now lifelogd

看记录：

    lifelog today
    lifelog-tui                           # 终端界面
    journalctl --user -u lifelogd -f      # 日志

每日摘要（先在配置里填好模型接口，见 README）：

    lifelog summarize --dry-run           # 先看看要发给模型的内容
    systemctl --user enable --now lifelog-summarize.timer   # 每天 04:30 生成昨天的

给 Claude Code 用：skill 已经放到 ~/.claude/skills/lifelog。没有 shell 的客户端用 MCP：

    claude mcp add lifelog -- lifelog-mcp

MSG
