//! 低电提醒 + 双轨动态防黑屏关机检测 + 倒计时状态机。
//!
//! 复刻自 daemon.sh 的 get_snooze_remaining(891-909) / check_tiered_low_warning(1380-1411) /
//! check_voltage_shutdown(1414-1519) 与倒计时子进程逻辑。v5.52 的倒计时是一个后台子进程，
//! v6.0 改为进程内状态机，由事件循环每秒驱动一次 countdown_tick()。

use crate::config::uv_to_v2;
use crate::daemon::{Daemon, COUNTDOWN_PID_FILE, SNOOZE_FILE};
use crate::platform::{android, evdev};
use crate::sysfs;

impl Daemon {
    /// 读取 .snooze_until，返回 (剩余秒数, 是否已取消)。
    pub fn get_snooze_remaining(&self) -> (i32, bool) {
        let path = self.moddir.join(SNOOZE_FILE);
        if !path.is_file() {
            return (0, false);
        }
        let s = sysfs::read_node_str(&path, "0");
        let until: u64 = match s.parse() {
            Ok(v) => v,
            Err(_) => return (0, false),
        };
        if until >= 999_999_990 {
            return (999_999, true);
        }
        let uptime = self.get_uptime();
        if until > uptime {
            ((until - uptime) as i32, false)
        } else {
            let _ = std::fs::remove_file(&path);
            (0, false)
        }
    }

    /// 阶梯式低电提醒。
    pub fn check_tiered_low_warning(&mut self, disp_lvl: i32, flt_v: i32) {
        if self.counting_down {
            return;
        }
        let low_max = self.cfg.low_max;
        let low_min = self.cfg.low_min;

        if disp_lvl >= 18 && flt_v > low_max + 50_000 {
            self.warn_stage = 0;
            self.heavy_sag_warned = false;
            return;
        }

        if flt_v < low_max && flt_v > low_min {
            if self.warn_stage < 3 {
                self.notifi("低电警告", "电池电压已进入低电预警区，请尽快连接充电器");
                self.danmu("电压过低，立即连接充电器！");
                self.log_event(
                    "低电提醒",
                    &format!("电压进入低电预警区（{}V），请尽快充电", uv_to_v2(flt_v)),
                );
                self.warn_stage = 3;
            }
        } else if disp_lvl <= 5 {
            if self.warn_stage < 2 {
                self.notifi("电量极低", &format!("当前电量仅剩 {}%，请尽快充电", disp_lvl));
                self.danmu(&format!("电量仅剩 {}%，请尽快充电", disp_lvl));
                self.log_event("低电提醒", &format!("电量极低（{}%），请尽快充电", disp_lvl));
                self.warn_stage = 2;
            }
        } else if disp_lvl <= 15 {
            if self.warn_stage < 1 {
                self.notifi("电量偏低", &format!("当前电量剩余 {}%，请留意充电", disp_lvl));
                self.log_event("低电提醒", &format!("电量偏低（{}%），请留意充电", disp_lvl));
                self.warn_stage = 1;
            }
        }
    }

    /// 双轨动态防黑屏关机检测：连击计数 + 触发倒计时。
    pub fn check_voltage_shutdown(&mut self, u_raw: i32, u_raw_flt: i32, v_cmp: i32) {
        let sv = self.eff_shutdown_v;
        if u_raw > 1_000_000 && u_raw < sv && u_raw_flt < sv {
            self.low_v_streak += 1;
            if self.ir_active && v_cmp > sv + 120_000 && !self.heavy_sag_warned {
                self.notifi(
                    "重负载掉压提醒",
                    &format!(
                        "当前放电电流较大({}mA)拉低电压，退回轻度使用可继续榨出剩余电量",
                        self.disch_ma
                    ),
                );
                self.heavy_sag_warned = true;
            }
        } else {
            self.low_v_streak = 0;
            return;
        }

        if self.low_v_streak >= self.cfg.shutdown_confirm && !self.counting_down {
            let (rem, canceled) = self.get_snooze_remaining();
            if canceled || rem > 0 {
                self.low_v_streak = 0;
            } else if self.get_uptime() < self.cfg.boot_grace_sec as u64 {
                if !self.grace_notified {
                    self.notifi(
                        "安全保护期",
                        &format!(
                            "电压已低于动态关机红线，但处于开机 {}s 保护期内，暂不关机",
                            self.cfg.boot_grace_sec
                        ),
                    );
                    self.grace_notified = true;
                }
            } else {
                // v5.4：引导校准进行到关机红线时，先结算曲线再进入倒计时
                if self.cfg.calib_mode == 1 && self.calib_phase == 1 {
                    self.log_event(
                        "校准",
                        &format!("放电采样到达关机红线，采样停止（已采 {} 点）", self.calib_samples),
                    );
                    self.calib_generate("关机红线结算");
                }

                let eff_s = uv_to_v2(sv);
                let mut rapid_v = sv - 70_000;
                if rapid_v < 3_050_000 {
                    rapid_v = 3_050_000;
                }
                self.log_event(
                    "关机保护",
                    &format!(
                        "电压连续 {} 次低于红线 {}V，开始 {} 秒关机倒计时",
                        self.low_v_streak, eff_s, self.cfg.off_time
                    ),
                );
                self.notifi(
                    "关机警告",
                    &format!(
                        "连续 {} 次低于动态红线({}V)，将在 {} 秒后关机",
                        self.low_v_streak, eff_s, self.cfg.off_time
                    ),
                );
                self.danmu(&format!("即将 {} 秒内关机！", self.cfg.off_time));
                self.start_countdown(rapid_v);
            }
        }
    }

