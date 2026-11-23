//! telemetry：事件日志 / 退出原因 / 运行时状态快照 / 历史采样。

use crate::platform::android;
use std::fs;
use std::io::Write;
use std::path::Path;

pub const EVENTS_FILE: &str = "events.log";
pub const LAST_EXIT_FILE: &str = ".last_exit";
pub const STATE_FILE: &str = ".runtime_state";
pub const HIST_FILE: &str = "history.csv";

/// 追加一条事件日志（%Y-%m-%d %H:%M:%S|type|msg），并 tail -n120 轮转（对应 log_event）。
pub fn log_event(moddir: &Path, ty: &str, msg: &str) {
    let path = moddir.join(EVENTS_FILE);
    let line = format!("{}|{}|{}", android::now_ts_str(), ty, msg);
    let mut content = fs::read_to_string(&path).unwrap_or_default();
    content.push_str(&line);
    content.push('\n');
    // 轮转：保留最后 120 行
    let lines: Vec<&str> = content.lines().collect();
    let keep: Vec<&str> = if lines.len() > 120 {
        lines[lines.len() - 120..].to_vec()
    } else {
        lines
    };
    let _ = fs::write(&path, keep.join("\n") + "\n");
}

/// 写 .last_exit "ts|reason|rc=N" 并记事件（对应 record_exit_reason）。
pub fn record_exit_reason(moddir: &Path, reason: &str, rc: i32) {
    let line = format!("{}|{}|rc={}", android::now_ts_str(), reason, rc);
    let path = moddir.join(LAST_EXIT_FILE);
    let _ = fs::write(&path, &line);
    log_event(moddir, "退出", &format!("reason={} rc={}", reason, rc));
}

/// 追加一行 CSV（history.csv 等），返回写前是否成功。
pub fn append_csv(moddir: &Path, name: &str, line: &str) -> std::io::Result<()> {
    let path = moddir.join(name);
    let mut f = fs::OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(f, "{}", line)
}
