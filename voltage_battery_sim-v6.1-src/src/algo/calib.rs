//! 引导校准状态机 + 曲线拟合（对应 daemon.sh calib_* 与 common.sh
//! calib_scan_progress / calc_curve_knots / apply_calibration_result）。

use crate::config::{uv_to_v2, Config};
use crate::daemon::{Daemon, CALIB_CURVE_FILE, CALIB_SAMPLES_FILE, CALIB_STATE_FILE};
use crate::sysfs;

/// 锂电先验归一化模板（千分比 0..1000）。
const PRIOR: [i64; 11] = [0, 321, 429, 486, 529, 571, 629, 700, 786, 879, 1000];

/// 整数中位数（对应 _calc_median_mv 的插入排序 + 中位数）。
fn median_mv(list: &[i64]) -> i64 {
    let mut a = list.to_vec();
    a.sort_unstable();
    let n = a.len();
    if n == 0 {
        return 0;
    }
    if n % 2 == 1 {
        a[n / 2]
    } else {
        let l = a[n / 2 - 1];
        let r = a[n / 2];
        (l + r + 1) / 2
    }
}

/// 校准进度结构（对应 calib_scan_progress 输出）。
#[derive(Default, Clone, Copy)]
pub struct CalibProgress {
    pub samples: i32,
    pub buckets: i32,
    pub min_pct: i32,
    pub max_pct: i32,
    pub span: i32,
}

