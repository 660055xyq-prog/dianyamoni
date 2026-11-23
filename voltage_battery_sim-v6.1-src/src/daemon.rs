//! daemon：守护进程核心。定义 Daemon 结构体（全部运行状态）与
//! 传感器读取、电压归一化、平滑输出、充电平滑、主流水线、倒计时、事件循环、启动。
//!
//! 算法方法（内阻/补偿/红线/黑匣子/满电学习/校准/关机）以 `impl Daemon` 形式
//! 分布在 algo/ 各子模块中。

use crate::algo::filter::FilterWindow;
use crate::config::{parse_int, Config, Learned};
use crate::platform::android;
use crate::sysfs;
use crate::sysfs::SYS_USB_TYPE_DEFAULT;
use crate::telemetry;
use std::io::Write;
use std::path::PathBuf;

pub const CONFIG_FILE: &str = "config.conf";
pub const LEARNED_FILE: &str = "learned.conf";
pub const SNOOZE_FILE: &str = ".snooze_until";
pub const COUNTDOWN_PID_FILE: &str = ".countdown.pid";
pub const DAEMON_PID_FILE: &str = ".daemon.pid";
pub const BLACKBOX_FILE: &str = ".blackbox";
pub const BLACKBOX_BAK: &str = ".blackbox.bak";
pub const CALIB_STATE_FILE: &str = ".calibrating";
pub const CALIB_SAMPLES_FILE: &str = "calib_samples.csv";
pub const CALIB_CURVE_FILE: &str = "curve.conf";
pub const KEYNODE_CACHE: &str = ".keynode";

pub struct Daemon {
    pub cfg: Config,
    pub learned: Learned,
    pub curve: [i32; 11],
    pub custom_curve_ok: bool,

    pub moddir: PathBuf,
    pub bat_dir: PathBuf,
    pub usb_type_path: PathBuf,
    pub bl_node: Option<PathBuf>,

    pub clean_exit: bool,
    pub prev_level: Option<i32>,
    pub counting_down: bool,
    pub reload_pending: bool,

    // 充电 dumpsys 一次性标志
    pub usb1: bool,
    pub usb2: bool,
    pub last_charge_mode: i32,
    // 充电平滑
    pub chg_start_bb: i32,
    pub chg_start_lvl: Option<i32>,
    pub chg_lvl: Option<i32>,
    pub stuck: i32,

    // 低电提醒 / 关机
    pub warn_stage: i32,
    pub heavy_sag_warned: bool,
    pub low_v_streak: i32,
    pub rebound_streak: i32,
    pub rebound_start_up: u64,
    pub grace_notified: bool,
    pub in_danger_zone: bool,
    pub loop_cnt: i32,

    // 倒计时状态机（进程内，取代 v5.52 的 sleep_pid 子进程）
    pub cd_remaining: i32,
    pub cd_rapid_hits: i32,
    pub cd_rapid_done: bool,
    pub cd_recover_hits: i32,
    pub cd_recover_v: i32,
    pub cd_rapid_v: i32,
    pub cd_key_snooze_sec: i32,
    pub key_reader: Option<crate::platform::evdev::KeyReader>,

    // 满电学习
    pub max_learn_noted: bool,
    pub full_sat_accum: i32,
    pub full_sat_last_up: u64,
    pub full_100_accum: i32,
    pub true_full_armed: bool,
    pub max_learn_pending: bool,
    pub unplug_up: u64,

    // 历史
    pub last_hist_uptime: i64,
    pub hist_lines_cnt: i64,

    // 滤波
    pub raw_win: FilterWindow,
    pub cmp_win: FilterWindow,

    // 内阻学习
    pub prev_r_v: i32,
    pub prev_r_i: i32,
    pub prev_r_up: u64,
    pub r_init_cnt: i32,
    pub r_init1: i32,
    pub r_init2: i32,
    pub r_init3: i32,
    pub r_low_init_cnt: i32,
    pub r_low_init1: i32,
    pub r_low_init2: i32,
    pub r_low_init3: i32,

    // OCV
    pub ocv_init: i32,
    pub ocv_active: bool,

    // 传感器采样结果
    pub curr_valid: bool,
    pub temp_valid: bool,
    pub ir_active: bool,
    pub disch_ma: i32,
    pub chg_ma: i32,
    pub temp_c10: i32,
    pub temp_boost_uv: i32,
    pub temp_hyst_c10: i32,
    pub eff_shutdown_v: i32,
    pub active_r_mohm: i32,
    pub v_comp: i32,
    pub ma_vote_cnt: i32,

    // 黑匣子节流
    pub last_bb_v: i32,
    pub last_bb_i: i32,
    pub last_bb_up: u64,

    // 校准状态机
    pub calib_phase: i32,
    pub calib_samples: i32,
    pub calib_start_pct: i32,
    pub calib_min_v: i32,
    pub calib_max_v: i32,
    pub calib_last_up: i32,
    pub calib_buckets: i32,
    pub calib_min_pct: i32,
    pub calib_max_pct: i32,
    pub calib_span: i32,

    // 主循环快照
    pub charge_counter_path: PathBuf,
    pub f: i32,
    pub c: i32,
    pub bb: i32,
    pub usbtype: String,
    pub bst: String,
    pub u: i32,
    pub o: i32,
    pub curr: i32,
    pub last: i32,
    pub charging_now: bool,
}

