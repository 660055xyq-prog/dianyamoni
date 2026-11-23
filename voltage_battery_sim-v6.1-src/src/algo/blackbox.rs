//! 断电黑匣子：主备双副本 + 节流写盘 + 启动复盘（对应 daemon.sh
//! write_blackbox_throttled / is_normal_boot_reason / parse_blackbox_file /
//! audit_blackbox_on_boot / mark_blackbox_clean）。

use crate::config::uv_to_v2;
use crate::daemon::{Daemon, BLACKBOX_BAK, BLACKBOX_FILE};
use crate::platform::android;
use std::fs;

const BB_EVENTS_FILE: &str = "blackbox_events.csv";

/// 黑匣子解析结果。
#[derive(Default)]
struct BlackboxData {
    time: String,
    v: i32,
    i: i32,
    ocv: i32,
    temp: i32,
    comp: i32,
    clean: i32,
}

fn now_md_str() -> String {
    unsafe {
        let mut t: libc::time_t = 0;
        libc::time(&mut t);
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        format!("{:02}-{:02} {:02}:{:02}:{:02}", tm.tm_mon + 1, tm.tm_mday, tm.tm_hour, tm.tm_min, tm.tm_sec)
    }
}

fn append_rotated(path: &std::path::Path, line: &str, keep: usize) {
    let mut content = fs::read_to_string(path).unwrap_or_default();
    content.push_str(line);
    content.push('\n');
    let lines: Vec<&str> = content.lines().collect();
    let kept: Vec<&str> = if lines.len() > keep {
        lines[lines.len() - keep..].to_vec()
    } else {
        lines
    };
    let _ = fs::write(path, kept.join("\n") + "\n");
}

impl Daemon {
    /// 节流写黑匣子（daemon.sh:581-617）。
    pub fn write_blackbox_throttled(&mut self, u_raw: i32, i_ma: i32, o_val: i32, t_c10: i32, v_cmp: i32) {
        let path = self.moddir.join(BLACKBOX_FILE);
        let bak = self.moddir.join(BLACKBOX_BAK);
        let uptime = self.get_uptime();

        let mut need_write = false;
        if !path.exists() || self.last_bb_v == 0 {
            need_write = true;
        } else {
            let dv = (self.last_bb_v - u_raw).abs();
            let di = (i_ma - self.last_bb_i).abs();
            if dv >= 10_000 || di >= 250 || uptime.saturating_sub(self.last_bb_up) >= 15 {
                need_write = true;
            }
        }
        if !need_write {
            return;
        }

        // 主本转备份
        if path.exists() && fs::metadata(&path).map(|m| m.len() > 0).unwrap_or(false) {
            let _ = fs::copy(&path, &bak);
        }

        let content = format!(
            "BB_TIME={}\nBB_V={}\nBB_I_MA={}\nBB_OCV={}\nBB_TEMP={}\nBB_COMP_V={}\nCLEAN_EXIT=0\n",
            now_md_str(),
            u_raw,
            i_ma,
            o_val,
            t_c10,
            v_cmp
        );
        let _ = fs::write(&path, &content);

        self.last_bb_v = u_raw;
        self.last_bb_i = i_ma;
        self.last_bb_up = uptime;
    }

    /// 仅过滤明确的人工正常重启原因（daemon.sh:620-634）。
    pub fn is_normal_boot_reason(&self) -> bool {
        let br = android::getprop("ro.boot.bootreason");
        let sr = android::getprop("sys.boot.reason");
        for r in [br.as_str(), sr.as_str()] {
            match r {
                "reboot,userrequested"
                | "reboot,shell"
                | "reboot,recovery"
                | "reboot,bootloader"
                | "reboot,fastboot"
                | "shutdown,userrequested"
                | "shutdown,thermal"
                | "recovery"
                | "bootloader"
                | "fastboot" => return true,
                _ => {}
            }
        }
        false
    }

    fn parse_blackbox_file(&self, fpath: &std::path::Path) -> Option<BlackboxData> {
        let meta = fs::metadata(fpath).ok()?;
        if meta.len() == 0 {
            return None;
        }
        let text = fs::read_to_string(fpath).ok()?;
        let mut d = BlackboxData {
            temp: 250,
            ..Default::default()
        };
        for raw in text.lines() {
            let line = raw.trim_end_matches('\r');
            if let Some((key, val)) = line.split_once('=') {
                match key.trim() {
                    "BB_TIME" => d.time = val.trim().to_string(),
                    "BB_V" => d.v = val.trim().parse::<i32>().unwrap_or(0),
                    "BB_I_MA" => d.i = val.trim().parse::<i32>().unwrap_or(0),
                    "BB_OCV" => d.ocv = val.trim().parse::<i32>().unwrap_or(0),
                    "BB_TEMP" => d.temp = val.trim().parse::<i32>().unwrap_or(250),
                    "BB_COMP_V" => d.comp = val.trim().parse::<i32>().unwrap_or(0),
                    "CLEAN_EXIT" => d.clean = val.trim().parse::<i32>().unwrap_or(0),
                    _ => {}
                }
            }
        }
        if d.v > 2_000_000 {
            Some(d)
        } else {
            None
        }
    }

