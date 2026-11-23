//! AF_UNIX IPC：守护进程本地控制 socket（$MODDIR/.vbat.sock，权限 0600）。
//!
//! 取代 v5.52 的 USR1 信号唤醒与 kill 机制，用于 CLI 向守护发送
//! wake / reload / cancel_shutdown / stop / snooze 等行协议命令。
//! status 等只读查询仍走文件（与 config_helper.sh 一致），socket 只承载控制命令。

use std::ffi::CStr;
use std::os::fd::RawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

pub const IPC_SOCK_NAME: &str = ".vbat.sock";

pub struct IpcListener {
    fd: RawFd,
}

impl IpcListener {
    /// 清理残留 socket 文件并创建监听。
    pub fn new(moddir: &Path) -> Option<Self> {
        let path = moddir.join(IPC_SOCK_NAME);
        let _ = std::fs::remove_file(&path);

        let os = path.as_os_str().as_bytes();
        if os.len() + 1 > 108 {
            // sun_path 溢出，放弃 IPC
            return None;
        }

        unsafe {
            let fd = libc::socket(
                libc::AF_UNIX,
                libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
                0,
            );
            if fd < 0 {
                return None;
            }
            let mut addr: libc::sockaddr_un = std::mem::zeroed();
            addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
            let sun_path = addr.sun_path.as_mut_ptr() as *mut u8;
            std::ptr::copy_nonoverlapping(os.as_ptr(), sun_path, os.len());
            let len = (std::mem::size_of::<libc::sa_family_t>() + os.len() + 1) as libc::socklen_t;
            if libc::bind(fd, &addr as *const libc::sockaddr_un as *const libc::sockaddr, len) < 0 {
                libc::close(fd);
                return None;
            }
            // 权限 0600
            let cpath = CStr::from_bytes_with_nul(&{
                let mut v = os.to_vec();
                v.push(0);
                v
            })
            .ok()?;
            libc::chmod(cpath.as_ptr(), 0o600);
            if libc::listen(fd, 4) < 0 {
                libc::close(fd);
                return None;
            }
            Some(IpcListener { fd })
        }
    }

    pub fn fd(&self) -> RawFd {
        self.fd
    }

    /// 接受一个连接并读取一行命令（非阻塞，无连接返回 None）。
    pub fn accept_command(&self) -> Option<String> {
        unsafe {
            let cfd = libc::accept(self.fd, std::ptr::null_mut(), std::ptr::null_mut());
            if cfd < 0 {
                return None;
            }
            // 防止异常客户端连接后不写数据导致单线程 epoll 阻塞
            let tv = libc::timeval { tv_sec: 0, tv_usec: 200_000 };
            libc::setsockopt(
                cfd,
                libc::SOL_SOCKET,
                libc::SO_RCVTIMEO,
                &tv as *const libc::timeval as *const libc::c_void,
                std::mem::size_of::<libc::timeval>() as libc::socklen_t,
            );
            let mut buf = [0u8; 512];
            let n = libc::read(cfd, buf.as_mut_ptr() as *mut libc::c_void, buf.len());
            libc::close(cfd);
            if n <= 0 {
                return None;
            }
            let data = &buf[..n as usize];
            let line = data.split(|&b| b == b'\n').next().unwrap_or(&[]);
            let cmd = String::from_utf8_lossy(line).trim().to_string();
            if cmd.is_empty() {
                None
            } else {
                Some(cmd)
            }
        }
    }
}

impl Drop for IpcListener {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.fd);
        }
    }
}