impl Daemon {
    pub fn new(moddir: PathBuf) -> Self {
        let cfg = Config::load(&moddir.join(CONFIG_FILE));
        let learned = Learned::load(&moddir.join(LEARNED_FILE));
        let bat_dir = sysfs::detect_bat_node(&moddir);
        let usb_type_path = PathBuf::from(SYS_USB_TYPE_DEFAULT);
        let bl_node = sysfs::detect_backlight_node();
        let charge_counter_path = bat_dir.join("charge_counter");
        let f = cfg.max_v;
        let c = cfg.min_v;
        let hist_lines_cnt = count_lines(&moddir.join(telemetry::HIST_FILE));
        Daemon {
            cfg,
            learned,
            curve: [0; 11],
            custom_curve_ok: false,
            moddir,
            bat_dir,
            usb_type_path,
            bl_node,
            clean_exit: false,
            prev_level: None,
            counting_down: false,
            reload_pending: false,
            usb1: false,
            usb2: false,
            last_charge_mode: -1,
            chg_start_bb: 0,
            chg_start_lvl: None,
            chg_lvl: None,
            stuck: 0,
            warn_stage: 0,
            heavy_sag_warned: false,
            low_v_streak: 0,
            rebound_streak: 0,
            rebound_start_up: 0,
            grace_notified: false,
            in_danger_zone: false,
            loop_cnt: 0,
            cd_remaining: 0,
            cd_rapid_hits: 0,
            cd_rapid_done: false,
            cd_recover_hits: 0,
            cd_recover_v: 0,
            cd_rapid_v: 0,
            cd_key_snooze_sec: 0,
            key_reader: None,
            max_learn_noted: false,
            full_sat_accum: 0,
            full_sat_last_up: 0,
            full_100_accum: 0,
            true_full_armed: false,
            max_learn_pending: false,
            unplug_up: 0,
            last_hist_uptime: -999,
            hist_lines_cnt,
            raw_win: FilterWindow::new(),
            cmp_win: FilterWindow::new(),
            prev_r_v: 0,
            prev_r_i: 0,
            prev_r_up: 0,
            r_init_cnt: 0,
            r_init1: 0,
            r_init2: 0,
            r_init3: 0,
            r_low_init_cnt: 0,
            r_low_init1: 0,
            r_low_init2: 0,
            r_low_init3: 0,
            ocv_init: 0,
            ocv_active: false,
            curr_valid: false,
            temp_valid: false,
            ir_active: false,
            disch_ma: 0,
            chg_ma: 0,
            temp_c10: 250,
            temp_boost_uv: 0,
            temp_hyst_c10: 250,
            eff_shutdown_v: 0,
            active_r_mohm: 0,
            v_comp: 0,
            ma_vote_cnt: 0,
            last_bb_v: 0,
            last_bb_i: 0,
            last_bb_up: 0,
            calib_phase: 0,
            calib_samples: 0,
            calib_start_pct: 0,
            calib_min_v: 0,
            calib_max_v: 0,
            calib_last_up: 0,
            calib_buckets: 0,
            calib_min_pct: 0,
            calib_max_pct: 0,
            calib_span: 0,
            charge_counter_path,
            f,
            c,
            bb: 0,
            usbtype: String::new(),
            bst: String::new(),
            u: 0,
            o: 0,
            curr: 0,
            last: 0,
            charging_now: false,
        }
    }

    // ------------------------------------------------------------------
    // 通用辅助
    // ------------------------------------------------------------------

    pub fn save_learned(&self) {
        let _ = self.learned.write_file(&self.moddir.join(LEARNED_FILE));
    }

    pub fn save_config(&self) {
        let _ = self.cfg.write_file(&self.moddir.join(CONFIG_FILE));
    }

    pub fn log_event(&self, ty: &str, msg: &str) {
        telemetry::log_event(&self.moddir, ty, msg);
    }

    pub fn notifi(&self, title: &str, msg: &str) {
        android::notifi(title, msg);
    }

    pub fn danmu(&self, msg: &str) {
        android::danmu(msg);
    }

    pub fn get_uptime(&self) -> u64 {
        sysfs::get_uptime_sec()
    }

    pub fn mod_path(&self, name: &str) -> PathBuf {
        self.moddir.join(name)
    }

    // ------------------------------------------------------------------
    // 传感器读取（对应 daemon.sh read_sensors）
    // ------------------------------------------------------------------

    pub fn read_sensors(&mut self, chg_flag: i32) {
        self.curr_valid = false;
        self.disch_ma = 0;
        self.chg_ma = 0;

        let raw_i = sysfs::read_node_str(&self.bat_dir.join("current_now"), "");
        if let Some(raw) = parse_int(&raw_i).and_then(|v| i32::try_from(v).ok()) {
            let abs_i = raw.abs();
            // 单位判定
            let eff_unit: String = if self.cfg.curr_unit_ovr == 1 {
                "uA".to_string()
            } else if self.cfg.curr_unit_ovr == 2 {
                "mA".to_string()
            } else if abs_i > 25000 {
                self.ma_vote_cnt = 0;
                if self.learned.curr_unit != "uA" {
                    if self.learned.curr_unit == "mA" {
                        self.learned.r_int_mohm = 80;
                        self.learned.r_samples = 0;
                        self.learned.r_low_mohm = 0;
                        self.learned.r_low_samples = 0;
                    }
                    self.learned.curr_unit = "uA".to_string();
                    self.save_learned();
                }
                self.learned.curr_unit.clone()
            } else if self.learned.curr_unit == "auto" && abs_i >= 150 && abs_i <= 12000 {
                self.ma_vote_cnt += 1;
                if self.ma_vote_cnt >= 5 {
                    self.learned.curr_unit = "mA".to_string();
                    self.save_learned();
                }
                self.learned.curr_unit.clone()
            } else {
                self.learned.curr_unit.clone()
            };

            let ma = if eff_unit == "uA" {
                abs_i / 1000
            } else if eff_unit == "mA" {
                abs_i
            } else {
                0
            };

            // 符号判定
            let eff_sign: i32 = if self.cfg.curr_sign_ovr == 1 {
                1
            } else if self.cfg.curr_sign_ovr == 2 {
                -1
            } else {
                if eff_unit != "auto" && ma >= 120 && raw != 0 {
                    let detected = if chg_flag == 1 {
                        if raw > 0 {
                            -1
                        } else {
                            1
                        }
                    } else if raw > 0 {
                        1
                    } else {
                        -1
                    };
                    if self.learned.disch_sign == 0 && detected != 0 {
                        self.learned.disch_sign = detected;
                        self.save_learned();
                    }
                }
                self.learned.disch_sign
            };

            if eff_unit != "auto" && eff_sign != 0 {
                self.curr_valid = true;
                let signed_ma = (if raw < 0 { -ma } else { ma }) * eff_sign;
                if signed_ma >= 0 {
                    self.disch_ma = signed_ma;
                    self.chg_ma = 0;
                } else {
                    self.disch_ma = 0;
                    self.chg_ma = -signed_ma;
                }
            }
        }

        // 温度
        self.temp_valid = false;
        self.temp_c10 = 250;
        let raw_t = sysfs::read_node_str(&self.bat_dir.join("temp"), "");
        if let Some(raw) = parse_int(&raw_t).and_then(|v| i32::try_from(v).ok()) {
            let v = if raw > 1000 || raw < -1000 {
                raw / 100
            } else {
                raw
            };
            if v >= -300 && v <= 800 {
                self.temp_c10 = v;
                self.temp_valid = true;
            }
        }
    }

