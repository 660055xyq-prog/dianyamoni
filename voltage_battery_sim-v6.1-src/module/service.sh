#!/system/bin/sh
# ============================================================
# 电压电量模拟服务 v6.1 - 开机启动器 + 看门狗自愈循环
# 职责：等开机完成 -> 启动 bin/vbat_sim daemon -> 每 60s 巡检，
#       守护进程缺失时自动拉起（每次事故只通知一次）。
# ============================================================
MODDIR=${0%/*}
BIN="$MODDIR/bin/vbat_sim"

# 1. 等待开机完成与 dumpsys 就绪
until [ "$(getprop sys.boot_completed)" = "1" ]; do
  sleep 2
done

count=0
while [ $count -lt 30 ]; do
  if dumpsys battery >/dev/null 2>&1; then
    break
  fi
  sleep 2
  count=$((count + 1))
done

sleep 5

export PATH="$PATH:/data/adb/ksu/bin:/data/adb/ap/bin:/data/adb/magisk"

log_event () {
  echo "$(date '+%Y-%m-%d %H:%M:%S')|$1|$2" >> "$MODDIR/events.log" 2>/dev/null
  tail -n 120 "$MODDIR/events.log" > "$MODDIR/events.log.tmp" 2>/dev/null && mv "$MODDIR/events.log.tmp" "$MODDIR/events.log" 2>/dev/null
}

# 2. 开机即恢复自愈 + 清理旧版（中文名/daemon.sh）守护进程
rm -f "$MODDIR/.watchdog_pause" 2>/dev/null
pgrep -f "电压电量.sh" | xargs -r kill -9 >/dev/null 2>&1
pgrep -f "daemon.sh" | xargs -r kill -9 >/dev/null 2>&1

# 3. 记录看门狗 PID（供 status 快照与卸载清理）
echo $$ > "$MODDIR/.watchdog.pid" 2>/dev/null

launch_daemon () {
  "$BIN" daemon >/dev/null 2>&1 &
  sleep 2
}

log_event "服务" "开机启动（看门狗就绪 PID: $$）"
launch_daemon

# 4. 看门狗主循环：60s 巡检一次
alarmed=0
while true; do
  sleep 60

  # 用户显式 stop 时暂停自愈
  if [ -f "$MODDIR/.watchdog_pause" ]; then
    alarmed=0
    continue
  fi

  alive=0
  if [ -r "$MODDIR/.daemon.pid" ] && kill -0 "$(cat "$MODDIR/.daemon.pid" 2>/dev/null)" 2>/dev/null; then
    alive=1
  else
    DPID=$(pgrep -f "vbat_sim daemon" 2>/dev/null | head -n 1)
    [ -n "$DPID" ] && alive=1
  fi

  if [ "$alive" -eq 0 ]; then
    log_event "服务" "看门狗检测到守护进程缺失，自动拉起"
    if [ "$alarmed" -eq 0 ]; then
      alarmed=1
      su 2000 cmd notification post -S bigtext -t "电压电量模拟服务" "vbat_wd" "服务意外退出，看门狗已自动拉起" >/dev/null 2>&1
    fi
    launch_daemon
  else
    alarmed=0
  fi
done
