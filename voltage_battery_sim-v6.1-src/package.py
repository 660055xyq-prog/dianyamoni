#!/usr/bin/env python3
"""跨平台模块打包脚本（Windows/macOS/Linux 均强制使用 POSIX 正斜杠，彻底避免 WebUI 丢失）"""
import os, shutil, sys, zipfile

ROOT = os.path.dirname(os.path.abspath(__file__))
MODULE = os.path.join(ROOT, "module")
BIN = os.path.join(ROOT, "target", "aarch64-unknown-linux-musl", "release", "vbat_sim")
OUT = os.path.join(ROOT, "dist")
ZIP_PATH = os.path.join(OUT, "voltage_battery_sim-v6.1-rust.zip")

if not os.path.isfile(BIN):
    sys.exit(f"错误: 未找到编译产物 {BIN}，请先编译后再打包！")

os.makedirs(OUT, exist_ok=True)
stage = os.path.join(OUT, "stage")
if os.path.exists(stage):
    shutil.rmtree(stage)
shutil.copytree(MODULE, stage)
readme_bin = os.path.join(stage, "bin", "README.txt")
if os.path.exists(readme_bin):
    os.remove(readme_bin)
os.makedirs(os.path.join(stage, "bin"), exist_ok=True)
shutil.copy2(BIN, os.path.join(stage, "bin", "vbat_sim"))

with zipfile.ZipFile(ZIP_PATH, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as zf:
    for root, dirs, files in os.walk(stage):
        dirs.sort()
        for fn in sorted(files):
            full = os.path.join(root, fn)
            rel = os.path.relpath(full, stage).replace("\\", "/")
            zf.write(full, rel)

print("==> 已生成标准 POSIX 路径可刷入模块:", ZIP_PATH)
