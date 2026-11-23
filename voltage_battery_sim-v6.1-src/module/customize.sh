#!/system/bin/sh
ui_print "**********************************"
ui_print " 正在安装：电压电量模拟服务 v6.1 (by 影儿)  "
ui_print "**********************************"

if [ -n "$ARCH" ] && [ "$ARCH" != "arm64" ]; then
  abort "! 仅支持 arm64 (aarch64) 设备，当前架构：$ARCH"
fi

if [ ! -f "$MODPATH/bin/vbat_sim" ]; then
  abort "! 模块包缺少 bin/vbat_sim 二进制，请使用完整编译包（GitHub Actions 或 ./build.sh && python3 package.py）"
fi

OLD_DIR="/data/adb/modules/voltage_battery_sim"

# 终止旧版守护进程（中文名 / daemon.sh / 旧版 vbat_sim），避免新旧实例并存
ui_print "- 终止旧版守护进程..."
pgrep -f "电压电量.sh" | xargs -r kill -9 >/dev/null 2>&1
pgrep -f "daemon.sh" | xargs -r kill -9 >/dev/null 2>&1
pgrep -f "vbat_sim daemon" | xargs -r kill -9 >/dev/null 2>&1
[ -f "$OLD_DIR/.watchdog.pid" ] && kill -9 "$(cat "$OLD_DIR/.watchdog.pid" 2>/dev/null)" >/dev/null 2>&1

# 数据文件迁移：保留配置、学习库、专属曲线、断点校准进度与手动节点覆盖
for f in config.conf learned.conf history.csv blackbox_events.csv curve.conf events.log calib_samples.csv .calibrating .keynode .keynode_override .batnode_override; do
  if [ -f "$OLD_DIR/$f" ]; then
    cp -f "$OLD_DIR/$f" "$MODPATH/$f"
    ui_print "- 已保留旧版数据：$f"
  fi
done

ui_print "- 设置脚本与 WebUI 权限..."
set_perm "$MODPATH/bin/vbat_sim" 0 0 0755
set_perm "$MODPATH/service.sh" 0 0 0755
set_perm "$MODPATH/action.sh" 0 0 0755
set_perm "$MODPATH/config_helper.sh" 0 0 0755
set_perm "$MODPATH/uninstall.sh" 0 0 0755
set_perm "$MODPATH/config.conf" 0 0 0644
set_perm_recursive "$MODPATH/webroot" 0 0 0755 0644

sh "$MODPATH/config_helper.sh" migrate >/dev/null 2>&1
sh "$MODPATH/config_helper.sh" sync_prop >/dev/null 2>&1

ui_print "- v6.1 重点更新（作者：影儿）："
ui_print "  * 修复 evdev 音量键监听、timerfd 挂起与 IPC 超时等底层 Bug"
ui_print "  * 修复 Windows 打包反斜杠导致 WebUI 消失问题，补全 Magisk META-INF"
ui_print "  * 全静态 Rust (aarch64 musl) + epoll/uevent 事件驱动"
ui_print "- 请重启设备使新版本生效"
