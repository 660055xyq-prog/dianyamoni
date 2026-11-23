//! AF_NETLINK + NETLINK_KOBJECT_UEVENT 内核 uevent 监听，过滤 power_supply。
//!
//! 用于取代原 v5.52 的 USB 轮询：插电/拔电时内核广播 power_supply uevent，
//! 事件循环据此立即重读 usb/status 并重跑流水线。

use std::os::fd::RawFd;

pub struct UeventSocket {
    fd: RawFd,
}

impl UeventSocket {
    pub fn new() -> Option<Self> {
        unsafe {
            let fd = libc::socket(
                libc::AF_NETLINK,
                libc::SOCK_RAW | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
                libc::NETLINK_KOBJECT_UEVENT,
            );
            if fd < 0 {
                return None;
            }
            let mut addr: libc::sockaddr_nl = std::mem::zeroed();
            addr.nl_family = libc::AF_NETLINK as u16;
            // 接收所有多播组（内核把 kobject uevent 广播到组 1）
            addr.nl_groups = 1u32;
            let r = libc::bind(
                fd,
                &addr as *const libc::sockaddr_nl as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
            );
            if r < 0 {
                libc::close(fd);
                return None;
            }
            Some(UeventSocket { fd })
        }
    }

    pub fn fd(&self) -> RawFd {
        self.fd
    }

    /// 清空缓冲；返回其中是否含有 power_supply 相关事件。
    pub fn drain(&self) -> bool {
        let mut buf = [0u8; 8192];
        let mut found = false;
        loop {
            let n = unsafe {
                libc::recv(
                    self.fd,
                    buf.as_mut_ptr() as *mut libc::c_void,
                    buf.len(),
                    0,
                )
            };
            if n <= 0 {
                break;
            }
            let data = &buf[..n as usize];
            if contains_bytes(data, b"power_supply") {
                found = true;
            }
        }
        found
    }
}

impl Drop for UeventSocket {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.fd);
        }
    }
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}
