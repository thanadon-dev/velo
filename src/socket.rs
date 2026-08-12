use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::{UnixListener, UnixStream};

pub enum Listener {
    Tcp(TcpListener),
    Unix(UnixListener, std::path::PathBuf),
}

impl Listener {
    pub fn bind(addr: &str) -> io::Result<Listener> {
        let Some(path) = addr.strip_prefix("unix:") else {
            return Ok(Listener::Tcp(TcpListener::bind(addr)?));
        };
        let path = std::path::PathBuf::from(path);
        if let Ok(meta) = std::fs::metadata(&path) {
            if !meta.file_type().is_socket() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("{} exists and is not a socket", path.display()),
                ));
            }
            if UnixStream::connect(&path).is_ok() {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    format!("{} is already served", path.display()),
                ));
            }
            std::fs::remove_file(&path)?;
        }
        let listener = UnixListener::bind(&path)?;
        let mode = std::env::var("VELO_SOCKET_MODE")
            .ok()
            .and_then(|v| u32::from_str_radix(&v, 8).ok())
            .unwrap_or(0o660);
        set_mode(&path, mode)?;
        Ok(Listener::Unix(listener, path))
    }

    pub fn set_nonblocking(&self, on: bool) -> io::Result<()> {
        match self {
            Listener::Tcp(l) => l.set_nonblocking(on),
            Listener::Unix(l, _) => l.set_nonblocking(on),
        }
    }

    pub fn accept(&self) -> io::Result<(Stream, String)> {
        match self {
            Listener::Tcp(l) => {
                let (stream, peer) = l.accept()?;
                Ok((Stream::Tcp(stream), peer.ip().to_string()))
            }
            Listener::Unix(l, _) => {
                let (stream, _) = l.accept()?;
                Ok((Stream::Unix(stream), "unix".to_string()))
            }
        }
    }
}

impl AsRawFd for Listener {
    fn as_raw_fd(&self) -> RawFd {
        match self {
            Listener::Tcp(l) => l.as_raw_fd(),
            Listener::Unix(l, _) => l.as_raw_fd(),
        }
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        if let Listener::Unix(_, path) = self {
            let _ = std::fs::remove_file(path);
        }
    }
}

pub enum Stream {
    Tcp(TcpStream),
    Unix(UnixStream),
}

impl Stream {
    pub fn connect(target: &str) -> io::Result<Stream> {
        match target.strip_prefix("unix:") {
            Some(path) => Ok(Stream::Unix(UnixStream::connect(path)?)),
            None => Ok(Stream::Tcp(TcpStream::connect(target)?)),
        }
    }

    pub fn set_read_timeout(&self, dur: Option<std::time::Duration>) -> io::Result<()> {
        match self {
            Stream::Tcp(s) => s.set_read_timeout(dur),
            Stream::Unix(s) => s.set_read_timeout(dur),
        }
    }

    pub fn set_nonblocking(&self, on: bool) -> io::Result<()> {
        match self {
            Stream::Tcp(s) => s.set_nonblocking(on),
            Stream::Unix(s) => s.set_nonblocking(on),
        }
    }

    pub fn set_nodelay(&self) {
        if let Stream::Tcp(s) = self {
            let _ = s.set_nodelay(true);
        }
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Stream::Tcp(s) => s.read(buf),
            Stream::Unix(s) => s.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Stream::Tcp(s) => s.write(buf),
            Stream::Unix(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Stream::Tcp(s) => s.flush(),
            Stream::Unix(s) => s.flush(),
        }
    }
}

impl AsRawFd for Stream {
    fn as_raw_fd(&self) -> RawFd {
        match self {
            Stream::Tcp(s) => s.as_raw_fd(),
            Stream::Unix(s) => s.as_raw_fd(),
        }
    }
}

extern "C" {
    fn chmod(path: *const u8, mode: u32) -> i32;
}

fn set_mode(path: &std::path::Path, mode: u32) -> io::Result<()> {
    let mut c_path = path.to_string_lossy().into_owned().into_bytes();
    c_path.push(0);
    if unsafe { chmod(c_path.as_ptr(), mode) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
