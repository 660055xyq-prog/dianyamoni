//! 配置与学习数据：类型、默认值、读写、校验、参数联动、单位换算。
//!
//! 复刻自 v5.52 的 common.sh（reset_config_defaults / read_config_file /
//! write_config / reset_learned_defaults / save_learned / derive_linked_vals /
//! apply_linkage / uv_to_v2 等）。文件格式与 v5.52 保持逐字节兼容。

use std::fs;
use std::io::Write;
use std::path::Path;

// ---------------------------------------------------------------------------
// 常量
// ---------------------------------------------------------------------------

/// 关机红线预设档位（cycle 子命令遍历用，单位 uV）。
pub const PRESETS: [i32; 8] = [
    3_000_000, 3_050_000, 3_100_000, 3_150_000,
    3_200_000, 3_250_000, 3_300_000, 3_350_000,
];

pub const MAX_LOG_BYTES: u64 = 2_097_152;

// ---------------------------------------------------------------------------
// 通用解析 / 工具
// ---------------------------------------------------------------------------

/// 是否为非负十进制整数（对应 shell `is_uint`）。
pub fn is_uint(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// 是否为有符号十进制整数（对应 shell `is_signed_int`）。
pub fn is_signed_int(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let body = if s.as_bytes()[0] == b'-' { &s[1..] } else { s };
    !body.is_empty() && body.bytes().all(|b| b.is_ascii_digit())
}

/// 安全解析非负整数为 i64（失败返回 None）。
pub fn parse_uint(s: &str) -> Option<i64> {
    if !is_uint(s) {
        return None;
    }
    s.parse::<i64>().ok()
}

/// 安全解析有符号整数为 i64（失败返回 None）。
pub fn parse_int(s: &str) -> Option<i64> {
    if !is_signed_int(s) {
        return None;
    }
    s.parse::<i64>().ok()
}

/// 夹取。
pub fn clamp<T: PartialOrd>(v: T, lo: T, hi: T) -> T {
    if v < lo {
        lo
    } else if v > hi {
        hi
    } else {
        v
    }
}

/// 以字符串形式读文件首行（去尾部 \r、空格），不存在返回 None。
pub fn read_first_line(path: &Path) -> Option<String> {
    let data = fs::read_to_string(path).ok()?;
    let line = data.lines().next()?;
    Some(line.trim_end_matches('\r').trim().to_string())
}

/// 读整个文件文本，失败返回空串。
pub fn read_file_string(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

/// 原子写：先写 tmp 再 rename，并 chmod 644。失败静默（与 shell 行为一致）。
pub fn atomic_write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let tmp = path.with_extension("tmp");
    let res = (|| -> std::io::Result<()> {
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(content.as_bytes())?;
            f.sync_all().ok(); // 尽力刷盘，失败不致命
        }
        let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o644));
        fs::rename(&tmp, path)?;
        Ok(())
    })();
    let _ = res;
}

/// 追加一行到文件（保留已有内容）。
pub fn append_line(path: &Path, line: &str) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(line.as_bytes());
        let _ = f.write_all(b"\n");
    }
}

/// uV -> 两位小数字符串（对应 shell uv_to_v2）。负值返回 "0.00"。
pub fn uv_to_v2(uv: i32) -> String {
    if uv < 0 {
        return "0.00".to_string();
    }
    let mut vi = uv / 1_000_000;
    let mut vf = (uv % 1_000_000 + 5000) / 10_000;
    if vf >= 100 {
        vi += 1;
        vf -= 100;
    }
    if vf < 10 {
        format!("{}.0{}", vi, vf)
    } else {
        format!("{}.{}", vi, vf)
    }
}

/// uV -> 以 10mV 步长向上取整（对应 shell `((_v+5000)/10000*10000)`）。
pub fn round_10mv(uv: i32) -> i32 {
    (uv + 5000) / 10_000 * 10_000
}