    /// 武装倒计时（对应倒计时子进程的初始化 + start_key_monitor）。
    pub fn start_countdown(&mut self, rapid_v: i32) {
        self.counting_down = true;
        self.cd_remaining = self.cfg.off_time;
        self.cd_rapid_hits = 0;
        self.cd_rapid_done = false;
        self.cd_recover_hits = 0;
        self.cd_recover_v = self.eff_shutdown_v + 80_000;
        self.cd_rapid_v = rapid_v;
        self.cd_key_snooze_sec = self.cfg.key_snooze_sec;

        if self.cfg.key_snooze == 1 {
            if let Some(node) = evdev::detect_key_node(&self.moddir) {
                self.key_reader = evdev::KeyReader::open(&node);
            }
        }

        // status 的 COUNTDOWN_ACTIVE 用 .countdown.pid 存活判定：写入自身 pid。
        let pid = std::process::id();
        let _ = std::fs::write(self.moddir.join(COUNTDOWN_PID_FILE), format!("{}\n", pid));
    }

    /// 取消倒计时（对应 stop_key_monitor + kill countdown）。
    pub fn stop_countdown(&mut self) {
        self.counting_down = false;
        self.cd_remaining = 0;
        self.key_reader = None;
        let _ = std::fs::remove_file(self.moddir.join(COUNTDOWN_PID_FILE));
    }

    /// 每秒一次倒计时 tick（事件循环驱动，对应倒计时子进程的 while 循环体）。
    pub fn countdown_tick(&mut self) {
        if !self.counting_down {
            return;
        }
        self.cd_remaining -= 1;

        // v5.4 音量键盲操续命
        if self.cfg.key_snooze == 1 {
            let pressed = self
                .key_reader
                .as_ref()
                .map(|k| k.read_volume_down())
                .unwrap_or(false);
            if pressed {
                self.cd_remaining += self.cd_key_snooze_sec;
                if self.cd_remaining > 900 {
                    self.cd_remaining = 900;
                }
                self.log_event(
                    "续命",
                    &format!(
                        "音量键盲操续命 +{}s，自动关机延后至 {}s",
                        self.cd_key_snooze_sec, self.cd_remaining
                    ),
                );
                self.notifi(
                    "音量键续命",
                    &format!("已延后自动关机 {} 秒，剩余约 {} 秒", self.cd_key_snooze_sec, self.cd_remaining),
                );
                self.danmu(&format!("已延后关机，剩余 {}s", self.cd_remaining));
            }
        }

        // v5.52：每 2 秒复查带载电压
        if self.cd_remaining % 2 == 0 {
            let cd_v = self.normalize_voltage(sysfs::read_node_i32(&self.bat_dir.join("voltage_now"), 0));
            if cd_v >= self.cd_recover_v {
                self.cd_recover_hits += 1;
                self.cd_rapid_hits = 0;
                if self.cd_recover_hits >= 3 {
                    self.log_event(
                        "关机保护",
                        &format!("负载撤离后电压已稳定恢复至 {}V，自动取消关机倒计时", uv_to_v2(cd_v)),
                    );
                    self.notifi(
                        "自动取消关机",
                        &format!("电压已恢复至 {}V 安全区间，本次关机已自动取消", uv_to_v2(cd_v)),
                    );
                    self.danmu("电压已恢复，自动取消关机");
                    self.stop_countdown();
                    return;
                }
            } else {
                self.cd_recover_hits = 0;
                if self.cfg.rapid_shutdown == 1
                    && !self.cd_rapid_done
                    && self.cd_remaining > 12
                    && cd_v > 1_000_000
                    && cd_v < self.cd_rapid_v
                {
                    self.cd_rapid_hits += 1;
                    if self.cd_rapid_hits >= 2 {
                        self.cd_remaining = 10;
                        self.cd_rapid_done = true;
                        self.log_event(
                            "急速关机",
                            &format!("电压跌破临界安全线（{}V），倒计时快进至 10 秒防硬断电", uv_to_v2(cd_v)),
                        );
                        self.notifi("急速关机加速", "电压已跌破临界安全线，倒计时缩短至 10 秒以防直接黑屏！");
                        self.danmu("电压告急，10 秒后关机！");
                    }
                } else {
                    self.cd_rapid_hits = 0;
                }
            }
        }

        // 里程碑提醒（仅 5s 节点，避免连弹）
        if self.cd_remaining == 60
            || self.cd_remaining == 30
            || self.cd_remaining == 15
            || self.cd_remaining == 10
            || self.cd_remaining == 5
        {
            if self.cd_remaining > 0 {
                self.notifi(
                    "关机倒计时",
                    &format!("剩余 {} 秒后自动关机，插电或按音量键可延后", self.cd_remaining),
                );
                self.danmu(&format!("剩余 {} 秒关机", self.cd_remaining));
            }
        }

        if self.cd_remaining <= 0 {
            self.stop_countdown();
            self.mark_blackbox_clean();
            android::svc_power_shutdown();
        }
    }
}