    // ------------------------------------------------------------------
    // 电压归一化（对应 daemon.sh normalize_voltage）
    // ------------------------------------------------------------------

    pub fn normalize_voltage(&mut self, raw: i32) -> i32 {
        if raw > 5_500_000 && raw < 10_000_000 {
            if self.learned.cell_div != 2 {
                self.learned.cell_div = 2;
                self.save_learned();
            }
        } else if raw > 2_000_000 && raw <= 5_000_000 {
            if self.learned.cell_div != 1 {
                self.learned.cell_div = 1;
                self.save_learned();
            }
        }
        if self.learned.cell_div == 2 {
            raw / 2
        } else {
            raw
        }
    }
}

impl Daemon {
    /// 复位滤波窗与内阻学习窗（对应 reset_voltage_windows）。
    pub fn reset_voltage_windows(&mut self) {
        self.raw_win.reset();
        self.cmp_win.reset();
        self.prev_r_v = 0;
        self.prev_r_i = 0;
        self.prev_r_up = 0;
        self.r_init_cnt = 0;
        self.r_init1 = 0;
        self.r_init2 = 0;
        self.r_init3 = 0;
        self.r_low_init_cnt = 0;
        self.r_low_init1 = 0;
        self.r_low_init2 = 0;
        self.r_low_init3 = 0;
    }

    /// 双轨 3 点滑动中值滤波（对应 push_dual_samples）。
    pub fn push_dual_samples(&mut self, u_raw: i32, u_cmp: i32) {
        self.raw_win.push(u_raw);
        self.cmp_win.push(u_cmp);
    }

    pub fn is_screen_off(&self) -> bool {
        sysfs::is_screen_off(self.bl_node.as_deref())
    }

    /// 平滑设置系统电量（对应 smooth_set_level，STEP=1）。
    pub fn smooth_set_level(&mut self, target: i32) {
        match self.prev_level {
            None => {
                self.prev_level = Some(target);
                self.rebound_streak = 0;
                self.rebound_start_up = 0;
                android::dumpsys_set_level(target);
                return;
            }
            Some(prev) => {
                let diff = target - prev;
                if diff < 0 {
                    self.rebound_streak = 0;
                    self.rebound_start_up = 0;
                    self.prev_level = Some(if -diff > 1 { prev - 1 } else { target });
                } else if diff > 0 {
                    if sysfs::is_charging_now(&self.usbtype, &self.bst) {
                        self.rebound_streak = 0;
                        self.rebound_start_up = 0;
                        self.prev_level = Some(if diff > 1 { prev + 1 } else { target });
                    } else if self.cfg.allow_rebound == 1 && diff >= 3 {
                        let up = self.get_uptime();
                        if self.rebound_start_up == 0 || self.rebound_start_up > up {
                            self.rebound_start_up = up;
                        }
                        if up - self.rebound_start_up >= 45 {
                            self.prev_level = Some(prev + 1);
                            self.rebound_start_up = up - 30;
                        }
                    } else {
                        self.rebound_streak = 0;
                        self.rebound_start_up = 0;
                    }
                } else {
                    self.rebound_streak = 0;
                    self.rebound_start_up = 0;
                }
            }
        }
        android::dumpsys_set_level(self.prev_level.unwrap_or(target));
    }

    /// 拔电即时首帧显示 + 4 秒平稳校准（对应 outusb）。
    pub fn outusb(&mut self) {
        self.reset_voltage_windows();

        let usb = sysfs::read_node_str(&self.usb_type_path, "Unknown");
        let st = sysfs::read_node_str(&self.bat_dir.join("status"), "");
        if sysfs::is_charging_now(&usb, &st) {
            return;
        }

        self.read_sensors(0);
        let o = self.normalize_voltage(sysfs::read_node_i32(&self.bat_dir.join("voltage_now"), 0));
        let ocv_o = self.normalize_voltage(sysfs::read_node_i32(&self.bat_dir.join("voltage_ocv"), 0));
        self.calc_compensated_voltage(o, ocv_o);
        let mut sum = self.v_comp;
        let mut min_v = self.v_comp;
        let mut max_v = self.v_comp;
        self.push_dual_samples(o, self.v_comp);
        let init_raw = crate::algo::soc::calc_battery_pct(
            self.v_comp,
            self.f,
            self.c,
            self.cfg.curve_mode,
            &self.curve,
            self.custom_curve_ok,
        );
        let init_bat = crate::algo::soc::apply_high_soc_floor(init_raw, self.bb, self.cfg.curve_mode);
        self.prev_level = Some(init_bat);
        android::dumpsys_set_level(init_bat);

        for _ in 2..=5 {
            std::thread::sleep(std::time::Duration::from_secs(1));
            let usb2 = sysfs::read_node_str(&self.usb_type_path, "Unknown");
            let st2 = sysfs::read_node_str(&self.bat_dir.join("status"), "");
            if sysfs::is_charging_now(&usb2, &st2) {
                self.prev_level = None;
                return;
            }
            self.read_sensors(0);
            let o2 = self.normalize_voltage(sysfs::read_node_i32(&self.bat_dir.join("voltage_now"), 0));
            let ocv_o2 = self.normalize_voltage(sysfs::read_node_i32(&self.bat_dir.join("voltage_ocv"), 0));
            self.update_internal_resistance(o2);
            self.calc_compensated_voltage(o2, ocv_o2);
            sum += self.v_comp;
            if self.v_comp < min_v {
                min_v = self.v_comp;
            }
            if self.v_comp > max_v {
                max_v = self.v_comp;
            }
            self.push_dual_samples(o2, self.v_comp);
        }
        self.danmu("拔电电量已平稳校准");

        let avg = (sum - min_v - max_v + 1) / 3;
        let bat5_raw = crate::algo::soc::calc_battery_pct(
            avg,
            self.f,
            self.c,
            self.cfg.curve_mode,
            &self.curve,
            self.custom_curve_ok,
        );
        let bat5 = crate::algo::soc::apply_high_soc_floor(bat5_raw, self.bb, self.cfg.curve_mode);
        self.prev_level = Some(bat5);
        self.rebound_streak = 0;
        self.rebound_start_up = 0;
        android::dumpsys_set_level(bat5);
    }

