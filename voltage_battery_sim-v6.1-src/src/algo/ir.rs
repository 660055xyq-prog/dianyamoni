//! 内阻学习、补偿电压、动态红线（对应 daemon.sh update_internal_resistance /
//! calc_compensated_voltage / calc_effective_shutdown_v）。

use crate::algo::filter::median3;
use crate::daemon::Daemon;

impl Daemon {
    /// 内阻学习（daemon.sh:402-487）。
    pub fn update_internal_resistance(&mut self, u_now: i32) {
        if !self.curr_valid {
            return;
        }
        let uptime = self.get_uptime();

        // 低温冻结（<10°C）：只更新基准点
        if self.temp_valid && self.temp_c10 < 100 {
            self.prev_r_v = u_now;
            self.prev_r_i = self.disch_ma;
            self.prev_r_up = uptime;
            return;
        }

        if self.prev_r_v > 0 && self.prev_r_up > 0 {
            let dt = uptime as i64 - self.prev_r_up as i64;
            if dt >= 1 && dt <= 6 {
                let di = self.disch_ma - self.prev_r_i;
                let dv = self.prev_r_v - u_now;
                let abs_di = di.abs();
                if abs_di >= 350 {
                    let r_est = if di > 0 && dv > 0 {
                        dv / di
                    } else if di < 0 && dv < 0 {
                        (-dv) / (-di)
                    } else {
                        0
                    };
                    if r_est >= 25 && r_est <= 600 {
                        if u_now < 3_600_000 {
                            // 低电段 R_LOW
                            if self.learned.r_low_samples == 0 {
                                self.r_low_init_cnt += 1;
                                match self.r_low_init_cnt {
                                    1 => self.r_low_init1 = r_est,
                                    2 => self.r_low_init2 = r_est,
                                    _ => {
                                        self.r_low_init3 = r_est;
                                        self.learned.r_low_mohm = median3(
                                            self.r_low_init1,
                                            self.r_low_init2,
                                            self.r_low_init3,
                                        );
                                        self.learned.r_low_samples = 3;
                                        self.r_low_init_cnt = 0;
                                        self.save_learned();
                                    }
                                }
                            } else {
                                let mut ema = (self.learned.r_low_mohm * 8 + r_est * 2 + 5) / 10;
                                let base = self.learned.r_low_mohm;
                                ema = ema.clamp(base - 15, base + 15).clamp(25, 600);
                                self.learned.r_low_mohm = ema;
                                self.learned.r_low_samples += 1;
                            }
                        } else {
                            // 高电段 R_INT
                            if self.learned.r_samples == 0 {
                                self.r_init_cnt += 1;
                                match self.r_init_cnt {
                                    1 => self.r_init1 = r_est,
                                    2 => self.r_init2 = r_est,
                                    _ => {
                                        self.r_init3 = r_est;
                                        self.learned.r_int_mohm = median3(
                                            self.r_init1,
                                            self.r_init2,
                                            self.r_init3,
                                        );
                                        self.learned.r_samples = 3;
                                        self.r_init_cnt = 0;
                                        self.save_learned();
                                    }
                                }
                            } else {
                                let mut ema = (self.learned.r_int_mohm * 8 + r_est * 2 + 5) / 10;
                                let base = self.learned.r_int_mohm;
                                ema = ema.clamp(base - 15, base + 15).clamp(25, 600);
                                self.learned.r_int_mohm = ema;
                                self.learned.r_samples += 1;
                            }
                        }
                    }
                }
            }
        }

        // 每次调用都刷新基准点
        self.prev_r_v = u_now;
        self.prev_r_i = self.disch_ma;
        self.prev_r_up = uptime;
    }

    /// 内阻补偿电压（daemon.sh:490-548）。
    pub fn calc_compensated_voltage(&mut self, u_now: i32, o_now: i32) {
        self.ir_active = false;
        self.active_r_mohm = 0;
        self.v_comp = u_now;

        if o_now > 2_800_000 && o_now < 4_700_000 {
            if self.ocv_init == 0 {
                self.ocv_init = o_now;
            } else if o_now != self.ocv_init {
                self.ocv_active = true;
            }
        }

        if self.cfg.ir_comp_mode != 1 || !self.curr_valid || self.disch_ma <= 0 {
            return;
        }
        let total = self.learned.r_samples + self.learned.r_low_samples;
        if total < 3 {
            return;
        }

        let mut r_use = self.learned.r_int_mohm;
        if self.learned.r_samples == 0 && self.learned.r_low_samples > 0 {
            r_use = self.learned.r_low_mohm;
        } else if self.learned.r_low_samples >= 2 && self.learned.r_low_mohm > 0 {
            if u_now <= 3_550_000 {
                r_use = self.learned.r_low_mohm;
            } else if u_now >= 3_700_000 {
                r_use = self.learned.r_int_mohm;
            } else {
                let w_hi = (u_now - 3_550_000) / 1500;
                let w_lo = 100 - w_hi;
                r_use = (self.learned.r_int_mohm * w_hi + self.learned.r_low_mohm * w_lo + 50) / 100;
            }
        }

        if self.cfg.temp_comp == 1 && self.temp_valid {
            if self.temp_c10 < 0 {
                r_use = (r_use * 15 + 5) / 10;
            } else if self.temp_c10 < 100 {
                r_use = (r_use * 125 + 50) / 100;
            }
            if r_use > 750 {
                r_use = 750;
            }
        }

        self.active_r_mohm = r_use;
        self.ir_active = true;
        let mut boost = self.disch_ma * r_use;
        if boost > 450_000 {
            boost = 450_000;
        }
        self.v_comp = u_now + boost;

        if self.ocv_active && o_now > 2_800_000 && o_now < 4_700_000 {
            let ocv_cap = o_now + 150_000;
            if self.v_comp > ocv_cap {
                self.v_comp = ocv_cap;
            }
        }
    }

    /// 动态红线（daemon.sh:551-577）。
    pub fn calc_effective_shutdown_v(&mut self) {
        let mut base = self.cfg.shutdown_v;
        if self.cfg.auto_min_learn == 1 && self.learned.learned_floor_v > base {
            base = self.learned.learned_floor_v;
        }

        self.temp_boost_uv = 0;
        if self.cfg.temp_comp == 1 && self.temp_valid {
            if self.temp_c10 < self.temp_hyst_c10 {
                self.temp_hyst_c10 = self.temp_c10;
            } else if self.temp_c10 > self.temp_hyst_c10 + 15 {
                self.temp_hyst_c10 = self.temp_c10 - 15;
            }
            if self.temp_hyst_c10 < 150 {
                let raw_b = (150 - self.temp_hyst_c10) * 600;
                let mut boost = (raw_b + 2500) / 5000 * 5000;
                if boost > 120_000 {
                    boost = 120_000;
                }
                self.temp_boost_uv = boost;
            }
        } else {
            self.temp_hyst_c10 = 250;
        }

        let mut eff = base + self.temp_boost_uv;
        if eff > 3_650_000 {
            eff = 3_650_000;
        }
        self.eff_shutdown_v = eff;
    }
}
