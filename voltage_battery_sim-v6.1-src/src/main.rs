//! 电压电量模拟服务 v6.0 Rust —— 单二进制双角色入口。
//!
//! `vbat_sim daemon`      —— 守护进程（epoll 事件循环）
//! `vbat_sim <subcommand>` —— 命令中心（承接 config_helper.sh 全部 24 个子命令）

mod algo;
mod config;
mod daemon;
mod ipc;
mod platform;
mod sysfs;
mod telemetry;

use std::collections::HashMap;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::exit;
use std::time::Duration;

use config::{uv_to_v2, Config, Learned};

// config_helper.sh / common.sh 使用、但 daemon.rs 未定义的路径常量。
const WATCHDOG_PID_FILE: &str = ".watchdog.pid";
const WATCHDOG_PAUSE_FILE: &str = ".watchdog_pause";
const KEYNODE_OVR_FILE: &str = ".keynode_override";
const BATNODE_OVR_FILE: &str = ".batnode_override";
const KEYREQ_FILE: &str = ".keysnooze_req";
const BB_EVENTS_FILE: &str = "blackbox_events.csv";
const LOG_FILE: &str = "log/电压日志.log";

fn main() {
    let mut args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(|s| s.as_str()).unwrap_or("").to_string();
    let rest: Vec<String> = args.drain(2..).collect();

    if cmd == "daemon" {
        let moddir = resolve_moddir();
        let mut d = daemon::Daemon::new(moddir);
        d.run();
    }

    let moddir = resolve_moddir();

    match cmd.as_str() {
        "status" => cmd_status(&moddir),
        "history" => tail_file(&moddir.join(telemetry::HIST_FILE), 360),
        "blackbox_events" => tail_file(&moddir.join(BB_EVENTS_FILE), 10),
        "events" => tail_file(&moddir.join(telemetry::EVENTS_FILE), 6),
        "apply_bb_floor" => cmd_apply_bb_floor(&moddir),
        "dismiss_bb_floor" => cmd_dismiss_bb_floor(&moddir),
        "set" => cmd_set(&moddir, &rest),
        "set_internal_max" => cmd_set_internal_max(&moddir, rest.first().map(|s| s.as_str())),
        "calibrate_full" => cmd_calibrate_full(&moddir),
        "calib_start" => cmd_calib_start(&moddir),
        "calib_abort" => cmd_calib_abort(&moddir),
        "calib_finish" => cmd_calib_finish(&moddir),
        "reset_learned" => cmd_reset_learned(&moddir),
        "snooze" => cmd_snooze(&moddir, rest.first().map(|s| s.as_str())),
        "cancel_shutdown" => cmd_cancel_shutdown(&moddir),
        "resume_shutdown" => cmd_resume_shutdown(&moddir),
        "reset" => cmd_reset(&moddir),
        "clear_log" => cmd_clear_log(&moddir),
        "action" => cmd_action(&moddir),
        "cycle" => cmd_cycle(&moddir),
        "migrate" => cmd_migrate(&moddir),
        "sync_prop" => cmd_sync_prop(&moddir),
        "restart" => cmd_restart(&moddir),
        "stop" => cmd_stop(&moddir),
        _ => {
            println!("Usage: vbat_sim {{status|history|blackbox_events|events|apply_bb_floor|dismiss_bb_floor|set|set_internal_max|calibrate_full|calib_start|calib_abort|calib_finish|reset_learned|snooze|cancel_shutdown|resume_shutdown|reset|clear_log|action|cycle|migrate|sync_prop|restart|stop}}");
            exit(1);
        }
    }
}

// ----------------------------------------------------------------------
// 基础工具
// ----------------------------------------------------------------------

fn resolve_moddir() -> PathBuf {
    if let Ok(m) = std::env::var("MODDIR") {
        if !m.is_empty() {
            return PathBuf::from(m);
        }
    }
    let argv0 = std::env::args().next().unwrap_or_default();
    let exe = PathBuf::from(argv0);
    let parent = exe
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    if parent.file_name().map(|n| n == "bin").unwrap_or(false) {
        parent.parent().map(|p| p.to_path_buf()).unwrap_or(parent)
    } else {
        parent
    }
}

fn pid_alive(pid: i32) -> bool {
    if pid <= 1 {
        return false;
    }
    unsafe { libc::kill(pid, 0) == 0 }
}

fn get_daemon_pid(moddir: &Path) -> Option<i32> {
    let s = std::fs::read_to_string(moddir.join(daemon::DAEMON_PID_FILE)).ok()?;
    let pid = s.trim().parse::<i32>().ok()?;
    if pid_alive(pid) {
        Some(pid)
    } else {
        None
    }
}

fn is_counting_down(moddir: &Path) -> bool {
    match sysfs::read_one_line(&moddir.join(daemon::COUNTDOWN_PID_FILE)) {
        Some(s) => match s.parse::<i32>() {
            Ok(pid) => pid_alive(pid),
            Err(_) => false,
        },
        None => false,
    }
}

fn ipc_send(moddir: &Path, cmd: &str) -> bool {
    let sock = moddir.join(ipc::IPC_SOCK_NAME);
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return false;
    }
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    let bytes = sock.as_os_str().as_bytes();
    let n = bytes.len().min(addr.sun_path.len() - 1);
    for (i, b) in bytes.iter().take(n).enumerate() {
        addr.sun_path[i] = *b as libc::c_char;
    }
    addr.sun_path[n] = 0;
    let len = (std::mem::size_of::<libc::sa_family_t>() + n + 1) as libc::socklen_t;
    let r = unsafe { libc::connect(fd, &addr as *const _ as *const libc::sockaddr, len) };
    if r < 0 {
        unsafe { libc::close(fd) };
        return false;
    }
    let data = format!("{}\n", cmd);
    unsafe { libc::write(fd, data.as_ptr() as *const libc::c_void, data.len()) };
    unsafe { libc::close(fd) };
    true
}

