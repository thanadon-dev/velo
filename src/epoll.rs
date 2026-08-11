use std::io;
use std::os::fd::{AsRawFd, RawFd};

pub const IN: u32 = 0x1;
pub const OUT: u32 = 0x4;
pub const ERR: u32 = 0x8;
pub const HUP: u32 = 0x10;
pub const RDHUP: u32 = 0x2000;
pub const EXCLUSIVE: u32 = 1 << 28;

const ADD: i32 = 1;
const DEL: i32 = 2;
const MOD: i32 = 3;

#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
pub struct Event {
    pub events: u32,
    pub data: u64,
}

extern "C" {
    fn epoll_create1(flags: i32) -> i32;
    fn epoll_ctl(epfd: i32, op: i32, fd: i32, event: *mut Event) -> i32;
    fn epoll_wait(epfd: i32, events: *mut Event, maxevents: i32, timeout: i32) -> i32;
    fn close(fd: i32) -> i32;
}

pub struct Epoll {
    fd: RawFd,
}

impl Epoll {
    pub fn new() -> io::Result<Epoll> {
        let fd = unsafe { epoll_create1(0) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Epoll { fd })
    }

    pub fn add(&self, target: &impl AsRawFd, events: u32, data: u64) -> io::Result<()> {
        self.ctl(ADD, target.as_raw_fd(), events, data)
    }

    pub fn modify(&self, target: &impl AsRawFd, events: u32, data: u64) -> io::Result<()> {
        self.ctl(MOD, target.as_raw_fd(), events, data)
    }

    pub fn remove(&self, target: &impl AsRawFd) -> io::Result<()> {
        self.ctl(DEL, target.as_raw_fd(), 0, 0)
    }

    fn ctl(&self, op: i32, fd: RawFd, events: u32, data: u64) -> io::Result<()> {
        let mut ev = Event { events, data };
        let rc = unsafe { epoll_ctl(self.fd, op, fd, &mut ev) };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn wait(&self, buf: &mut [Event], timeout_ms: i32) -> io::Result<usize> {
        let n = unsafe { epoll_wait(self.fd, buf.as_mut_ptr(), buf.len() as i32, timeout_ms) };
        if n < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                return Ok(0);
            }
            return Err(e);
        }
        Ok(n as usize)
    }
}

impl Drop for Epoll {
    fn drop(&mut self) {
        unsafe { close(self.fd) };
    }
}
