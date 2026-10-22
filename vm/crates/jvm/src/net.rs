//! Sockets and pipes behind java.nio channels.
//!
//! Sockets share the file descriptor number space with files. Natively they
//! are non-blocking `std::net` sockets; readiness for `poll` is probed
//! without consuming data (a pending connection is accepted early and kept).
//! In the browser this table will be backed by a virtual network instead.

use std::collections::{HashMap, VecDeque};
use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

pub const POLLIN: i16 = 0x001;
pub const POLLOUT: i16 = 0x004;
pub const POLLERR: i16 = 0x008;
pub const POLLHUP: i16 = 0x010;
pub const POLLNVAL: i16 = 0x020;

pub enum Endpoint {
    /// Created by socket0, not yet bound or connected.
    Unbound { bind: Option<SocketAddr> },
    Listener { listener: TcpListener, pending: VecDeque<(TcpStream, SocketAddr)> },
    Stream(TcpStream),
    PipeRead(Arc<Mutex<VecDeque<u8>>>),
    PipeWrite(Arc<Mutex<VecDeque<u8>>>),
}

pub type Socket = Arc<Mutex<Endpoint>>;

#[derive(Default)]
pub struct Sockets {
    table: Mutex<HashMap<i32, Socket>>,
}

impl Sockets {
    pub fn insert(&self, fd: i32, endpoint: Endpoint) {
        self.table.lock().unwrap().insert(fd, Arc::new(Mutex::new(endpoint)));
    }

    pub fn get(&self, fd: i32) -> Option<Socket> {
        self.table.lock().unwrap().get(&fd).cloned()
    }

    pub fn close(&self, fd: i32) -> bool {
        self.table.lock().unwrap().remove(&fd).is_some()
    }
}

impl Endpoint {
    /// poll(2)-style readiness for the requested events.
    pub fn ready(&mut self, events: i16) -> i16 {
        let mut revents = 0;
        match self {
            Endpoint::Unbound { .. } => {}
            Endpoint::Listener { listener, pending } => {
                if events & POLLIN != 0 {
                    if pending.is_empty() {
                        if let Ok((stream, addr)) = listener.accept() {
                            pending.push_back((stream, addr));
                        }
                    }
                    if !pending.is_empty() {
                        revents |= POLLIN;
                    }
                }
            }
            Endpoint::Stream(stream) => {
                if events & POLLIN != 0 {
                    let mut byte = [0u8];
                    match stream.peek(&mut byte) {
                        Ok(0) => revents |= POLLIN | POLLHUP,
                        Ok(_) => revents |= POLLIN,
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                        Err(_) => revents |= POLLERR,
                    }
                }
                if events & POLLOUT != 0 {
                    revents |= POLLOUT;
                }
            }
            Endpoint::PipeRead(buffer) => {
                if events & POLLIN != 0 && !buffer.lock().unwrap().is_empty() {
                    revents |= POLLIN;
                }
            }
            Endpoint::PipeWrite(_) => revents |= events & POLLOUT,
        }
        revents
    }

    /// Non-blocking read: Ok(None) when no data is available yet.
    pub fn read(&mut self, buf: &mut [u8]) -> io::Result<Option<usize>> {
        let result = match self {
            Endpoint::Stream(stream) => stream.read(buf),
            Endpoint::PipeRead(buffer) => {
                let mut buffer = buffer.lock().unwrap();
                if buffer.is_empty() {
                    return Ok(None);
                }
                let n = buf.len().min(buffer.len());
                for (slot, byte) in buf.iter_mut().zip(buffer.drain(..n)) {
                    *slot = byte;
                }
                Ok(n)
            }
            _ => return Err(io::Error::from(io::ErrorKind::NotConnected)),
        };
        match result {
            Ok(n) => Ok(Some(n)),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Non-blocking write: Ok(None) when the socket cannot take data yet.
    pub fn write(&mut self, data: &[u8]) -> io::Result<Option<usize>> {
        let result = match self {
            Endpoint::Stream(stream) => stream.write(data),
            Endpoint::PipeWrite(buffer) => {
                buffer.lock().unwrap().extend(data);
                Ok(data.len())
            }
            _ => return Err(io::Error::from(io::ErrorKind::NotConnected)),
        };
        match result {
            Ok(n) => Ok(Some(n)),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn listen(&mut self) -> io::Result<()> {
        let Endpoint::Unbound { bind } = self else { return Err(io::Error::from(io::ErrorKind::InvalidInput)) };
        let address = bind.unwrap_or_else(|| SocketAddr::from(([0, 0, 0, 0], 0)));
        let listener = TcpListener::bind(address)?;
        listener.set_nonblocking(true)?;
        *self = Endpoint::Listener { listener, pending: VecDeque::new() };
        Ok(())
    }

    pub fn connect(&mut self, address: SocketAddr) -> io::Result<()> {
        let stream = TcpStream::connect(address)?;
        stream.set_nonblocking(true)?;
        stream.set_nodelay(true)?;
        *self = Endpoint::Stream(stream);
        Ok(())
    }

    /// A pending connection, if any.
    pub fn accept(&mut self) -> io::Result<Option<(TcpStream, SocketAddr)>> {
        let Endpoint::Listener { listener, pending } = self else { return Err(io::Error::from(io::ErrorKind::InvalidInput)) };
        if let Some(connection) = pending.pop_front() {
            return Ok(Some(connection));
        }
        match listener.accept() {
            Ok(connection) => Ok(Some(connection)),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn local_address(&self) -> Option<SocketAddr> {
        match self {
            Endpoint::Unbound { bind } => *bind,
            Endpoint::Listener { listener, .. } => listener.local_addr().ok(),
            Endpoint::Stream(stream) => stream.local_addr().ok(),
            _ => None,
        }
    }

    pub fn remote_address(&self) -> Option<SocketAddr> {
        match self {
            Endpoint::Stream(stream) => stream.peer_addr().ok(),
            _ => None,
        }
    }

    pub fn shutdown(&mut self, how: Shutdown) -> io::Result<()> {
        match self {
            Endpoint::Stream(stream) => stream.shutdown(how),
            _ => Ok(()),
        }
    }

    pub fn available(&mut self) -> usize {
        match self {
            Endpoint::Stream(stream) => {
                let mut buf = [0u8; 4096];
                stream.peek(&mut buf).unwrap_or(0)
            }
            Endpoint::PipeRead(buffer) => buffer.lock().unwrap().len(),
            _ => 0,
        }
    }
}

/// An in-memory pipe (Selector wakeups): (read end, write end).
pub fn pipe() -> (Endpoint, Endpoint) {
    let buffer = Arc::new(Mutex::new(VecDeque::new()));
    (Endpoint::PipeRead(buffer.clone()), Endpoint::PipeWrite(buffer))
}
