//! TCP connections in the browser, through rustcraft-bridge: a program on the
//! player's computer that joins WebSockets from the page to TCP (see
//! bridge/). The page's main thread holds the WebSocket
//! (launcher/src/jvm/net.ts); the data passes through two rings in linear
//! memory that the VM and the page share.
//!
//! Host names do not resolve in the browser: a name gets a stand-in address
//! (in 100.64.0.0/10) that stands for the name until a connection to it, when
//! the bridge resolves the name itself (Minecraft's SRV records included).

use std::io;
use std::net::SocketAddr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};
use std::time::{Duration, Instant};

#[cfg_attr(not(rjava_stub), link(wasm_import_module = "rjava_web"))]
unsafe extern "C" {
    /// Opens a WebSocket to the bridge for `address` (host:port, UTF-8) on
    /// behalf of the connection whose `Shared` block is at `shared`.
    fn net_open(shared: u32, address: *const u8, length: usize);
    /// The connection at `shared` has data to send, wants to close, or freed
    /// room in its receive ring.
    fn net_kick(shared: u32);
}

/// Connecting; open; closed by the server (after the data received); failed;
/// released by the page (the block may be freed).
const CONNECTING: i32 = 0;
const OPEN: i32 = 1;
const CLOSED: i32 = 2;
const FAILED: i32 = 3;
const RELEASED: i32 = 4;

const RING_BYTES: usize = 256 << 10;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

/// Shared with the page (net.ts reads this layout): ring positions are byte
/// counts (wrapping u32), the rings' sizes powers of two.
#[repr(C)]
struct Shared {
    state: AtomicI32,
    /// Receive ring: written by the page, read here.
    rx_head: AtomicU32,
    rx_tail: AtomicU32,
    /// Send ring: written here, read by the page.
    tx_head: AtomicU32,
    tx_tail: AtomicU32,
    /// Set here: close the WebSocket.
    closing: AtomicI32,
    /// Set by the page when the receive ring was full (kick once there is room).
    rx_waiting: AtomicI32,
    rx: u32,
    rx_size: u32,
    tx: u32,
    tx_size: u32,
}

pub struct Connection {
    shared: &'static Shared,
    remote: SocketAddr,
}

/// Connections closed here that the page has not released yet.
static CLOSED_BLOCKS: Mutex<Vec<usize>> = Mutex::new(Vec::new());

/// Host names behind the stand-in addresses, by index.
static NAMES: Mutex<Vec<String>> = Mutex::new(Vec::new());
const STAND_IN_BASE: u32 = 0x6440_0000; // 100.64.0.0
const STAND_IN_MASK: u32 = 0xFFC0_0000; // /10

/// The stand-in IPv4 address for a host name.
pub fn stand_in(name: &str) -> u32 {
    let mut names = NAMES.lock().unwrap();
    let index = match names.iter().position(|n| n.eq_ignore_ascii_case(name)) {
        Some(index) => index,
        None => {
            names.push(name.to_ascii_lowercase());
            names.len() - 1
        }
    };
    STAND_IN_BASE + 1 + index as u32
}

/// The name a stand-in address stands for.
pub fn name_of(address: u32) -> Option<String> {
    if address & STAND_IN_MASK != STAND_IN_BASE {
        return None;
    }
    NAMES.lock().unwrap().get((address - STAND_IN_BASE).checked_sub(1)? as usize).cloned()
}

fn leak_ring() -> u32 {
    Box::leak(vec![0u8; RING_BYTES].into_boxed_slice()).as_mut_ptr() as usize as u32
}

/// Frees the blocks of closed connections the page has released.
fn reap() {
    let mut closed = CLOSED_BLOCKS.lock().unwrap();
    closed.retain(|&block| {
        // SAFETY: a block leaked by `connect`, not freed yet.
        let shared = unsafe { &*(block as *const Shared) };
        if shared.state.load(Ordering::Acquire) != RELEASED {
            return true;
        }
        // SAFETY: the page no longer touches it; allocated as below.
        unsafe {
            drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(shared.rx as usize as *mut u8, RING_BYTES)));
            drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(shared.tx as usize as *mut u8, RING_BYTES)));
            drop(Box::from_raw(block as *mut Shared));
        }
        false
    });
}