    /// 启动复盘（daemon.sh:658-734）。
    pub fn audit_blackbox_on_boot(&mut self) {
        let file = self.moddir.join(BLACKBOX_FILE);
        let bak = self.moddir.join(BLACKBOX_BAK);
        if !file.exists() && !bak.exists() {
            return;
        }

        let data = self.parse_blackbox_file(&file).or_else(|| self.parse_blackbox_file(&bak));

        let _ = fs::remove_file(&file);
        let _ = fs::remove_file(&bak);
        self.last_bb_v = 0;
        self.last_bb_i = 0;
        self.last_bb_up = 0;

        let d = match data {
            Some(x) => x,
            None => return,
        };
        if d.clean != 0 || self.is_normal_boot_reason() {
            return;
        }

        self.calc_effective_shutdown_v();
        let mut max_suspect = self.eff_shutdown_v + 120_000;
        if max_suspect > 3_420_000 {
            max_suspect = 3_420_000;
        }
        if max_suspect < 3_350_000 {
            max_suspect = 3_350_000;
        }

        if d.v > 2_500_000 && d.v < max_suspect {
            self.learned.blackscreen_cnt += 1;
            if self.learned.hw_cutoff_v == 0 {
                self.learned.hw_cutoff_v = d.v;
            } else if d.v > self.learned.hw_cutoff_v {
                self.learned.hw_cutoff_v = (self.learned.hw_cutoff_v * 5 + d.v * 5) / 10;
            } else {
                self.learned.hw_cutoff_v = (self.learned.hw_cutoff_v * 8 + d.v * 2) / 10;
            }

            let ev_line = format!("{},{},{},{},{}", d.time, d.v, d.i, d.temp, d.comp);
            append_rotated(&self.moddir.join(BB_EVENTS_FILE), &ev_line, 20);

            let v_die_s = uv_to_v2(d.v);
            let t_whole = d.temp / 10;
            self.learned.last_bs_info = format!("{} @ {}V ({}mA, {}C)", d.time, v_die_s, d.i, t_whole);

            if self.cfg.auto_min_learn == 1 {
                let mut adj_margin = 50_000;
                if d.i > 1200 {
                    adj_margin = 25_000;
                }
                let mut cold_deduct = 0;
                if d.temp > -300 && d.temp < 150 {
                    cold_deduct = (150 - d.temp) * 300;
                    if cold_deduct > 60_000 {
                        cold_deduct = 60_000;
                    }
                }
                let mut target_floor = (d.v + adj_margin - cold_deduct + 5000) / 10_000 * 10_000;
                let mut cur_ref = self.learned.learned_floor_v;
                if self.cfg.shutdown_v > cur_ref {
                    cur_ref = self.cfg.shutdown_v;
                }
                if cur_ref >= target_floor {
                    target_floor = cur_ref + 30_000;
                }
                if target_floor > 3_500_000 {
                    target_floor = 3_500_000;
                }
                if target_floor < 3_000_000 {
                    target_floor = 3_000_000;
                }
                let v_fl_s = uv_to_v2(target_floor);

                if self.cfg.bb_auto_apply == 1 {
                    self.learned.learned_floor_v = target_floor;
                    self.learned.pending_floor_v = 0;
                    self.notifi(
                        "黑匣子复盘",
                        &format!("上次在 {}V ({}mA) 硬断电，已自动将保护红线提升至 {}V", v_die_s, d.i, v_fl_s),
                    );
                    self.log_event("黑匣子", &format!("复盘：上次在 {}V 硬断电，已自动将保护红线提升至 {}V", v_die_s, v_fl_s));
                } else {
                    self.learned.pending_floor_v = target_floor;
                    self.notifi(
                        "黑匣子复盘建议",
                        &format!("上次在 {}V ({}mA) 疑似硬断电，建议在 WebUI 采纳新红线 {}V", v_die_s, d.i, v_fl_s),
                    );
                    self.log_event("黑匣子", &format!("复盘：上次在 {}V 硬断电，建议保护红线 {}V（待 WebUI 确认）", v_die_s, v_fl_s));
                }
            } else {
                self.notifi(
                    "黑匣子复盘",
                    &format!("检测到上次在 {}V ({}mA) 疑似硬断电，建议调高关机电压", v_die_s, d.i),
                );
                self.log_event("黑匣子", &format!("复盘：上次在 {}V 疑似硬断电，但复盘开关已关闭", v_die_s));
            }
            self.save_learned();
        }
    }

    /// 签退（daemon.sh:184-192）。
    pub fn mark_blackbox_clean(&self) {
        for f in [BLACKBOX_FILE, BLACKBOX_BAK] {
            let p = self.moddir.join(f);
            if p.exists() {
                if let Ok(mut file) = fs::OpenOptions::new().append(true).open(&p) {
                    use std::io::Write;
                    let _ = file.write_all(b"CLEAN_EXIT=1\n");
                }
            }
        }
    }
}
