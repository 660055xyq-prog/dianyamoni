#!/system/bin/sh
# v6.0 卸载清理：终止守护进程（新二进制 + 旧脚本）与看门狗，恢复系统电池状态
MODDIR=${0%/*}

# 终止看门狗（service.sh 驻留进程）
if [ -r "$MODDIR/.watchdog.pid" ]; then
  while read -r _wp; do
    [ -n "$_wp" ] && kill -9 "$_wp" >/dev/null 2>&1
  done < "$MODDIR/.watchdog.pid"
fi

# 终止守护进程：优先 pid 文件，再按新/旧名兜底
if [ -r "$MODDIR/.daemon.pid" ]; then
  while read -r _dp; do
    [ -n "$_dp" ] && kill -9 "$_dp" >/dev/null 2>&1
  done < "$MODDIR/.daemon.pid"
fi
pgrep -f "vbat_sim daemon" | xargs -r kill -TERM >/dev/null 2>&1
pgrep -f "daemon.sh" | xargs -r kill -TERM >/dev/null 2>&1
pgrep -f "电压电量.sh$" | xargs -r kill -TERM >/dev/null 2>&1
sleep 1
pgrep -f "vbat_sim daemon" | xargs -r kill -9 >/dev/null 2>&1
pgrep -f "daemon.sh" | xargs -r kill -9 >/dev/null 2>&1
pgrep -f "电压电量.sh$" | xargs -r kill -9 >/dev/null 2>&1

dumpsys battery reset >/dev/null 2>&1
