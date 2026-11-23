//! evdev 音量键节点探测与按键读取。
//!
//! 复刻自 common.sh 的 detect_key_node（getevent -lp 解析）与 daemon.sh 的
//! start_key_monitor（getevent -l 监听），改用 EVIOCGBIT/EVIOCGNAME ioctl 直接
//! 探测，不依赖 getevent 二进制。

use std::ffi::{CStr, CString};
use std::fs;
use std::os::fd::RawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};

use crate::sysfs::read_one_line;

pub const KEY_VOLUMEUP: u16 = 115;
pub const KEY_VOLUMEDOWN: u16 = 114;

const KEY_CNT: usize = 0x300; // KEY_MAX = 0x2ff
const BITS_LEN: usize = KEY_CNT / 8; // 96

#[repr(C)]
pub struct InputEvent {
    pub time: libc::timeval,
    pub type_: u16,
    pub code: u16,
    pub value: i32,
}

fn eviocgbit(ev: u32, len: u32) -> u64 {
    (2u64 << 30) | (0x45u64 << 8) | (0x20u64 + ev as u64) | ((len as u64) << 16)
}

fn eviocgname(len: u32) -> u64 {
    (2u64 << 30) | (0x45u64 << 8) | 0x06u64 | ((len as u64) << 16)
}

fn ioctl(fd: RawFd, request: u64, arg: *mut u8) -> i32 {
    unsafe { libc::ioctl(fd, request as _, arg as *mut libc::c_void) }
}

fn open_dev(path: &Path) -> Option<RawFd> {
    let c = CString::new(path.as_os_str().as_bytes()).ok()?;
    let fd = unsafe {
        libc::open(
            c.as_ptr(),
            libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        None
    } else {
        Some(fd)
    }
}

fn is_char_device(p: &Path) -> bool {
    fs::metadata(p)
        .map(|m| m.file_type().is_char_device())
        .unwrap_or(false)
}

fn has_key(bits: &[u8], key: u16) -> bool {
    let idx = (key / 8) as usize;
    let bit = (key % 8) as usize;
    idx < bits.len() && (bits[idx] >> bit) & 1 == 1
}

/// 探测音量键节点：override > cache > 扫描 /dev/input/event*。
pub fn detect_key_node(moddir: &Path) -> Option<PathBuf> {
    if let Some(v) = read_one_line(&moddir.join(".keynode_override")) {
        let p = PathBuf::from(&v);
        if is_char_device(&p) {
            return Some(p);
        }
    }
    if let Some(v) = read_one_line(&moddir.join(".keynode")) {
        let p = PathBuf::from(&v);
        if is_char_device(&p) {
            return Some(p);
        }
    }
    let entries = fs::read_dir("/dev/input").ok()?;
    let mut fallback: Option<PathBuf> = None;
    for e in entries.flatten() {
        let path = e.path();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if !name.starts_with("event") {
            continue;
        }
        let fd = match open_dev(&path) {
            Some(f) => f,
            None => continue,
        };
        let mut bits = [0u8; BITS_LEN];
        let ok = ioctl(fd, eviocgbit(1, BITS_LEN as u32), bits.as_mut_ptr()) >= 0;
        let has_vol = ok && (has_key(&bits, KEY_VOLUMEUP) || has_key(&bits, KEY_VOLUMEDOWN));
        if !has_vol {
            unsafe { libc::close(fd) };
            continue;
        }
        let mut namebuf = [0u8; 256];
        let devname = if ioctl(fd, eviocgname(256), namebuf.as_mut_ptr()) >= 0 {
            CStr::from_bytes_until_nul(&namebuf)
                .map(|c| c.to_string_lossy().to_string())
                .unwrap_or_default()
        } else {
            String::new()
        };
        unsafe { libc::close(fd) };
        let dl = devname.to_lowercase();
        if dl.contains("vol") || dl.contains("key") {
            let _ = fs::write(moddir.join(".keynode"), format!("{}\n", path.display()));
            return Some(path);
        }
        if fallback.is_none() {
            fallback = Some(path);
        }
    }
    if let Some(ref p) = fallback {
        let _ = fs::write(moddir.join(".keynode"), format!("{}\n", p.display()));
    }
    fallback
}

/// 已打开的按键设备（倒计时期间 epoll 监听用）。
pub struct KeyReader {
    fd: RawFd,
}

impl KeyReader {
    pub fn open(path: &Path) -> Option<Self> {
        open_dev(path).map(|fd| KeyReader { fd })
    }

    pub fn fd(&self) -> RawFd {
        self.fd
    }

    /// 读空缓冲；检测到音量键按下（EV_KEY value=1）返回 true。
    pub fn read_volume_down(&self) -> bool {
        let mut buf = [0u8; 512];
        let ev_sz = std::mem::size_of::<InputEvent>();
        let mut pressed = false;
        loop {
            let n = unsafe {
                libc::read(
                    self.fd,
                    buf.as_mut_ptr() as *mut libc::c_void,
                    buf.len(),
                )
            };
            if n <= 0 {
                break;
            }
            let n = n as usize;
            let mut off = 0usize;
            while off + ev_sz <= n {
                let ev: InputEvent =
                    unsafe { std::ptr::read_unaligned(buf[off..].as_ptr() as *const InputEvent) };
                off += ev_sz;
                if ev.type_ == 1
                    && ev.value == 1
                    && (ev.code == KEY_VOLUMEUP || ev.code == KEY_VOLUMEDOWN)
                {
                    pressed = true;
                }
            }
        }
        pressed
    }
}

impl Drop for KeyReader {
    fn drop(&mut self) {
        unsafe { libc::close(self.fd) };
    }
}