impl Connection {
    /// Connects to `address` through the bridge, waiting for the outcome (the
    /// caller waits outside Java code).
    pub fn connect(address: SocketAddr) -> io::Result<Connection> {
        reap();
        let target = match address {
            SocketAddr::V4(v4) => match name_of(u32::from(*v4.ip())) {
                Some(name) => format!("{name}:{}", v4.port()),
                None => format!("{}:{}", v4.ip(), v4.port()),
            },
            SocketAddr::V6(v6) => format!("[{}]:{}", v6.ip(), v6.port()),
        };
        let shared: &'static Shared = Box::leak(Box::new(Shared {
            state: AtomicI32::new(CONNECTING),
            rx_head: AtomicU32::new(0),
            rx_tail: AtomicU32::new(0),
            tx_head: AtomicU32::new(0),
            tx_tail: AtomicU32::new(0),
            closing: AtomicI32::new(0),
            rx_waiting: AtomicI32::new(0),
            rx: leak_ring(),
            rx_size: RING_BYTES as u32,
            tx: leak_ring(),
            tx_size: RING_BYTES as u32,
        }));
        let block = shared as *const Shared as usize as u32;
        // SAFETY: a valid string for the call.
        unsafe { net_open(block, target.as_ptr(), target.len()) };
        let started = Instant::now();
        while shared.state.load(Ordering::Acquire) == CONNECTING && started.elapsed() < CONNECT_TIMEOUT {
            std::thread::sleep(Duration::from_millis(2));
        }
        let connection = Connection { shared, remote: address };
        match shared.state.load(Ordering::Acquire) {
            OPEN => Ok(connection),
            CONNECTING => Err(io::Error::new(io::ErrorKind::TimedOut, "connection timed out")),
            _ => Err(io::Error::new(
                io::ErrorKind::ConnectionRefused,
                "cannot connect (is rustcraft-bridge running on this computer?)",
            )),
        }
    }

    pub fn remote(&self) -> SocketAddr {
        self.remote
    }

    fn received(&self) -> u32 {
        self.shared.rx_head.load(Ordering::Acquire).wrapping_sub(self.shared.rx_tail.load(Ordering::Relaxed))
    }

    pub fn available(&self) -> usize {
        self.received() as usize
    }

    /// Ok(None): nothing received yet; Ok(Some(0)): closed by the server.
    pub fn read(&mut self, buf: &mut [u8]) -> io::Result<Option<usize>> {
        let s = self.shared;
        let available = self.received() as usize;
        if available == 0 {
            return match s.state.load(Ordering::Acquire) {
                OPEN | CONNECTING => Ok(None),
                CLOSED => Ok(Some(0)),
                _ => Err(io::Error::from(io::ErrorKind::ConnectionReset)),
            };
        }
        let n = available.min(buf.len());
        let tail = s.rx_tail.load(Ordering::Relaxed);
        // SAFETY: the ring the page fills; bytes [tail, tail + available) are written.
        let ring = unsafe { std::slice::from_raw_parts(s.rx as usize as *const u8, s.rx_size as usize) };
        for (i, slot) in buf[..n].iter_mut().enumerate() {
            *slot = ring[(tail.wrapping_add(i as u32) & (s.rx_size - 1)) as usize];
        }
        s.rx_tail.store(tail.wrapping_add(n as u32), Ordering::Release);
        if s.rx_waiting.swap(0, Ordering::AcqRel) != 0 {
            // SAFETY: the connection's block.
            unsafe { net_kick(s as *const Shared as usize as u32) };
        }
        Ok(Some(n))
    }

    /// Ok(None): the send ring is full.
    pub fn write(&mut self, data: &[u8]) -> io::Result<Option<usize>> {
        let s = self.shared;
        if s.state.load(Ordering::Acquire) != OPEN {
            return Err(io::Error::from(io::ErrorKind::BrokenPipe));
        }
        let head = s.tx_head.load(Ordering::Relaxed);
        let used = head.wrapping_sub(s.tx_tail.load(Ordering::Acquire));
        let room = (s.tx_size - used) as usize;
        if room == 0 {
            return Ok(None);
        }
        let n = room.min(data.len());
        // SAFETY: the send ring; bytes [head, head + room) are free.
        let ring = unsafe { std::slice::from_raw_parts_mut(s.tx as usize as *mut u8, s.tx_size as usize) };
        for (i, &byte) in data[..n].iter().enumerate() {
            ring[(head.wrapping_add(i as u32) & (s.tx_size - 1)) as usize] = byte;
        }
        s.tx_head.store(head.wrapping_add(n as u32), Ordering::Release);
        // SAFETY: the connection's block.
        unsafe { net_kick(s as *const Shared as usize as u32) };
        Ok(Some(n))
    }

    /// poll(2) readiness.
    pub fn ready(&self, events: i16) -> i16 {
        use crate::net::{POLLERR, POLLHUP, POLLIN, POLLOUT};
        let s = self.shared;
        let state = s.state.load(Ordering::Acquire);
        let mut revents = 0;
        if events & POLLIN != 0 && (self.received() > 0 || state >= CLOSED) {
            revents |= POLLIN;
        }
        if state == CLOSED && self.received() == 0 {
            revents |= POLLHUP;
        }
        if state == FAILED {
            revents |= POLLERR;
        }
        let used = s.tx_head.load(Ordering::Relaxed).wrapping_sub(s.tx_tail.load(Ordering::Acquire));
        if events & POLLOUT != 0 && state == OPEN && used < s.tx_size {
            revents |= POLLOUT;
        }
        revents
    }

    /// Asks the page to close the WebSocket (after sending what is queued).
    pub fn close(&mut self) {
        let s = self.shared;
        if s.closing.swap(1, Ordering::AcqRel) == 0 {
            CLOSED_BLOCKS.lock().unwrap().push(s as *const Shared as usize);
            // SAFETY: the connection's block.
            unsafe { net_kick(s as *const Shared as usize as u32) };
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.close();
    }
}