impl Daemon {
    /// 快速统计当前采样进度（common.sh:601-627）。
    pub fn calib_scan_progress(&self) -> CalibProgress {
        let mut p = CalibProgress::default();
        let path = self.moddir.join(CALIB_SAMPLES_FILE);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(_) => return p,
        };
        let mut bucket_flag = [false; 11];
        let mut min_p = 101i32;
        let mut max_p = 0i32;
        for raw in text.lines() {
            let line = raw.trim_end_matches('\r');
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut parts = line.split(',');
            let pct = match parts.next().and_then(|s| s.parse::<i32>().ok()) {
                Some(v) => v,
                None => continue,
            };
            let v = match parts.next().and_then(|s| s.parse::<i32>().ok()) {
                Some(v) => v,
                None => continue,
            };
            if pct > 100 || v <= 2_000_000 {
                continue;
            }
            p.samples += 1;
            if pct < min_p {
                min_p = pct;
            }
            if pct > max_p {
                max_p = pct;
            }
            let mut bi = (pct + 5) / 10;
            if bi > 10 {
                bi = 10;
            }
            bucket_flag[bi as usize] = true;
        }
        if p.samples > 0 {
            p.min_pct = min_p;
            p.max_pct = max_p;
            p.span = max_p - min_p;
            p.buckets = bucket_flag.iter().filter(|&&b| b).count() as i32;
        }
        p
    }

    /// 加载校准状态（daemon.sh:915-941）。
    pub fn calib_load_state(&mut self) {
        self.calib_phase = 0;
        self.calib_samples = 0;
        self.calib_start_pct = 0;
        self.calib_min_v = 0;
        self.calib_max_v = 0;
        self.calib_last_up = 0;
        self.calib_buckets = 0;
        self.calib_min_pct = 0;
        self.calib_max_pct = 0;
        self.calib_span = 0;

        let path = self.moddir.join(CALIB_STATE_FILE);
        if !path.is_file() {
            return;
        }
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        for raw in text.lines() {
            let line = raw.trim_end_matches('\r');
            if let Some((k, v)) = line.split_once('=') {
                let val = v.trim().parse::<i32>().unwrap_or(-1);
                if val < 0 {
                    continue;
                }
                match k.trim() {
                    "PHASE" => self.calib_phase = val,
                    "SAMPLES" => self.calib_samples = val,
                    "START_PCT" => self.calib_start_pct = val,
                    "MIN_V" => self.calib_min_v = val,
                    "MAX_V" => self.calib_max_v = val,
                    "LAST_UP" => self.calib_last_up = val,
                    "BUCKETS" => self.calib_buckets = val,
                    "MIN_PCT" => self.calib_min_pct = val,
                    "MAX_PCT" => self.calib_max_pct = val,
                    "SPAN" => self.calib_span = val,
                    _ => {}
                }
            }
        }
        let cp = self.calib_scan_progress();
        if cp.samples > 0 {
            self.calib_samples = cp.samples;
            self.calib_buckets = cp.buckets;
            self.calib_min_pct = cp.min_pct;
            self.calib_max_pct = cp.max_pct;
            self.calib_span = cp.span;
        }
    }

    pub fn calib_save_state(&self) {
        let content = format!(
            "PHASE={}\nSAMPLES={}\nSTART_PCT={}\nMIN_V={}\nMAX_V={}\nLAST_UP={}\nBUCKETS={}\nMIN_PCT={}\nMAX_PCT={}\nSPAN={}\n",
            self.calib_phase,
            self.calib_samples,
            self.calib_start_pct,
            self.calib_min_v,
            self.calib_max_v,
            self.calib_last_up,
            self.calib_buckets,
            self.calib_min_pct,
            self.calib_max_pct,
            self.calib_span
        );
        let _ = std::fs::write(self.moddir.join(CALIB_STATE_FILE), &content);
    }

    pub fn calib_reset(&mut self) {
        self.calib_phase = 0;
        self.calib_samples = 0;
        self.calib_start_pct = 0;
        self.calib_min_v = 0;
        self.calib_max_v = 0;
        self.calib_last_up = 0;
        self.calib_buckets = 0;
        self.calib_min_pct = 0;
        self.calib_max_pct = 0;
        self.calib_span = 0;
        self.calib_save_state();
    }

    /// 放电分支每轮采集（daemon.sh:967-1015）。
    pub fn calib_tick_discharge(&mut self, disp_pct: i32, v_now: i32) {
        if self.cfg.calib_mode != 1 {
            if self.calib_phase != 0 {
                self.calib_reset();
            }
            return;
        }
        let uptime = self.get_uptime();
        if self.calib_phase != 1 {
            let was_paused = self.calib_phase;
            self.calib_phase = 1;
            if self.calib_start_pct == 0 {
                self.calib_start_pct = disp_pct;
            }
            self.calib_last_up = 0;
            self.calib_save_state();
            if was_paused == 2 {
                self.log_event(
                    "校准",
                    &format!(
                        "断点续采已恢复：当前 {}%（已有 {} 点 / {} 区间）",
                        disp_pct, self.calib_samples, self.calib_buckets
                    ),
                );
            } else {
                self.log_event("校准", &format!("开始极速采样：当前 {}%（累计 ≥25% 跨度即可生成专属曲线）", disp_pct));
            }
        }
        if self.calib_last_up as u64 > uptime {
            self.calib_last_up = 0;
        }
        if self.calib_last_up == 0 || uptime - self.calib_last_up as u64 >= 45 {
            if self.curr_valid && !self.ir_active && self.disch_ma > 1500 {
                return;
            }
            self.calib_last_up = uptime as i32;
            let mut pct = disp_pct;
            if pct > 100 {
                pct = 100;
            }
            if pct < 0 {
                pct = 0;
            }
            if pct == 0 && v_now > 3_500_000 {
                return;
            }
            let line = format!("{},{},{},{}", pct, v_now, uptime, self.disch_ma);
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.moddir.join(CALIB_SAMPLES_FILE))
            {
                use std::io::Write;
                let _ = writeln!(f, "{}", line);
            }
            if self.calib_min_v == 0 || v_now < self.calib_min_v {
                self.calib_min_v = v_now;
            }
            if v_now > self.calib_max_v {
                self.calib_max_v = v_now;
            }
            let cp = self.calib_scan_progress();
            self.calib_samples = cp.samples;
            self.calib_buckets = cp.buckets;
            self.calib_min_pct = cp.min_pct;
            self.calib_max_pct = cp.max_pct;
            self.calib_span = cp.span;
            self.calib_save_state();

            if pct <= 2 || v_now <= self.eff_shutdown_v + 20_000 {
                if self.calib_span >= 25 && self.calib_buckets >= 3 && self.calib_samples >= 6 {
                    self.log_event(
                        "校准",
                        &format!("放电到达低电区（{}% / {}V），自动生成专属曲线", pct, uv_to_v2(v_now)),
                    );
                    self.calib_generate("低电自动结算");
                }
            }
        }
    }

    /// 充电分支每轮结算（daemon.sh:1018-1032）。
    pub fn calib_tick_charge(&mut self) {
        if self.cfg.calib_mode == 1 && self.calib_phase == 1 {
            let cp = self.calib_scan_progress();
            self.calib_samples = cp.samples;
            self.calib_buckets = cp.buckets;
            self.calib_min_pct = cp.min_pct;
            self.calib_max_pct = cp.max_pct;
            self.calib_span = cp.span;
            if self.calib_span >= 50 && self.calib_buckets >= 5 && self.calib_samples >= 12 {
                self.log_event(
                    "校准",
                    &format!(
                        "插电自动结算：已采 {} 点（{}/11 区间，跨度 {}%）",
                        self.calib_samples, self.calib_buckets, self.calib_span
                    ),
                );
                self.calib_generate("插电自动结算");
            } else {
                self.calib_phase = 2;
                self.calib_save_state();
                self.log_event(
                    "校准",
                    &format!(
                        "充电断点已保存：已采 {} 点（{}/11 区间，跨度 {}%），拔线自动续采",
                        self.calib_samples, self.calib_buckets, self.calib_span
                    ),
                );
            }
        }
    }

    /// 拔线瞬间调用（daemon.sh:1035-1051）。
    pub fn calib_on_unplug(&mut self) {
        if self.cfg.calib_mode != 1 {
            return;
        }
        if self.calib_phase == 0 || self.calib_phase == 2 {
            let was_paused = self.calib_phase;
            self.calib_phase = 1;
            if self.calib_start_pct == 0 {
                self.calib_start_pct = self.bb;
            }
            self.calib_last_up = 0;
            self.calib_save_state();
            if was_paused == 2 {
                self.log_event(
                    "校准",
                    &format!(
                        "拔线恢复断点续采：当前 {}%（已有 {} 点 / 跨度 {}%）",
                        self.bb, self.calib_samples, self.calib_span
                    ),
                );
                self.notifi(
                    "引导校准",
                    &format!("已恢复断点续采（当前跨度 {}%），累计 ≥25% 即可随时生成曲线", self.calib_span),
                );
            } else {
                self.log_event("校准", &format!("拔线开始极速采样：起点 {}%（支持断点续采，≥25% 跨度即可生成）", self.bb));
                self.notifi("引导校准", "已开始极速引导校准，正常使用放掉约 25%~30% 电量即可生成专属曲线");
            }
        }
    }

    /// 生成专属曲线（daemon.sh:1054-1078）。
    pub fn calib_generate(&mut self, reason: &str) {
        let knots = match self.calc_curve_knots() {
            Ok(k) => k,
            Err(e) => {
                self.log_event("校准", &format!("生成失败（{}）：{}", reason, e));
                self.notifi("引导校准", &format!("曲线生成失败：{}", e));
                self.cfg.calib_mode = 0;
                self.save_config();
                self.calib_reset();
                return;
            }
        };
        match self.apply_calibration_result(&knots, reason) {
            Ok(msg) => {
                self.notifi("引导校准", &msg);
                self.calib_reset();
                self.load_custom_curve();
            }
            Err(msg) => {
                self.log_event("校准", &format!("生成失败（{}）：{}", reason, msg));
                self.notifi("引导校准", &format!("曲线生成失败：{}", msg));
                self.cfg.calib_mode = 0;
                self.save_config();
                self.calib_reset();
            }
        }
    }

    /// 加载专属校准曲线（daemon.sh:1081-1100）。
    pub fn load_custom_curve(&mut self) {
        self.custom_curve_ok = false;
        self.curve = [0; 11];
        let path = self.moddir.join(CALIB_CURVE_FILE);
        if !path.is_file() {
            return;
        }
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(_) => return,
        };
        let mut ok = true;
        let mut prev = 0i32;
        for (i, t) in [0, 10, 20, 30, 40, 50, 60, 70, 80, 90, 100].iter().enumerate() {
            let key = format!("V_{}", t);
            let val = match read_key(&text, &key).and_then(|s| s.parse::<i32>().ok()) {
                Some(v) => v,
                None => {
                    ok = false;
                    break;
                }
            };
            if val < 2_850_000 || val > 4_650_000 {
                ok = false;
                break;
            }
            if prev != 0 && val <= prev {
                ok = false;
                break;
            }
            prev = val;
            self.curve[i] = val;
        }
        self.custom_curve_ok = ok;
        if !ok {
            self.curve = [0; 11];
        }
    }

    /// 曲线拟合核心（common.sh:664-869）。返回 11 个节点（uV）。
    pub fn calc_curve_knots(&self) -> Result<[i32; 11], String> {
        let path = self.moddir.join(CALIB_SAMPLES_FILE);
        let text = std::fs::read_to_string(&path).map_err(|_| "还没有采样数据（请先开启校准并放电一段时间）".to_string())?;

        // Step 1: 分箱
        let mut blist: Vec<Vec<i64>> = vec![Vec::new(); 11];
        let mut bcnt = [0usize; 11];
        let mut count = 0i64;
        let mut min_p = 101i32;
        let mut max_p = 0i32;
        for raw in text.lines() {
            let line = raw.trim_end_matches('\r');
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut parts = line.split(',');
            let pct = match parts.next().and_then(|s| s.parse::<i32>().ok()) {
                Some(v) => v,
                None => continue,
            };
            let v = match parts.next().and_then(|s| s.parse::<i32>().ok()) {
                Some(v) => v,
                None => continue,
            };
            if pct > 100 || v <= 2_000_000 || v >= 5_000_000 {
                continue;
            }
            let mv = (v as i64 + 500) / 1000;
            let mut bi = (pct + 5) / 10;
            if bi > 10 {
                bi = 10;
            }
            if bcnt[bi as usize] < 40 {
                blist[bi as usize].push(mv);
                bcnt[bi as usize] += 1;
            }
            count += 1;
            if pct < min_p {
                min_p = pct;
            }
            if pct > max_p {
                max_p = pct;
            }
        }
        if count < 6 {
            return Err(format!("有效采样只有 {} 个（至少需要 6 个）", count));
        }
        let span = max_p - min_p;
        if span < 25 {
            return Err(format!(
                "电量覆盖跨度仅 {}%（极速校准需至少 25% 跨度，中途插电会自动保留进度）",
                span
            ));
        }

        // Step 2: 各非空桶中位数与权重
        let mut ob_idx = Vec::new();
        let mut ob_v = Vec::new();
        let mut ob_w = Vec::new();
        let mut bmed = [0i64; 11];
        let mut bw = [0i64; 11];
        for i in 0..11 {
            if bcnt[i] > 0 {
                let med = median_mv(&blist[i]);
                bmed[i] = med;
                let mut w = bcnt[i] as i64;
                if w > 10 {
                    w = 10;
                }
                bw[i] = w;
                ob_idx.push(i as i32);
                ob_v.push(med);
                ob_w.push(w);
            }
        }
        let nb = ob_idx.len();
        if nb < 3 {
            return Err(format!("仅覆盖 {} 个电量区间（至少需要 3 个不同 10% 区间）", nb));
        }

        // Step 3: PAVA 保序回归
        #[derive(Clone, Copy)]
        struct Block {
            s: usize,
            e: usize,
            v: i64,
            w: i64,
        }
        let mut stack: Vec<Block> = Vec::new();
        for k in 0..nb {
            let mut blk = Block {
                s: k,
                e: k,
                v: ob_v[k],
                w: ob_w[k],
            };
            stack.push(blk);
            while stack.len() >= 2 {
                let top = stack.len() - 1;
                let prev = top - 1;
                if stack[prev].v > stack[top].v {
                    let wn = stack[prev].w + stack[top].w;
                    let vn = (stack[prev].v * stack[prev].w + stack[top].v * stack[top].w + wn / 2) / wn;
                    stack[prev].v = vn;
                    stack[prev].w = wn;
                    stack[prev].e = stack[top].e;
                    stack.pop();
                } else {
                    break;
                }
            }
        }
        // 展开回桶
        let mut biso = [0i64; 11];
        for blk in &stack {
            for j in blk.s..=blk.e {
                biso[ob_idx[j] as usize] = blk.v;
            }
        }

        let first_idx = ob_idx[0] as usize;
        let last_idx = ob_idx[nb - 1] as usize;
        let v_lo_obs = biso[first_idx];
        let v_hi_obs = biso[last_idx];
        if v_hi_obs - v_lo_obs < 80 {
            return Err("实测有效电压变化不足 0.08V，数据不可信（请检查电压节点）".to_string());
        }

        // Step 4: WLS 线性回归 y = a * prior / 1000 + b
        let mut sum_w = 0i64;
        let mut sum_wx = 0i64;
        let mut sum_wy = 0i64;
        for k in 0..nb {
            let bidx = ob_idx[k] as usize;
            let xk = PRIOR[bidx];
            let yk = biso[bidx];
            let wk = ob_w[k];
            sum_w += wk;
            sum_wx += wk * xk;
            sum_wy += wk * yk;
        }
        let xbar = (sum_wx + sum_w / 2) / sum_w;
        let ybar = (sum_wy + sum_w / 2) / sum_w;

        let mut sxx = 0i64;
        let mut sxy = 0i64;
        for k in 0..nb {
            let bidx = ob_idx[k] as usize;
            let xk = PRIOR[bidx];
            let yk = biso[bidx];
            let wk = ob_w[k];
            let dx = xk - xbar;
            let dy = yk - ybar;
            sxx += wk * dx * dx;
            sxy += wk * dx * dy;
        }

        let mut fit_a = if sxx > 0 { (sxy * 1000) / sxx } else { 1100 };
        if fit_a < 650 {
            fit_a = 650;
        }
        if fit_a > 1550 {
            fit_a = 1550;
        }
        let fit_b = ybar - (fit_a * xbar + 500) / 1000;

        // Step 5: 生成 11 个节点
        let mut knots = [0i32; 11];
        for i in 0..11 {
            let px = PRIOR[i];
            let v_pred = fit_b + (fit_a * px + 500) / 1000;
            let vf = if bcnt[i] > 0 {
                let vo = biso[i];
                (vo * 8 + v_pred * 2 + 5) / 10
            } else {
                let mut best_d = 99i64;
                let mut best_res = 0i64;
                for k in 0..nb {
                    let bidx = ob_idx[k] as i64;
                    let mut dist = (i as i64 - bidx).abs();
                    if dist < 0 {
                        dist = -dist;
                    }
                    if dist < best_d {
                        best_d = dist;
                        let npx = PRIOR[ob_idx[k] as usize];
                        let nvo = biso[ob_idx[k] as usize];
                        let npred = fit_b + (fit_a * npx + 500) / 1000;
                        best_res = nvo - npred;
                    }
                }
                if best_d == 1 {
                    v_pred + best_res / 2
                } else {
                    v_pred
                }
            };
            knots[i] = (((vf + 5) / 10) * 10_000) as i32;
        }
        Ok(knots)
    }

    /// 校验并落盘曲线（common.sh:878-941）。
    pub fn apply_calibration_result(&mut self, knots: &[i32; 11], reason: &str) -> Result<String, String> {
        let mut k = *knots;
        // 单调性修复
        let mut prev = 0i32;
        for i in 0..11 {
            if prev != 0 && k[i] <= prev {
                k[i] = prev + 10_000;
            }
            prev = k[i];
        }

        let v0 = k[0];
        let v100 = k[10];
        if v100 < 3_900_000 || v100 > 4_650_000 {
            return Err(format!("满电端电压 {}V 异常（合理区间 3.90~4.65V），已拒绝生成", uv_to_v2(v100)));
        }
        if v0 < 2_850_000 || v0 > 3_600_000 {
            return Err(format!("空电端电压 {}V 异常（合理区间 2.85~3.60V），已拒绝生成", uv_to_v2(v0)));
        }
        if v100 - v0 < 350_000 {
            return Err("全段电压跨度不足 0.35V，数据不可信，已拒绝生成".to_string());
        }

        // 写 curve.conf
        let mut s = String::new();
        s.push_str("# 电压电量模拟服务 v5.52 专属校准曲线（引导校准自动生成）\n");
        s.push_str("# 每 10% 一个节点，单位 uV，随电量升高电压严格升高\n");
        for (i, t) in [0, 10, 20, 30, 40, 50, 60, 70, 80, 90, 100].iter().enumerate() {
            s.push_str(&format!("V_{}={}\n", t, k[i]));
        }
        let _ = std::fs::write(self.moddir.join(CALIB_CURVE_FILE), &s);

        // 更新标尺
        self.cfg.max_v = v100;
        self.cfg.shutdown_v = v0 + 20_000;
        if self.cfg.shutdown_v > 3_580_000 {
            self.cfg.shutdown_v = 3_580_000;
        }
        if self.cfg.link_mode == 1 {
            self.cfg.apply_linkage();
        } else {
            self.cfg.min_v = v0;
        }
        if self.cfg.max_v - self.cfg.min_v < 500_000 {
            self.cfg.max_v = self.cfg.min_v + 500_000;
        }
        if self.cfg.max_v > 4_650_000 {
            self.cfg.max_v = 4_650_000;
        }
        self.cfg.curve_mode = 2;
        self.cfg.calib_mode = 0;
        self.save_config();
        crate::config::update_prop(&self.moddir, &self.cfg, &self.learned);
        self.log_event(
            "校准",
            &format!(
                "成功：{} 生成专属曲线（满电 {}V ~ 空电 {}V），已自动切换到专属曲线模式",
                reason,
                uv_to_v2(v100),
                uv_to_v2(v0)
            ),
        );
        let msg = format!(
            "已生成专属曲线并生效：100%={}V ~ 0%={}V（关机红线 {}V）",
            uv_to_v2(v100),
            uv_to_v2(v0),
            uv_to_v2(self.cfg.shutdown_v)
        );
        Ok(msg)
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

// 保持 Config 被引用（apply_calibration_result 使用 self.cfg 时编译器需要 crate::config::Config 可见即可）
#[allow(unused_imports)]
use Config as _ConfigUsed;