    /// 充电平滑电量计算（对应 calc_smooth_charge_level）。
    pub fn calc_smooth_charge_level(&mut self, u_now: i32, real_bb: i32, bst_now: &str, cc_val: i32) {
        if real_bb >= 100 || bst_now == "Full" {
            self.chg_lvl = Some(100);
            self.stuck = 0;
            return;
        }
        if self.chg_lvl.is_none() {
            let init = match self.prev_level {
                Some(p) if p > real_bb => p,
                _ => real_bb,
            };
            self.chg_lvl = Some(init);
            self.chg_start_bb = real_bb;
            self.chg_start_lvl = Some(init);
            self.stuck = 0;
            return;
        }
        let chg_lvl = self.chg_lvl.unwrap();
        if real_bb >= chg_lvl {
            self.chg_lvl = Some(real_bb);
            self.stuck = 0;
            return;
        }
        if real_bb >= 2 || cc_val > 0 {
            self.stuck = 0;
            let mut interp = chg_lvl;
            if real_bb > self.chg_start_bb && self.chg_start_bb < 100 {
                let start_lvl = self.chg_start_lvl.unwrap_or(0);
                interp = start_lvl
                    + (100 - start_lvl) * (real_bb - self.chg_start_bb)
                        / (100 - self.chg_start_bb);
            }
            if interp > 99 {
                interp = 99;
            }
            if interp > chg_lvl {
                self.chg_lvl = Some(chg_lvl + 1);
            }
            return;
        }
        self.stuck += 1;
        if self.stuck >= 6 && self.stuck % 3 == 0 {
            let mut chg_rise = 250_000;
            if self.curr_valid && self.chg_ma > 0 {
                let mut r_chg = self.learned.r_int_mohm;
                if r_chg < 40 {
                    r_chg = 80;
                }
                chg_rise = self.chg_ma * r_chg + 25_000;
                if chg_rise < 80_000 {
                    chg_rise = 80_000;
                }
                if chg_rise > 500_000 {
                    chg_rise = 500_000;
                }
            }
            let comp_v = u_now - chg_rise;
            let est_pct = crate::algo::soc::calc_battery_pct(
                comp_v,
                self.f,
                self.c,
                self.cfg.curve_mode,
                &self.curve,
                self.custom_curve_ok,
            );
            let est_pct = if est_pct > 95 { 95 } else { est_pct };
            if est_pct > chg_lvl {
                self.chg_lvl = Some(chg_lvl + 1);
            }
        }
    }

    /// 写 .runtime_state 与 history.csv（对应 record_runtime_and_history）。
    pub fn record_runtime_and_history(&mut self, u_raw: i32, u_flt: i32, o_val: i32, sim_l: i32, real_b: i32, chg_flag: i32) {
        let cd_active = if self.counting_down { 1 } else { 0 };
        let (snooze_rem, snooze_canceled) = self.get_snooze_remaining();
        let curr_disp_ma = if self.curr_valid {
            if chg_flag == 1 {
                -self.chg_ma
            } else {
                self.disch_ma
            }
        } else {
            0
        };
        let bl_ok = if self.bl_node.is_some() { 1 } else { 0 };
        let pend_elapsed = if self.max_learn_pending {
            (self.get_uptime().saturating_sub(self.unplug_up)) as i32
        } else {
            0
        };

        let content = format!(
            "SIM_LEVEL={}\nFILTERED_V={}\nCOMP_V={}\nSTREAK={}\nCHARGING={}\nCOUNTDOWN_ACTIVE={}\nSNOOZE_REM={}\nSNOOZE_CANCELED={}\nDISCH_MA={}\nCHG_MA={}\nCURR_SIGNED_MA={}\nCURR_VALID={}\nTEMP_C10={}\nTEMP_VALID={}\nTEMP_BOOST_UV={}\nEFF_SHUTDOWN_V={}\nDANGER_ZONE={}\nMAX_LEARN_ARMED={}\nMAX_LEARN_PENDING={}\nMAX_LEARN_ACCUM={}\nMAX_LEARN_PEND_SEC={}\nBL_OK={}\nIR_ACTIVE={}\nACTIVE_R_MOHM={}\nOCV_ACTIVE={}\nCALIB_PHASE={}\nCALIB_SAMPLES={}\nCALIB_START_PCT={}\nCALIB_MIN_V={}\nCALIB_MAX_V={}\nCALIB_BUCKETS={}\nCALIB_MIN_PCT={}\nCALIB_MAX_PCT={}\nCALIB_SPAN={}\n",
            sim_l,
            u_flt,
            self.v_comp,
            self.low_v_streak,
            chg_flag,
            cd_active,
            snooze_rem,
            if snooze_canceled { 1 } else { 0 },
            self.disch_ma,
            self.chg_ma,
            curr_disp_ma,
            if self.curr_valid { 1 } else { 0 },
            self.temp_c10,
            if self.temp_valid { 1 } else { 0 },
            self.temp_boost_uv,
            self.eff_shutdown_v,
            if self.in_danger_zone { 1 } else { 0 },
            if self.true_full_armed { 1 } else { 0 },
            if self.max_learn_pending { 1 } else { 0 },
            self.full_sat_accum,
            pend_elapsed,
            bl_ok,
            if self.ir_active { 1 } else { 0 },
            self.active_r_mohm,
            if self.ocv_active { 1 } else { 0 },
            self.calib_phase,
            self.calib_samples,
            self.calib_start_pct,
            self.calib_min_v,
            self.calib_max_v,
            self.calib_buckets,
            self.calib_min_pct,
            self.calib_max_pct,
            self.calib_span
        );
        let tmp = self.moddir.join(format!("{}.tmp", telemetry::STATE_FILE));
        let _ = std::fs::write(&tmp, &content);
        let _ = std::fs::rename(&tmp, self.moddir.join(telemetry::STATE_FILE));

        let uptime = self.get_uptime() as i64;
        if uptime - self.last_hist_uptime >= 120 || self.last_hist_uptime < 0 {
            self.last_hist_uptime = uptime;
            let ts = &android::now_ts_str()[11..16];
            let line = format!(
                "{},{},{},{},{},{},{}",
                ts, u_raw, self.v_comp, sim_l, real_b, chg_flag, curr_disp_ma
            );
            let _ = telemetry::append_csv(&self.moddir, telemetry::HIST_FILE, &line);
            self.hist_lines_cnt += 1;
            if self.hist_lines_cnt >= 400 {
                let p = self.moddir.join(telemetry::HIST_FILE);
                if let Ok(t) = std::fs::read_to_string(&p) {
                    let lines: Vec<&str> = t.lines().collect();
                    let start = lines.len().saturating_sub(360);
                    let trimmed: String = lines[start..].iter().map(|l| format!("{}\n", l)).collect();
                    let _ = std::fs::write(&p, trimmed);
                    self.hist_lines_cnt = 360;
                }
            }
        }
    }

