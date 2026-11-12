#!/usr/bin/env bash
# 全量检查：格式、clippy、架构规则、全部测试。每个阶段结束、每次提交前跑。
set -euo pipefail
cd "$(dirname "$0")/.."

echo "== 格式"
cargo fmt --all -- --check
echo "== clippy"
cargo clippy --workspace --all-targets --all-features -- -D warnings
echo "== 架构规则"
cargo test -q -p arch
echo "== 测试"
cargo test -q --workspace --all-features
echo "== 全部通过"