/// 改写 module.prop 的 description= 行（对应 update_prop）。
pub fn update_prop(moddir: &Path, cfg: &Config, learned: &Learned) {
    let prop = moddir.join("module.prop");
    let text = read_file_string(&prop);
    let eff_v = if cfg.auto_min_learn == 1 && learned.learned_floor_v > cfg.shutdown_v {
        learned.learned_floor_v
    } else {
        cfg.shutdown_v
    };
    let sv = uv_to_v2(eff_v);
    let min_s = uv_to_v2(cfg.min_v);
    let max_s = uv_to_v2(cfg.max_v);
    let new_desc = format!(
        "description=音量键盲操续命+看门狗自愈+参数联动+引导校准 [红线: {}V | 范围: {}~{}V]",
        sv, min_s, max_s
    );
    let mut out = String::new();
    let mut replaced = false;
    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        if line.starts_with("description=") {
            out.push_str(&new_desc);
            out.push('\n');
            replaced = true;
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    if !replaced {
        out.push_str(&new_desc);
        out.push('\n');
    }
    atomic_write(&prop, &out);
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Config {
    pub conf_ver: i32,
    pub max_v: i32,
    pub min_v: i32,
    pub shutdown_v: i32,
    pub low_max: i32,
    pub low_min: i32,
    pub off_time: i32,
    pub batlog: i32,
    pub curve_mode: i32,
    pub charge_mode: i32,
    pub allow_rebound: i32,
    pub ir_comp_mode: i32,
    pub auto_max_learn: i32,
    pub auto_min_learn: i32,
    pub bb_auto_apply: i32,
    pub rapid_shutdown: i32,
    pub temp_comp: i32,
    pub curr_unit_ovr: i32,
    pub curr_sign_ovr: i32,
    pub shutdown_confirm: i32,
    pub time: i32,
    pub screen_off_time: i32,
    pub usb_poll_step: i32,
    pub boot_grace_sec: i32,
    pub key_snooze: i32,
    pub key_snooze_sec: i32,
    pub link_mode: i32,
    pub calib_mode: i32,
}

impl Config {
    /// reset_config_defaults
    pub fn defaults() -> Self {
        Config {
            conf_ver: 1,
            max_v: 4_450_000,
            min_v: 3_180_000,
            shutdown_v: 3_200_000,
            low_max: 3_350_000,
            low_min: 3_250_000,
            off_time: 45,
            batlog: 0,
            curve_mode: 0,
            charge_mode: 0,
            allow_rebound: 1,
            ir_comp_mode: 1,
            auto_max_learn: 1,
            auto_min_learn: 1,
            bb_auto_apply: 0,
            rapid_shutdown: 1,
            temp_comp: 1,
            curr_unit_ovr: 0,
            curr_sign_ovr: 0,
            shutdown_confirm: 2,
            time: 10,
            screen_off_time: 60,
            usb_poll_step: 2,
            boot_grace_sec: 180,
            key_snooze: 1,
            key_snooze_sec: 180,
            link_mode: 1,
            calib_mode: 0,
        }
    }

    /// 计算派生值（对应 derive_linked_vals）。
    pub fn derive_linked_vals(&self) -> (i32, i32, i32) {
        let d_min = clamp(self.shutdown_v - 20_000, 2_850_000, 3_600_000);
        let d_low_min = clamp(self.shutdown_v + 50_000, 2_850_000, 3_700_000);
        let mut d_low_max = clamp(self.shutdown_v + 150_000, 2_900_000, 3_800_000);
        if d_low_max <= d_low_min {
            d_low_max = d_low_min + 100_000;
        }
        (d_min, d_low_min, d_low_max)
    }

    /// 把 MIN_V / LOW_MIN / LOW_MAX 赋为派生值（对应 apply_linkage）。
    pub fn apply_linkage(&mut self) {
        let (d_min, d_low_min, d_low_max) = self.derive_linked_vals();
        self.min_v = d_min;
        self.low_min = d_low_min;
        self.low_max = d_low_max;
    }

    /// 从文件读配置：先复位默认值，再逐行应用（对应 load_config）。
    pub fn load(path: &Path) -> Self {
        let mut cfg = Config::defaults();
        cfg.read_file(path);
        cfg
    }

    /// 读入并校验（对应 read_config_file 的静默忽略语义）。
    pub fn read_file(&mut self, path: &Path) {
        let text = read_file_string(path);
        let mut staged_max = self.max_v;
        let mut staged_min = self.min_v;
        for raw in text.lines() {
            let line = raw.trim_end_matches('\r');
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (k, v) = match line.split_once('=') {
                Some(x) => x,
                None => continue,
            };
            let k = k.trim();
            let v = v.trim_end_matches('\r').trim();
            let val = match parse_uint(v) {
                Some(x) => x,
                None => continue,
            };
            match k {
                "CONF_VER" if (0..=999).contains(&val) => self.conf_ver = val as i32,
                "MAX_V" if (3_900_000..=4_650_000).contains(&val) => staged_max = val as i32,
                "MIN_V" if (2_850_000..=3_600_000).contains(&val) => staged_min = val as i32,
                "SHUTDOWN_V" if (2_850_000..=3_600_000).contains(&val) => self.shutdown_v = val as i32,
                "LOW_MAX" if (2_900_000..=3_800_000).contains(&val) => self.low_max = val as i32,
                "LOW_MIN" if (2_850_000..=3_700_000).contains(&val) => self.low_min = val as i32,
                "off_time" if (10..=600).contains(&val) => self.off_time = val as i32,
                "BATLOG" if val <= 1 => self.batlog = val as i32,
                "CURVE_MODE" if val <= 2 => self.curve_mode = val as i32,
                "CHARGE_MODE" if val <= 2 => self.charge_mode = val as i32,
                "ALLOW_REBOUND" if val <= 1 => self.allow_rebound = val as i32,
                "IR_COMP_MODE" if val <= 1 => self.ir_comp_mode = val as i32,
                "AUTO_MAX_LEARN" if val <= 1 => self.auto_max_learn = val as i32,
                "AUTO_MIN_LEARN" if val <= 1 => self.auto_min_learn = val as i32,
                "BB_AUTO_APPLY" if val <= 1 => self.bb_auto_apply = val as i32,
                "RAPID_SHUTDOWN" if val <= 1 => self.rapid_shutdown = val as i32,
                "TEMP_COMP" if val <= 1 => self.temp_comp = val as i32,
                "CURR_UNIT_OVR" if val <= 2 => self.curr_unit_ovr = val as i32,
                "CURR_SIGN_OVR" if val <= 2 => self.curr_sign_ovr = val as i32,
                "SHUTDOWN_CONFIRM" if (1..=5).contains(&val) => self.shutdown_confirm = val as i32,
                "TIME" if (5..=60).contains(&val) => self.time = val as i32,
                "SCREEN_OFF_TIME" if (10..=300).contains(&val) => self.screen_off_time = val as i32,
                "USB_POLL_STEP" if (1..=10).contains(&val) => self.usb_poll_step = val as i32,
                "BOOT_GRACE_SEC" if val <= 600 => self.boot_grace_sec = val as i32,
                "KEY_SNOOZE" if val <= 1 => self.key_snooze = val as i32,
                "KEY_SNOOZE_SEC" if (30..=600).contains(&val) => self.key_snooze_sec = val as i32,
                "LINK_MODE" if val <= 1 => self.link_mode = val as i32,
                "CALIB_MODE" if val <= 1 => self.calib_mode = val as i32,
                _ => {}
            }
        }
        if staged_max - staged_min >= 500_000 {
            self.max_v = staged_max;
            self.min_v = staged_min;
        }
    }

    /// 写配置（对应 write_config：固定字段顺序 + 3 行注释头）。
    pub fn write_file(&self, path: &Path) {
        let mut s = String::with_capacity(1024);
        s.push_str("# 电压电量模拟服务 v5.4 配置文件（单电芯等效电压，单位：uV）\n");
        s.push_str("# 保存后通过 USR1 信号秒级热生效，无需重启手机\n");
        s.push_str("# 注：LINK_MODE=1 时 MIN_V / LOW_MIN / LOW_MAX 由 SHUTDOWN_V 自动联动，手动修改无效\n");
        s.push_str(&format!("CONF_VER={}\n", self.conf_ver));
        s.push_str(&format!("MAX_V={}\n", self.max_v));
        s.push_str(&format!("MIN_V={}\n", self.min_v));
        s.push_str(&format!("SHUTDOWN_V={}\n", self.shutdown_v));
        s.push_str(&format!("LOW_MAX={}\n", self.low_max));
        s.push_str(&format!("LOW_MIN={}\n", self.low_min));
        s.push_str(&format!("off_time={}\n", self.off_time));
        s.push_str(&format!("BATLOG={}\n", self.batlog));
        s.push_str(&format!("CURVE_MODE={}\n", self.curve_mode));
        s.push_str(&format!("CHARGE_MODE={}\n", self.charge_mode));
        s.push_str(&format!("ALLOW_REBOUND={}\n", self.allow_rebound));
        s.push_str(&format!("IR_COMP_MODE={}\n", self.ir_comp_mode));
        s.push_str(&format!("AUTO_MAX_LEARN={}\n", self.auto_max_learn));
        s.push_str(&format!("AUTO_MIN_LEARN={}\n", self.auto_min_learn));
        s.push_str(&format!("BB_AUTO_APPLY={}\n", self.bb_auto_apply));
        s.push_str(&format!("RAPID_SHUTDOWN={}\n", self.rapid_shutdown));
        s.push_str(&format!("TEMP_COMP={}\n", self.temp_comp));
        s.push_str(&format!("CURR_UNIT_OVR={}\n", self.curr_unit_ovr));
        s.push_str(&format!("CURR_SIGN_OVR={}\n", self.curr_sign_ovr));
        s.push_str(&format!("SHUTDOWN_CONFIRM={}\n", self.shutdown_confirm));
        s.push_str(&format!("TIME={}\n", self.time));
        s.push_str(&format!("SCREEN_OFF_TIME={}\n", self.screen_off_time));
        s.push_str(&format!("USB_POLL_STEP={}\n", self.usb_poll_step));
        s.push_str(&format!("BOOT_GRACE_SEC={}\n", self.boot_grace_sec));
        s.push_str(&format!("KEY_SNOOZE={}\n", self.key_snooze));
        s.push_str(&format!("KEY_SNOOZE_SEC={}\n", self.key_snooze_sec));
        s.push_str(&format!("LINK_MODE={}\n", self.link_mode));
        s.push_str(&format!("CALIB_MODE={}\n", self.calib_mode));
        atomic_write(path, &s);
    }
}

// ---------------------------------------------------------------------------
// Learned
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Learned {
    pub r_int_mohm: i32,
    pub r_samples: i32,
    pub r_low_mohm: i32,
    pub r_low_samples: i32,
    pub hw_cutoff_v: i32,
    pub learned_floor_v: i32,
    pub pending_floor_v: i32,
    pub blackscreen_cnt: i32,
    pub last_bs_info: String,
    pub last_max_learn_v: i32,
    pub max_learn_cnt: i32,
    pub curr_unit: String, // "uA" | "mA" | "auto"
    pub disch_sign: i32,   // 1 | -1 | 0
    pub cell_div: i32,     // 1 | 2
}

impl Learned {
    /// reset_learned_defaults
    pub fn defaults() -> Self {
        Learned {
            r_int_mohm: 80,
            r_samples: 0,
            r_low_mohm: 0,
            r_low_samples: 0,
            hw_cutoff_v: 0,
            learned_floor_v: 0,
            pending_floor_v: 0,
            blackscreen_cnt: 0,
            last_bs_info: String::new(),
            last_max_learn_v: 0,
            max_learn_cnt: 0,
            curr_unit: "auto".to_string(),
            disch_sign: 0,
            cell_div: 1,
        }
    }

    /// 从文件加载（对应 load_learned）。
    pub fn load(path: &Path) -> Self {
        let mut l = Learned::defaults();
        l.read_file(path);
        l
    }

    pub fn read_file(&mut self, path: &Path) {
        let text = read_file_string(path);
        for raw in text.lines() {
            let line = raw.trim_end_matches('\r');
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (k, v) = match line.split_once('=') {
                Some(x) => x,
                None => continue,
            };
            let k = k.trim();
            let v = v.trim_end_matches('\r').trim();
            match k {
                "LAST_BS_INFO" => self.last_bs_info = v.to_string(),
                "CURR_UNIT" if v == "uA" || v == "mA" || v == "auto" => self.curr_unit = v.to_string(),
                "DISCH_SIGN" if v == "1" || v == "-1" || v == "0" => self.disch_sign = v.parse::<i32>().unwrap(),
                "CELL_DIV" if v == "1" || v == "2" => self.cell_div = v.parse::<i32>().unwrap(),
                _ => {
                    let val = match parse_uint(v) {
                        Some(x) => x,
                        None => continue,
                    };
                    match k {
                        "R_INT_MOHM" if (25..=600).contains(&val) => self.r_int_mohm = val as i32,
                        "R_SAMPLES" => self.r_samples = val as i32,
                        "R_LOW_MOHM" if val <= 800 => self.r_low_mohm = val as i32,
                        "R_LOW_SAMPLES" => self.r_low_samples = val as i32,
                        "HW_CUTOFF_V" if val <= 3_600_000 => self.hw_cutoff_v = val as i32,
                        "LEARNED_FLOOR_V" if val <= 3_550_000 => self.learned_floor_v = val as i32,
                        "PENDING_FLOOR_V" if val <= 3_550_000 => self.pending_floor_v = val as i32,
                        "BLACKSCREEN_CNT" => self.blackscreen_cnt = val as i32,
                        "LAST_MAX_LEARN_V" if val <= 4_650_000 => self.last_max_learn_v = val as i32,
                        "MAX_LEARN_CNT" => self.max_learn_cnt = val as i32,
                        _ => {}
                    }
                }
            }
        }
    }

    /// 写学习数据（对应 save_learned）。
    pub fn write_file(&self, path: &Path) {
        let mut s = String::with_capacity(512);
        s.push_str("# 电压电量模拟服务 v5.4 自适应学习数据（自动维护）\n");
        s.push_str(&format!("R_INT_MOHM={}\n", self.r_int_mohm));
        s.push_str(&format!("R_SAMPLES={}\n", self.r_samples));
        s.push_str(&format!("R_LOW_MOHM={}\n", self.r_low_mohm));
        s.push_str(&format!("R_LOW_SAMPLES={}\n", self.r_low_samples));
        s.push_str(&format!("HW_CUTOFF_V={}\n", self.hw_cutoff_v));
        s.push_str(&format!("LEARNED_FLOOR_V={}\n", self.learned_floor_v));
        s.push_str(&format!("PENDING_FLOOR_V={}\n", self.pending_floor_v));
        s.push_str(&format!("BLACKSCREEN_CNT={}\n", self.blackscreen_cnt));
        s.push_str(&format!("LAST_BS_INFO={}\n", self.last_bs_info));
        s.push_str(&format!("LAST_MAX_LEARN_V={}\n", self.last_max_learn_v));
        s.push_str(&format!("MAX_LEARN_CNT={}\n", self.max_learn_cnt));
        s.push_str(&format!("CURR_UNIT={}\n", self.curr_unit));
        s.push_str(&format!("DISCH_SIGN={}\n", self.disch_sign));
        s.push_str(&format!("CELL_DIV={}\n", self.cell_div));
        atomic_write(path, &s);
    }
}