    /// 详细日志写入（2MB 轮转，对应 write_bat_log）。
    pub fn write_bat_log(&self, u_raw: i32, o_val: i32, lbat_val: i32, disp_l: i32) {
        let log_dir = self.moddir.join("log");
        let _ = std::fs::create_dir_all(&log_dir);
        let log_file = log_dir.join("电压日志.log");
        if log_file.is_file() {
            if let Ok(meta) = std::fs::metadata(&log_file) {
                if meta.len() >= 2_097_152 {
                    let _ = std::fs::rename(&log_file, log_dir.join("电压日志.log.old"));
                }
            }
        }
        let cc_now = sysfs::read_node_str(&self.charge_counter_path, "0");
        let text = format!(
            "[{}]\n界面显示电量 {} (充电模式:{}, 曲线:{}, 补偿生效:{}, 电芯:{}S)\n算法映射电量 {} | 系统真实电量 {}\n实时电压 {} | 原始滤波 {} | 内阻补偿 {} (常规R={}mΩ[{}], 低电R={}mΩ[{}])\n开路电压 {} (活跃:{}) | 温度 {}(0.1C) | 动态红线 {} (低温+{})\n标定范围 {}~{} | 放电 {}mA / 充电 {}mA (单位:{} 符号:{})\n剩余mAh {} | 连接 {} ({}) | 危险区:{} | 连击 {}/{} | 满电累积:{}s/{}/{}\n",
            android::now_ts_str(),
            disp_l,
            self.cfg.charge_mode,
            self.cfg.curve_mode,
            if self.ir_active { 1 } else { 0 },
            self.learned.cell_div,
            lbat_val,
            self.bb,
            u_raw,
            self.raw_win.flt,
            self.v_comp,
            self.learned.r_int_mohm,
            self.learned.r_samples,
            self.learned.r_low_mohm,
            self.learned.r_low_samples,
            o_val,
            if self.ocv_active { 1 } else { 0 },
            self.temp_c10,
            self.eff_shutdown_v,
            self.temp_boost_uv,
            self.cfg.min_v,
            self.cfg.max_v,
            self.disch_ma,
            self.chg_ma,
            self.learned.curr_unit,
            self.learned.disch_sign,
            cc_now,
            self.usbtype,
            self.bst,
            if self.in_danger_zone { 1 } else { 0 },
            self.low_v_streak,
            self.cfg.shutdown_confirm,
            self.full_sat_accum,
            if self.true_full_armed { 1 } else { 0 },
            if self.max_learn_pending { 1 } else { 0 }
        );
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&log_file) {
            let _ = writeln!(f, "{}", text);
        }
    }

    /// 主流水线周期（秒），对应 smart_wait 的分支。
    pub fn pipeline_period(&self) -> u64 {
        if self.in_danger_zone && !self.charging_now {
            3
        } else if self.is_screen_off() {
            (self.cfg.screen_off_time as u64).max(1)
        } else {
            (self.cfg.time as u64).max(1)
        }
    }

    /// 亮屏放电时的快速传感器/内阻采样节奏（对应 smart_wait 内层循环）。
    pub fn fast_enabled(&self) -> bool {
        !self.charging_now && !self.is_screen_off() && !self.in_danger_zone
    }

    pub fn run_fast_tick(&mut self) {
        self.read_sensors(0);
        let u = self.normalize_voltage(sysfs::read_node_i32(&self.bat_dir.join("voltage_now"), 0));
        self.update_internal_resistance(u);
    }

    /// 启动初始化（对应 daemon.sh 1637-1705 的启动序列）。
    pub fn startup(&mut self) {
        self.calib_load_state();
        self.load_custom_curve();
        self.f = self.cfg.max_v;
        self.c = self.cfg.min_v;

        // 清理旧实例（按 pidfile）
        if let Ok(s) = std::fs::read_to_string(self.moddir.join(DAEMON_PID_FILE)) {
            if let Ok(pid) = s.trim().parse::<i32>() {
                let mypid = std::process::id() as i32;
                if pid != mypid && pid > 1 {
                    unsafe {
                        libc::kill(pid, libc::SIGTERM);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    unsafe {
                        libc::kill(pid, libc::SIGKILL);
                    }
                }
            }
        }

        let pid = std::process::id();
        let _ = std::fs::write(self.moddir.join(DAEMON_PID_FILE), format!("{}\n", pid));
        let _ = std::fs::remove_file(self.moddir.join(COUNTDOWN_PID_FILE));

        let u0 = self.normalize_voltage(sysfs::read_node_i32(&self.bat_dir.join("voltage_now"), 0));
        self.u = u0;
        self.audit_blackbox_on_boot();

        self.log_event("服务", &format!("电压模拟服务 v6.1 已启动（PID {}，盲操续命/事件记录/引导校准已就绪）", pid));
        self.notifi("电压服务", "电压模拟服务 v6.1 已启动");

        self.bb = sysfs::read_node_i32(&self.bat_dir.join("capacity"), 0);
        self.usbtype = sysfs::read_node_str(&self.usb_type_path, "Unknown");
        self.bst = sysfs::read_node_str(&self.bat_dir.join("status"), "");

        if sysfs::is_charging_now(&self.usbtype, &self.bst) {
            android::dumpsys_reset();
            self.prev_level = None;
        } else {
            if self.bb <= 0 {
                android::dumpsys_unplug();
                android::dumpsys_set_status(3);
            } else {
                android::dumpsys_reset();
                android::dumpsys_set_level(self.bb);
            }
            self.outusb();
        }

        self.curr = sysfs::read_node_i32(&self.charge_counter_path, 0);
        self.last = self.curr;
    }

    /// 主流水线（对应 daemon.sh 主循环体 1710-1907）。
    pub fn run_pipeline(&mut self) {
        self.loop_cnt = self.loop_cnt.wrapping_add(1);
        let signal_wake = self.reload_pending;
        if self.reload_pending || self.loop_cnt % 30 == 0 {
            self.cfg = Config::load(&self.moddir.join(CONFIG_FILE));
            self.learned = Learned::load(&self.moddir.join(LEARNED_FILE));
            self.calib_load_state();
            self.load_custom_curve();
            self.reload_pending = false;
        }
        self.f = self.cfg.max_v;
        self.c = self.cfg.min_v;

        self.usbtype = sysfs::read_node_str(&self.usb_type_path, "Unknown");
        self.u = self.normalize_voltage(sysfs::read_node_i32(&self.bat_dir.join("voltage_now"), 0));
        self.o = self.normalize_voltage(sysfs::read_node_i32(&self.bat_dir.join("voltage_ocv"), 0));
        self.bb = sysfs::read_node_i32(&self.bat_dir.join("capacity"), 0);
        self.bst = sysfs::read_node_str(&self.bat_dir.join("status"), "");
        self.curr = sysfs::read_node_i32(&self.charge_counter_path, 0);

        let mut lbat: i32 = 0;
        let display_lvl: i32;

        if !sysfs::is_charging_now(&self.usbtype, &self.bst) {
            // ---- 放电分支 ----
            self.charging_now = false;
            self.read_sensors(0);
            self.calc_effective_shutdown_v();

            if self.usb1 || self.usb2 {
                // 拔电沿
                android::dumpsys_unplug();
                android::dumpsys_set_status(3);
                self.usb1 = false;
                self.usb2 = false;
                self.last_charge_mode = -1;
                self.chg_lvl = None;
                self.stuck = 0;
                self.prev_level = None;
                self.low_v_streak = 0;
                self.rebound_streak = 0;
                self.rebound_start_up = 0;
                self.warn_stage = 0;
                self.heavy_sag_warned = false;
                self.grace_notified = false;

                self.calib_on_unplug();

                if self.true_full_armed && self.cfg.auto_max_learn == 1 {
                    self.unplug_up = self.get_uptime();
                    self.max_learn_pending = true;
                    self.max_learn_noted = false;
                    self.log_event("满电学习", "真满电已就绪，开始静置采样窗口（45s~15min）");
                }
                self.true_full_armed = false;
                self.full_sat_accum = 0;
                self.full_sat_last_up = 0;
                self.full_100_accum = 0;

                self.outusb();
                self.u = self.normalize_voltage(sysfs::read_node_i32(&self.bat_dir.join("voltage_now"), 0));
                self.o = self.normalize_voltage(sysfs::read_node_i32(&self.bat_dir.join("voltage_ocv"), 0));
                self.curr = sysfs::read_node_i32(&self.charge_counter_path, 0);
                self.last = self.curr;
                self.read_sensors(0);
            }

            self.update_internal_resistance(self.u);
            self.calc_compensated_voltage(self.u, self.o);
            self.push_dual_samples(self.u, self.v_comp);

            self.try_learn_resting_max_v();

            let comp_flt = self.cmp_win.flt;
            let bat_raw = crate::algo::soc::calc_battery_pct(
                comp_flt,
                self.f,
                self.c,
                self.cfg.curve_mode,
                &self.curve,
                self.custom_curve_ok,
            );
            lbat = bat_raw;
            let bat = crate::algo::soc::apply_high_soc_floor(bat_raw, self.bb, self.cfg.curve_mode);

            self.calib_tick_discharge(self.bb, comp_flt);

            if signal_wake {
                self.prev_level = Some(bat);
                self.rebound_streak = 0;
                self.rebound_start_up = 0;
                android::dumpsys_set_level(bat);
            }

            if self.curr != 0 {
                if self.curr != self.last || self.prev_level.is_none() {
                    self.smooth_set_level(bat);
                    self.last = self.curr;
                } else if self.cfg.allow_rebound == 1 && bat >= self.prev_level.unwrap_or(0) + 3 {
                    self.smooth_set_level(bat);
                } else {
                    android::dumpsys_set_level(self.prev_level.unwrap_or(0));
                }
            } else {
                self.smooth_set_level(bat);
            }

            display_lvl = self.prev_level.unwrap_or(self.bb);

            // 危险区判定
            let mut danger_enter_v = self.eff_shutdown_v + 250_000;
            if danger_enter_v < 3_450_000 {
                danger_enter_v = 3_450_000;
            }
            if self.u > 1_000_000 && self.u < danger_enter_v {
                self.in_danger_zone = true;
                self.write_blackbox_throttled(self.u, self.disch_ma, self.o, self.temp_c10, self.v_comp);
            } else if self.u >= danger_enter_v + 100_000 {
                self.in_danger_zone = false;
                let bb = self.moddir.join(BLACKBOX_FILE);
                let bkb = self.moddir.join(BLACKBOX_BAK);
                if bb.is_file() || bkb.is_file() {
                    let _ = std::fs::remove_file(&bb);
                    let _ = std::fs::remove_file(&bkb);
                    self.last_bb_v = 0;
                    self.last_bb_i = 0;
                    self.last_bb_up = 0;
                }
            }

            self.check_tiered_low_warning(display_lvl, self.raw_win.flt);
            self.check_voltage_shutdown(self.u, self.raw_win.flt, self.v_comp);
        } else {
            // ---- 充电分支 ----
            self.charging_now = true;
            self.in_danger_zone = false;
            self.max_learn_pending = false;
            self.reset_voltage_windows();
            self.low_v_streak = 0;
            self.rebound_streak = 0;
            self.rebound_start_up = 0;
            self.warn_stage = 0;
            self.heavy_sag_warned = false;
            self.grace_notified = false;
            self.read_sensors(1);
            self.calc_effective_shutdown_v();
            self.raw_win.flt = self.u;
            self.cmp_win.flt = self.u;
            self.v_comp = self.u;
            self.ir_active = false;
            lbat = self.bb;

            let _ = std::fs::remove_file(self.moddir.join(SNOOZE_FILE));
            let _ = std::fs::remove_file(self.moddir.join(BLACKBOX_FILE));
            let _ = std::fs::remove_file(self.moddir.join(BLACKBOX_BAK));
            self.last_bb_v = 0;
            self.last_bb_i = 0;
            self.last_bb_up = 0;

            if self.counting_down {
                self.stop_countdown();
                self.log_event("关机保护", "充电器已连接，自动取消关机");
                self.notifi("取消关机", "充电器已连接，已取消自动关机");
                self.danmu("充电器已连接，取消关机");
            }

            self.track_true_full_charge(self.u, self.bb, &self.bst);
            self.calib_tick_charge();

            if self.cfg.charge_mode != self.last_charge_mode || signal_wake {
                self.usb1 = false;
                self.usb2 = false;
                self.chg_lvl = None;
                self.stuck = 0;
                if self.cfg.charge_mode != self.last_charge_mode {
                    self.prev_level = None;
                }
                self.last_charge_mode = self.cfg.charge_mode;
            }

            match self.cfg.charge_mode {
                2 => {
                    if !self.usb1 {
                        android::dumpsys_reset();
                        self.usb1 = true;
                        self.prev_level = None;
                        self.chg_lvl = None;
                    }
                    display_lvl = self.bb;
                }
                0 => {
                    self.calc_smooth_charge_level(self.u, self.bb, &self.bst, self.curr);
                    if !self.usb1 {
                        android::dumpsys_set_ac(1);
                        android::dumpsys_set_status(2);
                        self.usb1 = true;
                    }
                    android::dumpsys_set_level(self.chg_lvl.unwrap_or(0));
                    display_lvl = self.chg_lvl.unwrap_or(0);
                }
                _ => {
                    self.prev_level = None;
                    self.chg_lvl = None;
                    if self.bb >= 1 {
                        if !self.usb1 {
                            android::dumpsys_set_ac(1);
                            android::dumpsys_set_status(2);
                            self.usb1 = true;
                        }
                        android::dumpsys_set_level(self.bb);
                    } else if !self.usb2 {
                        android::dumpsys_set_ac(1);
                        android::dumpsys_set_status(2);
                        self.usb2 = true;
                    }
                    display_lvl = self.bb;
                }
            }
        }

        self.record_runtime_and_history(
            self.u,
            self.raw_win.flt,
            self.o,
            display_lvl,
            self.bb,
            if self.charging_now { 1 } else { 0 },
        );

        if self.cfg.batlog == 1 {
            self.write_bat_log(self.u, self.o, lbat, display_lvl);
        }
    }

    /// 优雅退出（对应 stop 子命令 / SIGTERM）。
    fn graceful_stop(&mut self) -> ! {
        telemetry::record_exit_reason(&self.moddir, "stop", 0);
        let _ = std::fs::remove_file(self.moddir.join(DAEMON_PID_FILE));
        let _ = std::fs::remove_file(self.moddir.join(COUNTDOWN_PID_FILE));
        let _ = std::fs::remove_file(self.moddir.join(crate::ipc::IPC_SOCK_NAME));
        android::dumpsys_reset();
        std::process::exit(0);
    }

    fn handle_ipc(&mut self, cmd: &str) {
        let c = cmd.trim();
        if c == "wake" || c == "reload" {
            self.reload_pending = true;
        } else if c == "cancel_shutdown" {
            self.stop_countdown();
        } else if c == "stop" {
            self.graceful_stop();
        } else if let Some(rest) = c.strip_prefix("snooze ") {
            // CLI 已写 .snooze_until；此处仅终止进行中的倒计时。
            let _ = rest;
            self.stop_countdown();
        }
    }

    /// 事件循环入口（epoll + timerfd + netlink uevent + unix socket + signalfd）。
    pub fn run(&mut self) -> ! {
        self.startup();

        let epfd = epoll_create();
        let tfd = timerfd_create();
        let uev = crate::platform::uevent::UeventSocket::new();
        let sfd = signalfd_create();
        let ipc = crate::ipc::IpcListener::new(&self.moddir);

        epoll_add(epfd, tfd, libc::EPOLLIN as u32, 1);
        if let Some(ref u) = uev {
            epoll_add(epfd, u.fd(), libc::EPOLLIN as u32, 2);
        }
        epoll_add(epfd, sfd, libc::EPOLLIN as u32, 3);
        if let Some(ref s) = ipc {
            epoll_add(epfd, s.fd(), libc::EPOLLIN as u32, 4);
        }

        let mut next_pipeline: u64 = 0;
        let mut next_fast: u64 = 0;
        let mut next_countdown: u64 = 0;

        loop {
            let now = boottime_now();

            if now >= next_pipeline {
                self.run_pipeline();
                next_pipeline = now + self.pipeline_period();
            }
            if self.counting_down {
                if next_countdown == 0 {
                    next_countdown = now + 1;
                }
                if now >= next_countdown {
                    self.countdown_tick();
                    next_countdown = now + 1;
                }
            } else {
                next_countdown = 0;
            }
            if self.fast_enabled() {
                if next_fast == 0 {
                    next_fast = now + (self.cfg.usb_poll_step as u64).max(1);
                }
                if now >= next_fast {
                    self.run_fast_tick();
                    next_fast = now + (self.cfg.usb_poll_step as u64).max(1);
                }
            } else {
                next_fast = 0;
            }

            let mut deadline = next_pipeline;
            if self.counting_down && next_countdown < deadline {
                deadline = next_countdown;
            }
            if self.fast_enabled() && next_fast < deadline {
                deadline = next_fast;
            }
            let wait = deadline.saturating_sub(now);
            timerfd_arm(tfd, wait);

            let mut evs: [libc::epoll_event; 8] = unsafe { std::mem::zeroed() };
            let n = unsafe { libc::epoll_wait(epfd, evs.as_mut_ptr(), 8, -1) };
            timerfd_drain(tfd);
            for i in 0..n.max(0) as usize {
                let data = evs[i].u64;
                match data {
                    2 => {
                        if uev.as_ref().map(|u| u.drain()).unwrap_or(false) {
                            self.reload_pending = true;
                            next_pipeline = 0;
                        }
                    }
                    3 => {
                        for sig in signalfd_read(sfd) {
                            if sig == libc::SIGTERM || sig == libc::SIGINT {
                                self.graceful_stop();
                            } else if sig == libc::SIGUSR1 {
                                self.reload_pending = true;
                                next_pipeline = 0;
                            }
                        }
                    }
                    4 => {
                        if let Some(cmd) = ipc.as_ref().and_then(|s| s.accept_command()) {
                            if cmd == "wake" || cmd == "reload" {
                                next_pipeline = 0;
                            }
                            self.handle_ipc(&cmd);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

// ----------------------------------------------------------------------
// 事件循环底层：timerfd / epoll / signalfd
// ----------------------------------------------------------------------

fn boottime_now() -> u64 {
    unsafe {
        let mut ts: libc::timespec = std::mem::zeroed();
        libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut ts);
        ts.tv_sec as u64
    }
}

fn timerfd_create() -> i32 {
    unsafe { libc::timerfd_create(libc::CLOCK_BOOTTIME, libc::TFD_CLOEXEC | libc::TFD_NONBLOCK) }
}

fn timerfd_arm(fd: i32, secs: u64) {
    unsafe {
        let mut its: libc::itimerspec = std::mem::zeroed();
        if secs == 0 {
            // Linux timerfd 传入 0s/0ns 会关闭定时器导致 epoll_wait 永久挂起，改为 10ms 立即唤醒
            its.it_value.tv_sec = 0;
            its.it_value.tv_nsec = 10_000_000;
        } else {
            its.it_value.tv_sec = secs as libc::time_t;
            its.it_value.tv_nsec = 0;
        }
        libc::timerfd_settime(fd, 0, &its, std::ptr::null_mut());
    }
}

fn timerfd_drain(fd: i32) {
    unsafe {
        let mut buf = [0u8; 8];
        libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, 8);
    }
}

fn epoll_create() -> i32 {
    unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) }
}

fn epoll_add(epfd: i32, fd: i32, events: u32, data: u64) {
    unsafe {
        let mut ev: libc::epoll_event = std::mem::zeroed();
        ev.events = events;
        ev.u64 = data;
        libc::epoll_ctl(epfd, libc::EPOLL_CTL_ADD, fd, &mut ev);
    }
}

fn signalfd_create() -> i32 {
    unsafe {
        let mut mask: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut mask);
        libc::sigaddset(&mut mask, libc::SIGTERM);
        libc::sigaddset(&mut mask, libc::SIGINT);
        libc::sigaddset(&mut mask, libc::SIGUSR1);
        libc::sigprocmask(libc::SIG_BLOCK, &mask, std::ptr::null_mut());
        // 忽略 SIGPIPE，避免写已关闭 socket 时进程被杀
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
        libc::signalfd(-1, &mask, libc::SFD_CLOEXEC | libc::SFD_NONBLOCK)
    }
}

fn signalfd_read(fd: i32) -> Vec<i32> {
    let mut out = Vec::new();
    unsafe {
        let mut buf = [0u8; 64 * std::mem::size_of::<libc::signalfd_siginfo>()];
        let n = libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len());
        if n > 0 {
            let n = n as usize;
            let sz = std::mem::size_of::<libc::signalfd_siginfo>();
            let mut off = 0;
            while off + sz <= n {
                let si: libc::signalfd_siginfo =
                    std::ptr::read_unaligned(buf[off..].as_ptr() as *const _);
                out.push(si.ssi_signo as i32);
                off += sz;
            }
        }
    }
    out
}

fn count_lines(path: &std::path::Path) -> i64 {
    std::fs::read_to_string(path)
        .map(|s| s.lines().count() as i64)
        .unwrap_or(0)
}
