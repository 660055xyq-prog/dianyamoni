//! sysfs 文件读取、节点探测、屏幕/充电状态判定。
//!
//! 复刻自 common.sh 的 read_one_line / read_node / detect_bat_node /
//! detect_backlight_node / is_screen_off / is_charging_now / get_uptime_sec /
//! norm_v_helper，以及 daemon.sh 的 read_node 包装。

use std::fs;
use std::path::{Path, PathBuf};

pub const SYS_BAT_DIR_DEFAULT: &str = "/sys/class/power_supply/battery";
pub const SYS_USB_TYPE_DEFAULT: &str = "/sys/class/power_supply/usb/type";

/// `[ -r path ]`：文件是否可读。
pub fn is_readable(path: &Path) -> bool {
    fs::File::open(path).is_ok()
}

/// 读单行（对应 read_one_line）：可读且首行非空则返回去 \r / 首尾空白的值。
pub fn read_one_line(path: &Path) -> Option<String> {
    let data = fs::read_to_string(path).ok()?;
    let line = data.lines().next()?;
    let s = line.trim_end_matches('\r').trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// 读节点值，为空/不可读时回退默认值（对应 read_node，返回原始字符串）。
pub fn read_node_str(path: &Path, default: &str) -> String {
    read_one_line(path).unwrap_or_else(|| default.to_string())
}

/// 读节点并解析为 i32（非有符号整数时回退默认值）。
pub fn read_node_i32(path: &Path, default: i32) -> i32 {
    match read_one_line(path) {
        Some(s) => s.parse::<i32>().unwrap_or(default),
        None => default,
    }
}

/// 从 key=value 文件读取指定键（对应 read_key_from_file）。
pub fn read_key_from_file(path: &Path, key: &str) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    for line in text.lines() {
        if let Some((k, v)) = line.split_once('=') {
            if k.trim() == key {
                return Some(v.trim_end_matches('\r').trim().to_string());
            }
        }
    }
    None
}

/// 系统运行秒数（对应 get_uptime_sec_common，读 /proc/uptime 首字段取整）。
pub fn get_uptime_sec() -> u64 {
    if let Ok(data) = fs::read_to_string("/proc/uptime") {
        if let Some(first) = data.split_whitespace().next() {
            if let Some(secs) = first.split('.').next() {
                if let Ok(v) = secs.parse::<u64>() {
                    return v;
                }
            }
        }
    }
    0
}

/// 电池电压节点自动探测（对应 detect_bat_node）。
pub fn detect_bat_node(moddir: &Path) -> PathBuf {
    let default = PathBuf::from(SYS_BAT_DIR_DEFAULT);
    if is_readable(&default.join("voltage_now")) {
        return default;
    }
    if let Some(v) = read_one_line(&moddir.join(".batnode_override")) {
        if !v.is_empty() {
            let p = PathBuf::from(&v);
            if is_readable(&p.join("voltage_now")) {
                return p;
            }
        }
    }
    if let Some(v) = read_one_line(&moddir.join(".batnode")) {
        if !v.is_empty() {
            let p = PathBuf::from(&v);
            if is_readable(&p.join("voltage_now")) {
                return p;
            }
        }
    }
    let mut found: Option<PathBuf> = None;
    if let Ok(entries) = fs::read_dir("/sys/class/power_supply") {
        for e in entries.flatten() {
            let dir = e.path();
            if !is_readable(&dir.join("voltage_now")) {
                continue;
            }
            let ty = read_one_line(&dir.join("type")).unwrap_or_default();
            match ty.as_str() {
                "Battery" | "Battery_2" | "BMS" | "Main" | "battery" | "bms" => {
                    found = Some(dir);
                    break;
                }
                _ => {
                    if found.is_none() {
                        found = Some(dir);
                    }
                }
            }
        }
    }
    if let Some(p) = found {
        let _ = fs::write(moddir.join(".batnode"), format!("{}\n", p.display()));
        p
    } else {
        default
    }
}

/// 背光亮度节点探测（对应 detect_backlight_node）。
pub fn detect_backlight_node() -> Option<PathBuf> {
    let panel0 = PathBuf::from("/sys/class/backlight/panel0-backlight/brightness");
    if is_readable(&panel0) {
        return Some(panel0);
    }
    if let Ok(entries) = fs::read_dir("/sys/class/backlight") {
        for e in entries.flatten() {
            let b = e.path().join("brightness");
            if is_readable(&b) {
                return Some(b);
            }
        }
    }
    let lcd = PathBuf::from("/sys/class/leds/lcd-backlight/brightness");
    if is_readable(&lcd) {
        return Some(lcd);
    }
    None
}

/// 是否息屏（对应 is_screen_off：无节点视为未息屏；亮度==0 视为息屏）。
pub fn is_screen_off(bl_node: Option<&Path>) -> bool {
    match bl_node {
        Some(p) => read_node_str(p, "1") == "0",
        None => false,
    }
}

/// 充电状态判定（对应 is_charging_now：usb type 非空非 Unknown/None，或 status==Charging）。
pub fn is_charging_now(usbtype: &str, bst: &str) -> bool {
    if !usbtype.is_empty() && usbtype != "Unknown" && usbtype != "None" {
        return true;
    }
    bst == "Charging"
}

/// 简单电压归一化（对应 common.sh norm_v_helper：>5.5V 且 <10V 除以 2）。
pub fn norm_v_helper(v: i32) -> i32 {
    if v > 5_500_000 && v < 10_000_000 {
        v / 2
    } else {
        v
    }
}
