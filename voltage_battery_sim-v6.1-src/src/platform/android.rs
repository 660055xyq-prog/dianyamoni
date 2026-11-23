//! Android 系统命令封装：通知、弹幕、dumpsys、svc、getprop、resetprop、时间戳。
//!
//! 复刻自 daemon.sh 的 notifi / danmu 与各 dumpsys/svc/getprop 直接调用。

use std::process::{Command, Stdio};

/// 通知（对应 notifi：su 2000 -c "cmd notification post ..."）。
pub fn notifi(title: &str, msg: &str) {
    let t = title.replace('\'', "'\\''");
    let m = msg.replace('\'', "'\\''");
    let s = format!("cmd notification post -S bigtext -t '{}' 'Tag' '{}'", t, m);
    let _ = Command::new("su")
        .arg("2000")
        .arg("-c")
        .arg(&s)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// 弹幕（对应 danmu：cmd activity shortx danmu）。
pub fn danmu(msg: &str) {
    let _ = Command::new("cmd")
        .args(["activity", "shortx", "danmu", msg])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// 通用 dumpsys battery 调用。
pub fn dumpsys_battery(args: &[&str]) {
    let _ = Command::new("dumpsys")
        .arg("battery")
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

pub fn dumpsys_set_level(level: i32) {
    let lv = level.to_string();
    dumpsys_battery(&["set", "level", &lv]);
}

pub fn dumpsys_reset() {
    dumpsys_battery(&["reset"]);
}

pub fn dumpsys_unplug() {
    dumpsys_battery(&["unplug"]);
}

pub fn dumpsys_set_status(s: i32) {
    let v = s.to_string();
    dumpsys_battery(&["set", "status", &v]);
}

pub fn dumpsys_set_ac(v: i32) {
    let x = v.to_string();
    dumpsys_battery(&["set", "ac", &x]);
}

/// 关机（对应 svc power shutdown）。
pub fn svc_power_shutdown() {
    let _ = Command::new("svc")
        .args(["power", "shutdown"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// 读取系统属性（对应 getprop key，去尾部换行）。
pub fn getprop(key: &str) -> String {
    match Command::new("getprop").arg(key).output() {
        Ok(o) => String::from_utf8_lossy(&o.stdout)
            .trim_end_matches('\n')
            .trim_end_matches('\r')
            .trim()
            .to_string(),
        Err(_) => String::new(),
    }
}

/// 写系统属性（对应 resetprop -n key val）。
pub fn resetprop(key: &str, val: &str) {
    let _ = Command::new("resetprop")
        .args(["-n", key, val])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// 本地时间戳 `%Y-%m-%d %H:%M:%S`（对应 date 调用）。
pub fn now_ts_str() -> String {
    unsafe {
        let mut t: libc::time_t = 0;
        libc::time(&mut t);
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        )
    }
}
