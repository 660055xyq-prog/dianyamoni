#!/usr/bin/env bash
# 本地交叉编译脚本：编 aarch64-unknown-linux-musl 全静态单二进制。
# 用法：./build.sh [target]   （默认 aarch64-unknown-linux-musl）
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
TARGET="${1:-aarch64-unknown-linux-musl}"
cd "$ROOT"

if ! command -v cargo >/dev/null 2>&1; then
  echo "错误: 未找到 cargo，请先安装 Rust：https://rustup.rs" >&2
  exit 1
fi

echo "==> 编译目标: $TARGET"

if command -v cargo-zigbuild >/dev/null 2>&1; then
  echo "==> 使用 cargo-zigbuild 交叉编译"
  cargo zigbuild --release --target "$TARGET"
else
  echo "==> 未检测到 cargo-zigbuild，尝试安装..."
  if ! command -v zig >/dev/null 2>&1; then
    echo "错误: 需要 zig 链接器（用于 musl 静态交叉编译）。" >&2
    echo "      macOS: brew install zig && cargo install cargo-zigbuild" >&2
    echo "      Linux: apt install zig && cargo install cargo-zigbuild" >&2
    exit 1
  fi
  cargo install cargo-zigbuild --locked
  cargo zigbuild --release --target "$TARGET"
fi

echo ""
echo "==> 编译完成: $ROOT/target/$TARGET/release/vbat_sim"
echo "==> 执行 ./package.sh 打包成可刷入 zip"