fn wake_daemon(moddir: &Path) {
    if !ipc_send(moddir, "wake") {
        if let Some(pid) = get_daemon_pid(moddir) {
            unsafe { libc::kill(pid, libc::SIGUSR1) };
        }
    }
}

fn kill_daemon_all(moddir: &Path) {
    if let Some(pid) = get_daemon_pid(moddir) {
        unsafe { libc::kill(pid, libc::SIGTERM) };
        std::thread::sleep(Duration::from_millis(1000));
        if pid_alive(pid) {
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
    }
}

fn spawn_daemon(moddir: &Path) {
    let exe = std::env::current_exe().unwrap_or_else(|_| moddir.join("bin/vbat_sim"));
    let _ = std::process::Command::new(exe)
        .arg("daemon")
        .env("MODDIR", moddir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

fn tail_file(path: &Path, n: usize) {
    if let Ok(t) = std::fs::read_to_string(path) {
        let lines: Vec<&str> = t.lines().collect();
        let start = lines.len().saturating_sub(n);
        for l in &lines[start..] {
            println!("{}", l);
        }
    }
}

fn read_runtime_state(moddir: &Path) -> HashMap<String, String> {
    let mut map = HashMap::new();
    if let Ok(t) = std::fs::read_to_string(moddir.join(telemetry::STATE_FILE)) {
        for line in t.lines() {
            if let Some((k, v)) = line.split_once('=') {
                map.insert(k.trim().to_string(), v.trim_end_matches('\r').trim().to_string());
            }
        }
    }
    map
}

fn rs<'a>(map: &'a HashMap<String, String>, key: &str, default: &'a str) -> &'a str {
    map.get(key).map(|s| s.as_str()).unwrap_or(default)
}

/// 读电压节点并做归一化（不可读/非数字返回空串，对应 shell read_one_line+norm_v_helper）。
fn norm_read(bat_dir: &Path, name: &str) -> String {
    match sysfs::read_one_line(&bat_dir.join(name)) {
        Some(s) => match s.parse::<i32>() {
            Ok(v) => sysfs::norm_v_helper(v).to_string(),
            Err(_) => String::new(),
        },
        None => String::new(),
    }
}

// ----------------------------------------------------------------------
// 子命令实现
// ----------------------------------------------------------------------

fn cmd_status(moddir: &Path) {
    let bat_dir = sysfs::detect_bat_node(moddir);
    let usb_type_path = PathBuf::from(sysfs::SYS_USB_TYPE_DEFAULT);
    let cfg = Config::load(&moddir.join(daemon::CONFIG_FILE));
    let learned = Learned::load(&moddir.join(daemon::LEARNED_FILE));
    let up_sec = sysfs::get_uptime_sec();
    let dpid = get_daemon_pid(moddir);

    let v_now = norm_read(&bat_dir, "voltage_now");
    let v_ocv = norm_read(&bat_dir, "voltage_ocv");
    let cap = sysfs::read_one_line(&bat_dir.join("capacity")).unwrap_or_default();
    let usb = sysfs::read_one_line(&usb_type_path).unwrap_or_default();
    let bst = sysfs::read_one_line(&bat_dir.join("status")).unwrap_or_default();
    let raw_temp = sysfs::read_one_line(&bat_dir.join("temp")).unwrap_or_default();

    let st = read_runtime_state(moddir);

    let sv_str = cfg.shutdown_v.to_string();
    let sim_lvl = rs(&st, "SIM_LEVEL", "").to_string();
    let flt_v = rs(&st, "FILTERED_V", "").to_string();
    let comp_v = rs(&st, "COMP_V", "").to_string();
    let streak = rs(&st, "STREAK", "").to_string();
    let disch_ma = rs(&st, "DISCH_MA", "0").to_string();
    let chg_ma = rs(&st, "CHG_MA", "0").to_string();
    let curr_signed = rs(&st, "CURR_SIGNED_MA", "0").to_string();
    let curr_valid = rs(&st, "CURR_VALID", "0").to_string();
    let temp_c10 = rs(&st, "TEMP_C10", &raw_temp).to_string();
    let temp_valid = rs(&st, "TEMP_VALID", "0").to_string();
    let temp_boost = rs(&st, "TEMP_BOOST_UV", "0").to_string();
    let eff_sv = rs(&st, "EFF_SHUTDOWN_V", &sv_str).to_string();
    let danger_z = rs(&st, "DANGER_ZONE", "0").to_string();
    let max_armed = rs(&st, "MAX_LEARN_ARMED", "0").to_string();
    let max_pend = rs(&st, "MAX_LEARN_PENDING", "0").to_string();
    let max_accum = rs(&st, "MAX_LEARN_ACCUM", "0").to_string();
    let max_psec = rs(&st, "MAX_LEARN_PEND_SEC", "0").to_string();
    let bl_ok = rs(&st, "BL_OK", "0").to_string();
    let ir_active = rs(&st, "IR_ACTIVE", "0").to_string();
    let active_r = rs(&st, "ACTIVE_R_MOHM", "0").to_string();
    let ocv_act = rs(&st, "OCV_ACTIVE", "0").to_string();

    let mut cd_active = rs(&st, "COUNTDOWN_ACTIVE", "0").to_string();
    let mut snooze_rem = rs(&st, "SNOOZE_REM", "0").to_string();
    let mut snooze_can = rs(&st, "SNOOZE_CANCELED", "0").to_string();

    // 倒计时判定（覆盖 state 文件）
    cd_active = if is_counting_down(moddir) { "1".to_string() } else { "0".to_string() };

    // snooze 剩余时间（覆盖 state 文件）
    if let Some(s) = sysfs::read_one_line(&moddir.join(daemon::SNOOZE_FILE)) {
        if let Ok(v) = s.parse::<u64>() {
            if v >= 999_999_990 {
                snooze_can = "1".to_string();
                snooze_rem = "999999".to_string();
            } else if v > up_sec {
                snooze_rem = (v - up_sec).to_string();
                snooze_can = "0".to_string();
            } else {
                snooze_rem = "0".to_string();
                snooze_can = "0".to_string();
            }
        }
    }

    let key_node = platform::evdev::detect_key_node(moddir);
    let key_ok = if key_node.is_some() { 1 } else { 0 };
    let key_node_str = key_node.map(|p| p.display().to_string()).unwrap_or_default();

    let mut custom_ok = 0;
    if let (Some(cv0), Some(cv100)) = (
        sysfs::read_key_from_file(&moddir.join(daemon::CALIB_CURVE_FILE), "V_0"),
        sysfs::read_key_from_file(&moddir.join(daemon::CALIB_CURVE_FILE), "V_100"),
    ) {
        if let (Ok(a), Ok(b)) = (cv0.parse::<i32>(), cv100.parse::<i32>()) {
            if b > a {
                custom_ok = 1;
            }
        }
    }

    let log_bytes = std::fs::metadata(moddir.join(LOG_FILE))
        .map(|m| m.len())
        .unwrap_or(0);

    let bat_ok = if sysfs::is_readable(&bat_dir.join("voltage_now")) { 1 } else { 0 };
    let watchdog_ok = match sysfs::read_one_line(&moddir.join(WATCHDOG_PID_FILE)) {
        Some(s) => match s.parse::<i32>() {
            Ok(pid) if pid_alive(pid) => 1,
            _ => 0,
        },
        None => 0,
    };
    let last_exit = sysfs::read_one_line(&moddir.join(telemetry::LAST_EXIT_FILE)).unwrap_or_default();

    let calib_phase = rs(&st, "CALIB_PHASE", "0");
    let calib_samples = rs(&st, "CALIB_SAMPLES", "0");
    let calib_start_pct = rs(&st, "CALIB_START_PCT", "0");
    let calib_min_v = rs(&st, "CALIB_MIN_V", "0");
    let calib_max_v = rs(&st, "CALIB_MAX_V", "0");
    let calib_buckets = rs(&st, "CALIB_BUCKETS", "0");
    let calib_min_pct = rs(&st, "CALIB_MIN_PCT", "0");
    let calib_max_pct = rs(&st, "CALIB_MAX_PCT", "0");
    let calib_span = rs(&st, "CALIB_SPAN", "0");

    println!("MAX_V={}", cfg.max_v);
    println!("MIN_V={}", cfg.min_v);
    println!("SHUTDOWN_V={}", cfg.shutdown_v);
    println!("LOW_MAX={}", cfg.low_max);
    println!("LOW_MIN={}", cfg.low_min);
    println!("off_time={}", cfg.off_time);
    println!("BATLOG={}", cfg.batlog);
    println!("CURVE_MODE={}", cfg.curve_mode);
    println!("CHARGE_MODE={}", cfg.charge_mode);
    println!("ALLOW_REBOUND={}", cfg.allow_rebound);
    println!("IR_COMP_MODE={}", cfg.ir_comp_mode);
    println!("AUTO_MAX_LEARN={}", cfg.auto_max_learn);
    println!("AUTO_MIN_LEARN={}", cfg.auto_min_learn);
    println!("BB_AUTO_APPLY={}", cfg.bb_auto_apply);
    println!("RAPID_SHUTDOWN={}", cfg.rapid_shutdown);
    println!("TEMP_COMP={}", cfg.temp_comp);
    println!("CURR_UNIT_OVR={}", cfg.curr_unit_ovr);
    println!("CURR_SIGN_OVR={}", cfg.curr_sign_ovr);
    println!("SHUTDOWN_CONFIRM={}", cfg.shutdown_confirm);
    println!("TIME={}", cfg.time);
    println!("SCREEN_OFF_TIME={}", cfg.screen_off_time);
    println!("USB_POLL_STEP={}", cfg.usb_poll_step);
    println!("BOOT_GRACE_SEC={}", cfg.boot_grace_sec);
    println!("KEY_SNOOZE={}", cfg.key_snooze);
    println!("KEY_SNOOZE_SEC={}", cfg.key_snooze_sec);
    println!("LINK_MODE={}", cfg.link_mode);
    println!("CALIB_MODE={}", cfg.calib_mode);
    println!("KEY_OK={}", key_ok);
    println!("KEY_NODE={}", key_node_str);
    println!("CUSTOM_CURVE_OK={}", custom_ok);
    println!("CALIB_PHASE={}", calib_phase);
    println!("CALIB_SAMPLES={}", calib_samples);
    println!("CALIB_START_PCT={}", calib_start_pct);
    println!("CALIB_MIN_V={}", calib_min_v);
    println!("CALIB_MAX_V={}", calib_max_v);
    println!("CALIB_BUCKETS={}", calib_buckets);
    println!("CALIB_MIN_PCT={}", calib_min_pct);
    println!("CALIB_MAX_PCT={}", calib_max_pct);
    println!("CALIB_SPAN={}", calib_span);
    println!("VOLTAGE_NOW={}", v_now);
    println!("VOLTAGE_OCV={}", v_ocv);
    println!("CAPACITY={}", cap);
    println!("USB_TYPE={}", usb);
    println!("BAT_STATUS={}", bst);
    println!("SERVICE_PID={}", dpid.map(|p| p.to_string()).unwrap_or_default());
    println!("UPTIME_SEC={}", up_sec);
    println!("SIM_LEVEL={}", sim_lvl);
    println!("FILTERED_V={}", flt_v);
    println!("COMP_V={}", comp_v);
    println!("STREAK={}", streak);
    println!("COUNTDOWN_ACTIVE={}", cd_active);
    println!("SNOOZE_REM={}", snooze_rem);
    println!("SNOOZE_CANCELED={}", snooze_can);
    println!("DISCH_MA={}", disch_ma);
    println!("CHG_MA={}", chg_ma);
    println!("CURR_SIGNED_MA={}", curr_signed);
    println!("CURR_VALID={}", curr_valid);
    println!("TEMP_C10={}", temp_c10);
    println!("TEMP_VALID={}", temp_valid);
    println!("TEMP_BOOST_UV={}", temp_boost);
    println!("EFF_SHUTDOWN_V={}", eff_sv);
    println!("DANGER_ZONE={}", danger_z);
    println!("MAX_LEARN_ARMED={}", max_armed);
    println!("MAX_LEARN_PENDING={}", max_pend);
    println!("MAX_LEARN_ACCUM={}", max_accum);
    println!("MAX_LEARN_PEND_SEC={}", max_psec);
    println!("BL_OK={}", bl_ok);
    println!("IR_ACTIVE={}", ir_active);
    println!("ACTIVE_R_MOHM={}", active_r);
    println!("OCV_ACTIVE={}", ocv_act);
    println!("R_INT_MOHM={}", learned.r_int_mohm);
    println!("R_SAMPLES={}", learned.r_samples);
    println!("R_LOW_MOHM={}", learned.r_low_mohm);
    println!("R_LOW_SAMPLES={}", learned.r_low_samples);
    println!("HW_CUTOFF_V={}", learned.hw_cutoff_v);
    println!("LEARNED_FLOOR_V={}", learned.learned_floor_v);
    println!("PENDING_FLOOR_V={}", learned.pending_floor_v);
    println!("BLACKSCREEN_CNT={}", learned.blackscreen_cnt);
    println!("LAST_BS_INFO={}", learned.last_bs_info);
    println!("LAST_MAX_LEARN_V={}", learned.last_max_learn_v);
    println!("MAX_LEARN_CNT={}", learned.max_learn_cnt);
    println!("CURR_UNIT={}", learned.curr_unit);
    println!("DISCH_SIGN={}", learned.disch_sign);
    println!("CELL_DIV={}", learned.cell_div);
    println!("LOG_BYTES={}", log_bytes);
    println!("CONF_VER={}", cfg.conf_ver);
    println!("BAT_NODE={}", bat_dir.display());
    println!("BAT_OK={}", bat_ok);
    println!("WATCHDOG_OK={}", watchdog_ok);
    println!("LAST_EXIT={}", last_exit);
}

fn cmd_apply_bb_floor(moddir: &Path) {
    let mut learned = Learned::load(&moddir.join(daemon::LEARNED_FILE));
    if learned.pending_floor_v >= 3_000_000 && learned.pending_floor_v <= 3_550_000 {
        learned.learned_floor_v = learned.pending_floor_v;
        learned.pending_floor_v = 0;
        learned.write_file(&moddir.join(daemon::LEARNED_FILE));
        let cfg = Config::load(&moddir.join(daemon::CONFIG_FILE));
        config::update_prop(moddir, &cfg, &learned);
        wake_daemon(moddir);
        telemetry::log_event(
            moddir,
            "黑匣子",
            &format!("已采纳建议保护红线 {}V", uv_to_v2(learned.learned_floor_v)),
        );
        println!("OK: 已采纳黑匣子保护红线 {}V", uv_to_v2(learned.learned_floor_v));
    } else {
        println!("ERR: 当前无待采纳的黑匣子建议");
        exit(1);
    }
}

fn cmd_dismiss_bb_floor(moddir: &Path) {
    let mut learned = Learned::load(&moddir.join(daemon::LEARNED_FILE));
    learned.pending_floor_v = 0;
    learned.write_file(&moddir.join(daemon::LEARNED_FILE));
    wake_daemon(moddir);
    telemetry::log_event(moddir, "黑匣子", "已忽略本次复盘建议");
    println!("OK: 已忽略本次黑匣子建议");
}

fn cmd_set_internal_max(moddir: &Path, arg: Option<&str>) {
    let mut cfg = Config::load(&moddir.join(daemon::CONFIG_FILE));
    if let Some(nm) = arg.and_then(|s| config::parse_uint(s)) {
        if nm >= 4_200_000 && nm <= 4_550_000 && nm - cfg.min_v as i64 >= 500_000 {
            cfg.max_v = nm as i32;
            cfg.write_file(&moddir.join(daemon::CONFIG_FILE));
            let learned = Learned::load(&moddir.join(daemon::LEARNED_FILE));
            config::update_prop(moddir, &cfg, &learned);
        }
    }
}

fn cmd_set(moddir: &Path, args: &[String]) {
    let cfg_path = moddir.join(daemon::CONFIG_FILE);
    let mut cfg = Config::load(&cfg_path);

    for arg in args {
        let (k, v) = match arg.split_once('=') {
            Some(x) => x,
            None => (arg.as_str(), arg.as_str()),
        };
        if !config::is_uint(v) {
            println!("ERR: 参数 {} 必须为非负整数", k);
            exit(1);
        }
        let val = v.parse::<i32>().unwrap_or(0);
        match k {
            "MAX_V" => cfg.max_v = val,
            "MIN_V" => cfg.min_v = val,
            "SHUTDOWN_V" => cfg.shutdown_v = val,
            "LOW_MAX" => cfg.low_max = val,
            "LOW_MIN" => cfg.low_min = val,
            "off_time" => cfg.off_time = val,
            "BATLOG" => cfg.batlog = val,
            "CURVE_MODE" => cfg.curve_mode = val,
            "CHARGE_MODE" => cfg.charge_mode = val,
            "ALLOW_REBOUND" => cfg.allow_rebound = val,
            "IR_COMP_MODE" => cfg.ir_comp_mode = val,
            "AUTO_MAX_LEARN" => cfg.auto_max_learn = val,
            "AUTO_MIN_LEARN" => cfg.auto_min_learn = val,
            "BB_AUTO_APPLY" => cfg.bb_auto_apply = val,
            "RAPID_SHUTDOWN" => cfg.rapid_shutdown = val,
            "TEMP_COMP" => cfg.temp_comp = val,
            "CURR_UNIT_OVR" => cfg.curr_unit_ovr = val,
            "CURR_SIGN_OVR" => cfg.curr_sign_ovr = val,
            "SHUTDOWN_CONFIRM" => cfg.shutdown_confirm = val,
            "TIME" => cfg.time = val,
            "SCREEN_OFF_TIME" => cfg.screen_off_time = val,
            "USB_POLL_STEP" => cfg.usb_poll_step = val,
            "BOOT_GRACE_SEC" => cfg.boot_grace_sec = val,
            "KEY_SNOOZE" => cfg.key_snooze = val,
            "KEY_SNOOZE_SEC" => cfg.key_snooze_sec = val,
            "LINK_MODE" => cfg.link_mode = val,
            "CALIB_MODE" => cfg.calib_mode = val,
            _ => {
                println!("ERR: 未知参数 {}", k);
                exit(1);
            }
        }
    }

    // 基础范围校验
    if cfg.max_v < 3_900_000 || cfg.max_v > 4_650_000 {
        println!("ERR: 满电电压超出范围 (3.90V~4.65V)");
        exit(1);
    }
    if cfg.shutdown_v < 2_850_000 || cfg.shutdown_v > 3_600_000 {
        println!("ERR: 自动关机红线超出范围 (2.85V~3.60V)");
        exit(1);
    }
    if cfg.off_time < 10 || cfg.off_time > 600 {
        println!("ERR: 关机倒计时须在 10~600 秒");
        exit(1);
    }
    if cfg.batlog != 0 && cfg.batlog != 1 {
        println!("ERR: BATLOG 须为 0/1");
        exit(1);
    }
    if cfg.curve_mode < 0 || cfg.curve_mode > 2 {
        println!("ERR: CURVE_MODE 须为 0/1/2");
        exit(1);
    }
    if cfg.charge_mode < 0 || cfg.charge_mode > 2 {
        println!("ERR: CHARGE_MODE 须为 0/1/2");
        exit(1);
    }
    if cfg.allow_rebound != 0 && cfg.allow_rebound != 1 {
        println!("ERR: ALLOW_REBOUND 须为 0/1");
        exit(1);
    }
    if cfg.ir_comp_mode != 0 && cfg.ir_comp_mode != 1 {
        println!("ERR: IR_COMP_MODE 须为 0/1");
        exit(1);
    }
    if cfg.auto_max_learn != 0 && cfg.auto_max_learn != 1 {
        println!("ERR: AUTO_MAX_LEARN 须为 0/1");
        exit(1);
    }
    if cfg.auto_min_learn != 0 && cfg.auto_min_learn != 1 {
        println!("ERR: AUTO_MIN_LEARN 须为 0/1");
        exit(1);
    }
    if cfg.bb_auto_apply != 0 && cfg.bb_auto_apply != 1 {
        println!("ERR: BB_AUTO_APPLY 须为 0/1");
        exit(1);
    }
    if cfg.rapid_shutdown != 0 && cfg.rapid_shutdown != 1 {
        println!("ERR: RAPID_SHUTDOWN 须为 0/1");
        exit(1);
    }
    if cfg.temp_comp != 0 && cfg.temp_comp != 1 {
        println!("ERR: TEMP_COMP 须为 0/1");
        exit(1);
    }
    if cfg.curr_unit_ovr < 0 || cfg.curr_unit_ovr > 2 {
        println!("ERR: CURR_UNIT_OVR 须为 0/1/2");
        exit(1);
    }
    if cfg.curr_sign_ovr < 0 || cfg.curr_sign_ovr > 2 {
        println!("ERR: CURR_SIGN_OVR 须为 0/1/2");
        exit(1);
    }
    if cfg.shutdown_confirm < 1 || cfg.shutdown_confirm > 5 {
        println!("ERR: 连击确认次数须在 1~5");
        exit(1);
    }
    if cfg.time < 5 || cfg.time > 60 {
        println!("ERR: 亮屏周期须在 5~60 秒");
        exit(1);
    }
    if cfg.screen_off_time < 10 || cfg.screen_off_time > 300 {
        println!("ERR: 息屏间隔须在 10~300 秒");
        exit(1);
    }
    if cfg.usb_poll_step < 1 || cfg.usb_poll_step > 10 {
        println!("ERR: 检测步长须在 1~10 秒");
        exit(1);
    }
    if cfg.boot_grace_sec < 0 || cfg.boot_grace_sec > 600 {
        println!("ERR: 开机保护期须在 0~600 秒");
        exit(1);
    }
    if cfg.key_snooze != 0 && cfg.key_snooze != 1 {
        println!("ERR: KEY_SNOOZE 须为 0/1");
        exit(1);
    }
    if cfg.key_snooze_sec < 30 || cfg.key_snooze_sec > 600 {
        println!("ERR: 音量键续命时长须在 30~600 秒");
        exit(1);
    }
    if cfg.link_mode != 0 && cfg.link_mode != 1 {
        println!("ERR: LINK_MODE 须为 0/1");
        exit(1);
    }
    if cfg.calib_mode != 0 && cfg.calib_mode != 1 {
        println!("ERR: CALIB_MODE 须为 0/1");
        exit(1);
    }

    // 参数自动联动
    if cfg.link_mode == 1 {
        cfg.apply_linkage();
    } else {
        if cfg.min_v < 2_850_000 || cfg.min_v > 3_600_000 {
            println!("ERR: 空电电压超出范围 (2.85V~3.60V)");
            exit(1);
        }
        if cfg.low_min < 2_850_000 || cfg.low_min > 3_700_000 {
            println!("ERR: 低电提醒下限超出范围");
            exit(1);
        }
        if cfg.low_max < 2_900_000 || cfg.low_max > 3_800_000 {
            println!("ERR: 低电提醒上限超出范围");
            exit(1);
        }
        if cfg.low_max <= cfg.low_min {
            println!("ERR: 低电提醒上限必须大于下限");
            exit(1);
        }
    }

    if cfg.max_v - cfg.min_v < 500_000 {
        println!("ERR: 满电电压须至少高于空电电压 0.50V（请调高满电电压或调低关机红线）");
        exit(1);
    }

    // 校准模式被外部开启时初始化运行时状态
    if cfg.calib_mode == 1 {
        let state = moddir.join(daemon::CALIB_STATE_FILE);
        let empty = std::fs::metadata(&state).map(|m| m.len() == 0).unwrap_or(true);
        if empty {
            let _ = std::fs::write(&state, "PHASE=0\nSAMPLES=0\nSTART_PCT=0\nMIN_V=0\nMAX_V=0\nLAST_UP=0\n");
        }
    }

    cfg.write_file(&cfg_path);
    let learned = Learned::load(&moddir.join(daemon::LEARNED_FILE));
    config::update_prop(moddir, &cfg, &learned);
    wake_daemon(moddir);

    if cfg.link_mode == 1 {
        telemetry::log_event(
            moddir,
            "配置",
            &format!("已保存：关机红线 {}V（空电/提醒参数已自动联动）", uv_to_v2(cfg.shutdown_v)),
        );
        println!(
            "OK: 已保存并秒级生效（红线 {}V，空电 {}V、提醒 {}~{}V 已联动）",
            uv_to_v2(cfg.shutdown_v),
            uv_to_v2(cfg.min_v),
            uv_to_v2(cfg.low_min),
            uv_to_v2(cfg.low_max)
        );
    } else {
        telemetry::log_event(
            moddir,
            "配置",
            &format!("已保存：关机红线 {}V（专家模式手动参数）", uv_to_v2(cfg.shutdown_v)),
        );
        println!("OK: 已保存并秒级生效（专家模式，手动空电 {}V）", uv_to_v2(cfg.min_v));
    }
}

fn cmd_calibrate_full(moddir: &Path) {
    let mut cfg = Config::load(&moddir.join(daemon::CONFIG_FILE));
    let bat_dir = sysfs::detect_bat_node(moddir);
    let v_now = match sysfs::read_one_line(&bat_dir.join("voltage_now")) {
        Some(s) => match s.parse::<i32>() {
            Ok(v) => sysfs::norm_v_helper(v),
            Err(_) => {
                println!("ERR: 无法读取当前电池电压");
                exit(1);
            }
        },
        None => {
            println!("ERR: 无法读取当前电池电压");
            exit(1);
        }
    };
    if v_now < 4_000_000 || v_now > 4_650_000 {
        println!(
            "ERR: 当前等效电压 {}V 不在满电区间 (4.00V~4.65V)，请充满电静置后再校准",
            uv_to_v2(v_now)
        );
        exit(1);
    }
    let rounded = (v_now + 5000) / 10_000 * 10_000;
    if rounded - cfg.min_v < 500_000 {
        println!("ERR: 校准电压与空电电压过近，请先调低关机红线");
        exit(1);
    }
    cfg.max_v = rounded;
    cfg.write_file(&moddir.join(daemon::CONFIG_FILE));
    let learned = Learned::load(&moddir.join(daemon::LEARNED_FILE));
    config::update_prop(moddir, &cfg, &learned);
    wake_daemon(moddir);
    telemetry::log_event(moddir, "配置", &format!("手动校准满电电压为 {}V", uv_to_v2(cfg.max_v)));
    println!("OK: 已将当前电压 {}V 设为满电 100% 电压", uv_to_v2(cfg.max_v));
}

fn cmd_calib_start(moddir: &Path) {
    let mut cfg = Config::load(&moddir.join(daemon::CONFIG_FILE));
    cfg.calib_mode = 1;
    let _ = std::fs::write(
        moddir.join(daemon::CALIB_STATE_FILE),
        "PHASE=0\nSAMPLES=0\nSTART_PCT=0\nMIN_V=0\nMAX_V=0\nLAST_UP=0\nBUCKETS=0\nMIN_PCT=0\nMAX_PCT=0\nSPAN=0\n",
    );
    let _ = std::fs::remove_file(moddir.join(daemon::CALIB_SAMPLES_FILE));
    cfg.write_file(&moddir.join(daemon::CONFIG_FILE));
    wake_daemon(moddir);
    telemetry::log_event(
        moddir,
        "校准",
        "已开启极速断点校准：无需充满，累计 ≥25% 放电跨度即可生成专属曲线（支持中途插电断点续采）",
    );
    println!("OK: 极速引导校准已开启（放电中立即开始，支持中途插电断点续采）");
}

fn cmd_calib_abort(moddir: &Path) {
    let mut cfg = Config::load(&moddir.join(daemon::CONFIG_FILE));
    cfg.calib_mode = 0;
    cfg.write_file(&moddir.join(daemon::CONFIG_FILE));
    let _ = std::fs::remove_file(moddir.join(daemon::CALIB_STATE_FILE));
    let _ = std::fs::remove_file(moddir.join(daemon::CALIB_SAMPLES_FILE));
    wake_daemon(moddir);
    telemetry::log_event(moddir, "校准", "用户放弃本次引导校准");
    println!("OK: 已放弃引导校准");
}

fn cmd_calib_finish(moddir: &Path) {
    let mut d = daemon::Daemon::new(moddir.clone());
    d.cfg = Config::load(&moddir.join(daemon::CONFIG_FILE));
    d.learned = Learned::load(&moddir.join(daemon::LEARNED_FILE));
    if d.cfg.calib_mode != 1 {
        println!("ERR: 引导校准未开启（请先在 WebUI 点击「开始引导校准」）");
        exit(1);
    }
    let knots = match d.calc_curve_knots() {
        Ok(k) => k,
        Err(e) => {
            telemetry::log_event(moddir, "校准", &format!("手动结算失败：{}", e));
            println!("ERR: {}", e);
            exit(1);
        }
    };
    match d.apply_calibration_result(&knots, "手动结算") {
        Ok(msg) => {
            wake_daemon(moddir);
            println!("OK: {}", msg);
        }
        Err(msg) => {
            telemetry::log_event(moddir, "校准", &format!("手动结算失败：{}", msg));
            println!("ERR: {}", msg);
            exit(1);
        }
    }
}

fn cmd_reset_learned(moddir: &Path) {
    let _ = std::fs::remove_file(moddir.join(daemon::LEARNED_FILE));
    let _ = std::fs::remove_file(moddir.join(BB_EVENTS_FILE));
    let cfg = Config::load(&moddir.join(daemon::CONFIG_FILE));
    let learned = Learned::load(&moddir.join(daemon::LEARNED_FILE));
    config::update_prop(moddir, &cfg, &learned);
    wake_daemon(moddir);
    telemetry::log_event(moddir, "服务", "已重置全部学习数据（内阻/黑匣子/满电学习）");
    println!("OK: 已彻底清空内存与磁盘中的内阻、黑匣子与学习记录");
}

fn cmd_snooze(moddir: &Path, arg: Option<&str>) {
    let mut sec: i64 = match arg.and_then(|s| config::parse_uint(s)) {
        Some(v) => v,
        None => 180,
    };
    if sec < 30 {
        sec = 30;
    }
    if sec > 1800 {
        sec = 1800;
    }
    let until = sysfs::get_uptime_sec() as i64 + sec;
    let _ = std::fs::write(moddir.join(daemon::SNOOZE_FILE), format!("{}\n", until));
    ipc_send(moddir, &format!("snooze {}", sec));
    wake_daemon(moddir);
    platform::android::notifi(
        "紧急续命",
        &format!("已延后自动关机 {} 秒，请尽快处理完并充电", sec),
    );
    telemetry::log_event(moddir, "续命", &format!("WebUI 手动延后自动关机 {} 秒", sec));
    println!("OK: 已紧急延后自动关机 {} 秒", sec);
}

fn cmd_cancel_shutdown(moddir: &Path) {
    let _ = std::fs::write(moddir.join(daemon::SNOOZE_FILE), "999999999\n");
    ipc_send(moddir, "cancel_shutdown");
    wake_daemon(moddir);
    platform::android::notifi("已取消关机", "本次放电周期已暂停自动关机，下次充电后自动恢复保护");
    telemetry::log_event(moddir, "关机保护", "已取消本次自动关机（下次充电后自动恢复）");
    println!("OK: 已取消本次自动关机（下次充电后自动恢复）");
}

fn cmd_resume_shutdown(moddir: &Path) {
    let _ = std::fs::remove_file(moddir.join(daemon::SNOOZE_FILE));
    wake_daemon(moddir);
    telemetry::log_event(moddir, "关机保护", "已恢复自动关机保护");
    println!("OK: 已恢复自动关机保护");
}

fn cmd_reset(moddir: &Path) {
    let cfg = Config::defaults();
    let _ = std::fs::remove_file(moddir.join(daemon::SNOOZE_FILE));
    let _ = std::fs::remove_file(moddir.join(daemon::CALIB_CURVE_FILE));
    let _ = std::fs::remove_file(moddir.join(daemon::CALIB_SAMPLES_FILE));
    cfg.write_file(&moddir.join(daemon::CONFIG_FILE));
    let learned = Learned::load(&moddir.join(daemon::LEARNED_FILE));
    config::update_prop(moddir, &cfg, &learned);
    wake_daemon(moddir);
    telemetry::log_event(moddir, "配置", "已恢复 v5.4 默认配置（并清空专属校准曲线）");
    println!("OK: 已恢复 v5.4 默认配置（专属校准曲线已一并清空）");
}

fn cmd_clear_log(moddir: &Path) {
    let _ = std::fs::remove_file(moddir.join(LOG_FILE));
    let _ = std::fs::remove_file(moddir.join(format!("{}.old", LOG_FILE)));
    let _ = std::fs::remove_file(moddir.join(telemetry::HIST_FILE));
    let _ = std::fs::remove_file(moddir.join(telemetry::EVENTS_FILE));
    telemetry::log_event(moddir, "服务", "已清空详细日志、历史曲线与事件记录");
    println!("OK: 已清空日志文件、历史曲线与事件记录");
}

fn cmd_action(moddir: &Path) {
    if is_counting_down(moddir) {
        cmd_snooze(moddir, Some("180"));
        println!("========================================");
        println!("  检测到正在关机倒计时 -> 已触发紧急续命！");
        println!("  自动关机已延后 3 分钟，请尽快保存/充电。");
        println!("  （倒计时期间直接按音量键也可盲操续命）");
        println!("========================================");
        return;
    }
    cmd_cycle(moddir);
}

fn cmd_cycle(moddir: &Path) {
    let cfg_path = moddir.join(daemon::CONFIG_FILE);
    let mut cfg = Config::load(&cfg_path);
    let learned = Learned::load(&moddir.join(daemon::LEARNED_FILE));
    let old_sv = cfg.shutdown_v;

    let presets = config::PRESETS;
    let first_sv = presets[0];
    let mut next_sv = 0;
    for &p in &presets {
        if p > old_sv {
            next_sv = p;
            break;
        }
    }
    if next_sv == 0 {
        next_sv = first_sv;
    }

    cfg.shutdown_v = next_sv;
    if cfg.link_mode == 1 {
        cfg.apply_linkage();
    }
    cfg.write_file(&cfg_path);
    config::update_prop(moddir, &cfg, &learned);
    wake_daemon(moddir);

    let bat_dir = sysfs::detect_bat_node(moddir);
    let v_now = sysfs::read_one_line(&bat_dir.join("voltage_now"))
        .and_then(|s| s.parse::<i32>().ok())
        .map(sysfs::norm_v_helper);
    let cap = sysfs::read_one_line(&bat_dir.join("capacity"));
    let dpid = get_daemon_pid(moddir);
    let key_node = platform::evdev::detect_key_node(moddir);

    println!("========================================");
    println!("    电压电量模拟服务 v6.0 - 档位切换    ");
    println!("========================================");
    println!("自动关机红线  ：{}V -> {}V", uv_to_v2(old_sv), uv_to_v2(next_sv));
    print!("可选档位      ：");
    for &p in &presets {
        let pv = uv_to_v2(p);
        if p == next_sv {
            print!("[{}V] ", pv);
        } else {
            print!("{}V ", pv);
        }
    }
    println!();
    println!("----------------------------------------");
    println!(
        "电量标尺      ：{}V ~ {}V ({}S)",
        uv_to_v2(cfg.min_v),
        uv_to_v2(cfg.max_v),
        learned.cell_div
    );
    println!(
        "常规/低电内阻 ：{}mΩ({}) / {}mΩ({})",
        learned.r_int_mohm, learned.r_samples, learned.r_low_mohm, learned.r_low_samples
    );
    if cfg.link_mode == 1 {
        println!(
            "参数联动      ：低电提醒 {}~{}V（已自动更新）",
            uv_to_v2(cfg.low_min),
            uv_to_v2(cfg.low_max)
        );
    }
    match &key_node {
        Some(p) => println!("盲操续命      ：可用（音量键节点 {}）", p.display()),
        None => println!("盲操续命      ：本机型未识别到音量键节点（可写入 .keynode_override 手动指定）"),
    }
    if learned.pending_floor_v > 0 {
        println!("黑匣子待确认  ：建议将红线提升至 {}V（请在 WebUI 确认）", uv_to_v2(learned.pending_floor_v));
    } else if learned.learned_floor_v > 0 {
        println!(
            "黑匣子保护线  ：{}V (记录: {} 次)",
            uv_to_v2(learned.learned_floor_v),
            learned.blackscreen_cnt
        );
    } else {
        println!("黑匣子状态    ：暂无硬断电记录");
    }
    if let Some(v) = v_now {
        println!("当前等效电压  ：{}V", uv_to_v2(v));
    }
    if let Some(c) = cap {
        println!("系统真实电量  ：{}%", c);
    }
    if let Some(pid) = dpid {
        println!("后台服务状态  ：运行中 (PID: {}，已发送秒级生效信号)", pid);
    } else {
        println!("后台服务状态  ：未运行");
    }
    println!("----------------------------------------");
    println!("提示：倒计时期间按音量键 / 点「操作」按钮均可延后 3 分钟！");
}

fn cmd_migrate(moddir: &Path) {
    let mut cfg = Config::load(&moddir.join(daemon::CONFIG_FILE));
    let migrated_from: &str;
    if cfg.conf_ver == 1 {
        migrated_from = "v5.4";
    } else {
        let (dmin, dlmin, dlmax) = cfg.derive_linked_vals();
        if cfg.min_v == dmin && cfg.low_min == dlmin && cfg.low_max == dlmax {
            cfg.link_mode = 1;
        } else {
            cfg.link_mode = 0;
        }
        cfg.conf_ver = 1;
        migrated_from = "v5.3";
    }
    cfg.write_file(&moddir.join(daemon::CONFIG_FILE));
    let learned = Learned::load(&moddir.join(daemon::LEARNED_FILE));
    config::update_prop(moddir, &cfg, &learned);
    if migrated_from == "v5.4" {
        println!("OK: 配置已是最新格式（CONF_VER={}）", cfg.conf_ver);
    } else if cfg.link_mode == 1 {
        println!("OK: 已迁移到 v5.4 配置格式（参数自动联动已开启）");
    } else {
        println!("OK: 已迁移到 v5.4 配置格式（检测到自定义参数，保留专家手动模式）");
    }
}

fn cmd_sync_prop(moddir: &Path) {
    let cfg = Config::load(&moddir.join(daemon::CONFIG_FILE));
    let learned = Learned::load(&moddir.join(daemon::LEARNED_FILE));
    config::update_prop(moddir, &cfg, &learned);
}

fn cmd_restart(moddir: &Path) {
    let _ = std::fs::remove_file(moddir.join(WATCHDOG_PAUSE_FILE));
    kill_daemon_all(moddir);
    spawn_daemon(moddir);
    std::thread::sleep(Duration::from_secs(1));
    match get_daemon_pid(moddir) {
        Some(pid) => {
            telemetry::log_event(moddir, "服务", &format!("服务已重启 (PID: {})", pid));
            println!("OK: 服务已重启 (PID: {})", pid);
        }
        None => {
            telemetry::log_event(moddir, "服务", "重启命令已发送，但未检测到进程");
            println!("ERR: 重启命令已发送，但未检测到进程");
            exit(1);
        }
    }
}

fn cmd_stop(moddir: &Path) {
    let _ = std::fs::write(moddir.join(WATCHDOG_PAUSE_FILE), "1\n");
    kill_daemon_all(moddir);
    let _ = std::fs::remove_file(moddir.join(daemon::DAEMON_PID_FILE));
    let _ = std::fs::remove_file(moddir.join(daemon::COUNTDOWN_PID_FILE));
    let _ = std::fs::remove_file(moddir.join(KEYREQ_FILE));
    platform::android::dumpsys_reset();
    telemetry::log_event(moddir, "服务", "服务已停止并重置电池状态（看门狗已暂停）");
    println!("OK: 服务已停止并重置电池状态（看门狗已暂停）");
}
