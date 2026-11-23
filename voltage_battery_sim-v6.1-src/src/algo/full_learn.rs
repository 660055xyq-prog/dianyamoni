//! 真满电累积计时 + 拔线静置满电学习（对应 daemon.sh track_true_full_charge /
//! sync_custom_curve_v100 / try_learn_resting_max_v）。

use crate::config::uv_to_v2;
use crate::daemon::Daemon;
use crate::sysfs;
use std::time::Duration;

impl Daemon {
    /// 真满电累积计时状态机（daemon.sh:737-778）。
    pub fn track_true_full_charge(&mut self, u_now: i32, real_bb: i32, bst_now: &str) {
        if self.cfg.auto_max_learn != 1 {
            self.full_sat_accum = 0;
            self.full_sat_last_up = 0;
            self.full_100_accum = 0;
            self.true_full_armed = false;
            return;
        }

        let uptime = self.get_uptime();
        let mut dt = 0i64;
        if self.full_sat_last_up > 0 {
            dt = uptime as i64 - self.full_sat_last_up as i64;
            if dt < 0 {
                dt = 0;
            }
            if dt > 120 {
                dt = 120;
            }
        }
        self.full_sat_last_up = uptime;

        if (real_bb < 98 && bst_now != "Full") || u_now < 4_150_000 {
            self.full_sat_accum = 0;
            self.full_100_accum = 0;
            self.true_full_armed = false;
            return;
        }

        if (real_bb >= 99 || bst_now == "Full") && u_now >= 4_200_000 {
            if real_bb >= 100 || bst_now == "Full" {
                self.full_100_accum += dt as i32;
            }
            let mut max_chg_ma = 300;
            if bst_now == "Full" {
                max_chg_ma = 350;
            }
            if !self.curr_valid || self.chg_ma <= max_chg_ma {
                self.full_sat_accum += dt as i32;
            }
            if self.full_sat_accum >= 180 || self.full_100_accum >= 600 {
                if !self.true_full_armed {
                    self.true_full_armed = true;
                    self.log_event("满电学习", "饱充判定完成，真满电就绪（拔线后自动采样静置电压）");
                }
            }
        }
    }

    /// 同步更新专属曲线 V_100 节点（daemon.sh:781-803）。
    pub fn sync_custom_curve_v100(&mut self, mut new_v100: i32) {
        let curve_file = self.moddir.join(crate::daemon::CALIB_CURVE_FILE);
        if !curve_file.is_file() {
            return;
        }
        let text = match std::fs::read_to_string(&curve_file) {
            Ok(t) => t,
            Err(_) => return,
        };
        if let Some(v90) = read_key(&text, "V_90") {
            if let Ok(v) = v90.parse::<i32>() {
                if new_v100 < v + 20_000 {
                    new_v100 = v + 20_000;
                }
            }
        }
        if new_v100 > 4_650_000 {
            new_v100 = 4_650_000;
        }
        let mut out = String::new();
        for raw in text.lines() {
            let line = raw.trim_end_matches('\r');
            if line.starts_with("V_100=") {
                out.push_str(&format!("V_100={}\n", new_v100));
            } else {
                out.push_str(line);
                out.push('\n');
            }
        }
        let _ = std::fs::write(&curve_file, &out);
        self.load_custom_curve();
    }

    /// 拔线静置学习满电电压（daemon.sh:806-888）。
    pub fn try_learn_resting_max_v(&mut self) {
        if !(self.max_learn_pending && self.cfg.auto_max_learn == 1) {
            return;
        }
        let uptime = self.get_uptime();
        let elapsed = uptime as i64 - self.unplug_up as i64;
        if elapsed < 45 {
            return;
        }
        if elapsed > 900 {
            self.max_learn_pending = false;
            if !self.max_learn_noted {
                self.log_event("满电学习", "跳过：拔线后 15 分钟采样窗口超时，放弃本次学习");
                self.max_learn_noted = true;
            }
            return;
        }
        if self.curr_valid && self.disch_ma > 550 {
            return;
        }

        let mut samples = [0i32; 5];
        for idx in 0..5 {
            let ut_s = sysfs::read_node_str(&self.usb_type_path, "Unknown");
            let st_s = sysfs::read_node_str(&self.bat_dir.join("status"), "");
            if sysfs::is_charging_now(&ut_s, &st_s) {
                self.max_learn_pending = false;
                if !self.max_learn_noted {
                    self.log_event("满电学习", "跳过：采样窗口内重新插电，放弃本次学习");
                    self.max_learn_noted = true;
                }
                return;
            }
            self.read_sensors(0);
            let o_s = self.normalize_voltage(sysfs::read_node_i32(&self.bat_dir.join("voltage_now"), 0));
            let ocv_s = self.normalize_voltage(sysfs::read_node_i32(&self.bat_dir.join("voltage_ocv"), 0));
            self.calc_compensated_voltage(o_s, ocv_s);
            samples[idx] = self.v_comp;
            if idx < 4 {
                std::thread::sleep(Duration::from_secs(1));
            }
        }

        let mut sorted = samples;
        sorted.sort_unstable();
        let v_full = sorted[2];
        self.max_learn_pending = false;
        self.max_learn_noted = true;

        if v_full >= 4_200_000 && v_full <= 4_550_000 && v_full - self.cfg.min_v >= 500_000 {
            let ema_v = (self.cfg.max_v * 7 + v_full * 3 + 5) / 10;
            let mut new_max = (ema_v + 5000) / 10_000 * 10_000;
            if new_max < 4_200_000 {
                new_max = 4_200_000;
            }
            if new_max > 4_550_000 {
                new_max = 4_550_000;
            }

            self.learned.last_max_learn_v = v_full;
            self.learned.max_learn_cnt += 1;
            self.save_learned();

            let vf_s = uv_to_v2(v_full);
            if new_max != self.cfg.max_v {
                self.cfg.max_v = new_max;
                self.save_config();
                crate::config::update_prop(&self.moddir, &self.cfg, &self.learned);
                self.sync_custom_curve_v100(new_max);
                let nm_s = uv_to_v2(new_max);
                self.notifi(
                    "满电静置学习",
                    &format!("采样真满电补偿电压 {}V，已平滑更新满电电压至 {}V", vf_s, nm_s),
                );
                self.log_event(
                    "满电学习",
                    &format!(
                        "成功：真满电 {}V，满电电压平滑更新为 {}V（第 {} 次学习）",
                        vf_s, nm_s, self.learned.max_learn_cnt
                    ),
                );
            } else {
                self.notifi(
                    "满电静置学习",
                    &format!("采样真满电补偿电压 {}V，与当前满电电压一致无需变动", vf_s),
                );
                self.log_event("满电学习", &format!("完成：真满电 {}V，与当前满电电压一致", vf_s));
            }
        } else {
            let vf_s = uv_to_v2(v_full);
            self.log_event("满电学习", &format!("跳过：采样电压 {}V 超出有效区间（4.20~4.55V），放弃本次学习", vf_s));
        }
    }
}

fn read_key(text: &str, key: &str) -> Option<String> {
    for line in text.lines() {
        if let Some((k, v)) = line.split_once('=') {
            if k.trim() == key {
                return Some(v.trim_end_matches('\r').trim().to_string());
            }
        }
    }
    None
}
