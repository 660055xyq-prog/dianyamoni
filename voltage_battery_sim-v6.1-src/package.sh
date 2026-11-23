#!/usr/bin/env bash
# 打包 KSU/Magisk 模块 zip（强制 POSIX 正斜杠，禁止打包缺失二进制的残缺包）
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
MODULE="$ROOT/module"
BIN="$ROOT/target/aarch64-unknown-linux-musl/release/vbat_sim"
OUT="$ROOT/dist"
ZIP="voltage_battery_sim-v6.1-rust.zip"

if [ ! -f "$BIN" ]; then
  echo "错误: 未找到编译产物 $BIN，请先运行 ./build.sh" >&2
  exit 1
fi

rm -rf "$OUT"
mkdir -p "$OUT/stage/bin"
cp -r "$MODULE/." "$OUT/stage/"
rm -f "$OUT/stage/bin/README.txt"
cp "$BIN" "$OUT/stage/bin/vbat_sim"
chmod 0755 "$OUT/stage/bin/vbat_sim"

cd "$OUT/stage"
zip -qr "$OUT/$ZIP" .
cd "$ROOT"

echo "==> 已生成标准可刷入模块: $OUT/$ZIP"
ls -lh "$OUT/$ZIP"
