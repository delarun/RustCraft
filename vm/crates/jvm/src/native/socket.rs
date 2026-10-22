//! sun.nio.ch socket natives (Net, SocketDispatcher, PollSelectorImpl, pipes)
//! on top of the VM socket table (`net.rs`). IPv4 only.

use super::*;
use crate::heap::ARRAY_BASE_OFFSET;
use crate::net::{Endpoint, POLLERR, POLLHUP, POLLIN, POLLNVAL, POLLOUT};
use std::net::{Ipv4Addr, Shutdown, SocketAddr};

const IOSTATUS_EOF: i32 = -1;
const IOSTATUS_UNAVAILABLE: i32 = -2;

pub(super) fn register(n: &mut Natives) {
    // Extended (Linux-specific) socket options are reported as unsupported.
    n.add("jdk/net/LinuxSocketOptions.keepAliveOptionsSupported0()Z", return_false);
    n.add("jdk/net/LinuxSocketOptions.quickAckSupported0()Z", return_false);
    n.add("jdk/net/LinuxSocketOptions.incomingNapiIdSupported0()Z", return_false);
    n.add("sun/nio/ch/Net.initIDs()V", no_op);
    n.add("sun/nio/ch/Net.isIPv6Available0()Z", return_false);
    n.add("sun/nio/ch/Net.isReusePortAvailable0()Z", return_false);
    n.add("sun/nio/ch/Net.isExclusiveBindAvailable()I", |_, _| ret_i(-1));
    n.add("sun/nio/ch/Net.shouldShutdownWriteBeforeClose0()Z", return_false);
    n.add("sun/nio/ch/Net.shouldSetBothIPv4AndIPv6Options0()Z", return_false);
    n.add("sun/nio/ch/Net.canIPv6SocketJoinIPv4Group0()Z", return_false);
    n.add("sun/nio/ch/Net.canJoin6WithIPv4Group0()Z", return_false);
    n.add("sun/nio/ch/Net.canUseIPv6OptionsWithIPv4LocalAddress0()Z", return_false);
    n.add("sun/nio/ch/Net.pollinValue()S", |_, _| ret_i(POLLIN as i32));
    n.add("sun/nio/ch/Net.polloutValue()S", |_, _| ret_i(POLLOUT as i32));
    n.add("sun/nio/ch/Net.pollerrValue()S", |_, _| ret_i(POLLERR as i32));
    n.add("sun/nio/ch/Net.pollhupValue()S", |_, _| ret_i(POLLHUP as i32));
    n.add("sun/nio/ch/Net.pollnvalValue()S", |_, _| ret_i(POLLNVAL as i32));
    n.add("sun/nio/ch/Net.pollconnValue()S", |_, _| ret_i(POLLOUT as i32));

    n.add("sun/nio/ch/Net.socket0(ZZZZ)I", |t, _| {
        let fd = t.vm.fds.allocate();
        t.vm.sockets.insert(fd, Endpoint::Unbound { bind: None });
        ret_i(fd)
    });
    n.add("sun/nio/ch/Net.bind0(Ljava/io/FileDescriptor;ZZLjava/net/InetAddress;I)V", |t, a| {
        let address = socket_address(t, arg_ref(a, 3), arg_i(a, 4))?;
        with_socket(t, arg_ref(a, 0), |s| match s {
            Endpoint::Unbound { bind } => {
                *bind = Some(address);
                Ok(())
            }
            _ => Err(std::io::Error::from(std::io::ErrorKind::InvalidInput)),
        })?;
        ret_void()
    });
    n.add("sun/nio/ch/Net.listen(Ljava/io/FileDescriptor;I)V", |t, a| {
        let result = socket(t, arg_ref(a, 0))?.lock().unwrap().listen();
        result.map_err(|e| exception(t, "java/net/BindException", &e))?;
        ret_void()
    });
    n.add("sun/nio/ch/Net.connect0(ZLjava/io/FileDescriptor;Ljava/net/InetAddress;I)I", |t, a| {
        let address = socket_address(t, arg_ref(a, 2), arg_i(a, 3))?;
        let socket = socket(t, arg_ref(a, 1))?;
        let result = t.blocking(|| socket.lock().unwrap().connect(address));
        result.map_err(|e| exception(t, "java/net/ConnectException", &e))?;
        ret_i(1)
    });
    n.add("sun/nio/ch/Net.accept(Ljava/io/FileDescriptor;Ljava/io/FileDescriptor;[Ljava/net/InetSocketAddress;)I", |t, a| {
        let accepted = socket(t, arg_ref(a, 0))?.lock().unwrap().accept();
        let Some((stream, remote)) = accepted.map_err(|e| exception(t, "java/io/IOException", &e))? else {
            return ret_i(IOSTATUS_UNAVAILABLE);
        };
        let _ = stream.set_nonblocking(true);
        let _ = stream.set_nodelay(true);
        let fd = t.vm.fds.allocate();
        t.vm.sockets.insert(fd, Endpoint::Stream(stream));
        t.set_field(arg_ref(a, 1), "java/io/FileDescriptor", "fd", fd as u32 as u64);
        let remote = inet_socket_address(t, remote)?;
        let holder = t.null_check(arg_ref(a, 2))?;
        t.vm.heap.write(holder, ARRAY_BASE_OFFSET, remote.0);
        ret_i(1)
    });
    n.add("sun/nio/ch/Net.shutdown(Ljava/io/FileDescriptor;I)V", |t, a| {
        let how = match arg_i(a, 1) {
            0 => Shutdown::Read,
            1 => Shutdown::Write,
            _ => Shutdown::Both,
        };
        let _ = socket(t, arg_ref(a, 0))?.lock().unwrap().shutdown(how);
        ret_void()
    });
    n.add("sun/nio/ch/Net.localPort(Ljava/io/FileDescriptor;)I", |t, a| {
        let address = socket(t, arg_ref(a, 0))?.lock().unwrap().local_address();
        ret_i(address.map_or(0, |a| a.port() as i32))
    });
    n.add("sun/nio/ch/Net.localInetAddress(Ljava/io/FileDescriptor;)Ljava/net/InetAddress;", |t, a| {
        let address = socket(t, arg_ref(a, 0))?.lock().unwrap().local_address();
        ret_ref(inet_address(t, address.unwrap_or_else(|| SocketAddr::from(([0, 0, 0, 0], 0))))?)
    });
    n.add("sun/nio/ch/Net.remotePort(Ljava/io/FileDescriptor;)I", |t, a| {
        let address = socket(t, arg_ref(a, 0))?.lock().unwrap().remote_address();
        ret_i(address.map_or(0, |a| a.port() as i32))
    });
    n.add("sun/nio/ch/Net.remoteInetAddress(Ljava/io/FileDescriptor;)Ljava/net/InetAddress;", |t, a| {
        let address = socket(t, arg_ref(a, 0))?.lock().unwrap().remote_address();
        match address {
            Some(address) => ret_ref(inet_address(t, address)?),
            None => ret_ref(Ref::NULL),
        }
    });
    n.add("sun/nio/ch/Net.getIntOption0(Ljava/io/FileDescriptor;ZII)I", return_zero_int);
    n.add("sun/nio/ch/Net.setIntOption0(Ljava/io/FileDescriptor;ZIIIZ)V", no_op);
    n.add("sun/nio/ch/Net.poll(Ljava/io/FileDescriptor;IJ)I", |t, a| {
        let fd = descriptor(t, arg_ref(a, 0));
        let events = arg_i(a, 1) as i16;
        let timeout = arg_j(a, 2);
        let mut revents = 0;
        wait_until(t, timeout, |t| {
            revents = readiness(t, fd, events);
            revents != 0
        });
        ret_i(revents as i32)
    });
    n.add("sun/nio/ch/Net.pollConnect(Ljava/io/FileDescriptor;J)Z", |_, _| ret_z(true));
    n.add("sun/nio/ch/Net.available(Ljava/io/FileDescriptor;)I", |t, a| {
        let available = socket(t, arg_ref(a, 0))?.lock().unwrap().available();
        ret_i(available as i32)
    });

    n.add("sun/nio/ch/SocketDispatcher.read0(Ljava/io/FileDescriptor;JI)I", |t, a| {
        let fd = descriptor(t, arg_ref(a, 0));
        ret_i(read(t, fd, arg_j(a, 1), arg_i(a, 3))?)
    });
    n.add("sun/nio/ch/SocketDispatcher.write0(Ljava/io/FileDescriptor;JI)I", |t, a| {
        let fd = descriptor(t, arg_ref(a, 0));
        ret_i(write(t, fd, arg_j(a, 1), arg_i(a, 3))?)
    });
    n.add("sun/nio/ch/SocketDispatcher.readv0(Ljava/io/FileDescriptor;JI)J", |t, a| {
        let fd = descriptor(t, arg_ref(a, 0));
        let mut total = 0i64;
        for (base, len) in super::nio::iovecs(arg_j(a, 1), arg_i(a, 3)) {
            let n = read(t, fd, base, len as i32)?;
            if n < 0 {
                return ret_j(if total == 0 { n as i64 } else { total });
            }
            total += n as i64;
            if (n as usize) < len {
                break;
            }
        }
        ret_j(total)
    });
    n.add("sun/nio/ch/SocketDispatcher.writev0(Ljava/io/FileDescriptor;JI)J", |t, a| {
        let fd = descriptor(t, arg_ref(a, 0));
        let mut total = 0i64;
        for (base, len) in super::nio::iovecs(arg_j(a, 1), arg_i(a, 3)) {
            let n = write(t, fd, base, len as i32)?;
            if n < 0 {
                return ret_j(if total == 0 { n as i64 } else { total });
            }
            total += n as i64;
            if (n as usize) < len {
                break;
            }
        }
        ret_j(total)
    });

    // struct pollfd { int fd; short events; short revents; }
    n.add("sun/nio/ch/PollSelectorImpl.poll(JII)I", |t, a| {
        let (address, count, timeout) = (arg_j(a, 0) as usize, arg_i(a, 2).max(0) as usize, arg_i(a, 3) as i64);
        let mut ready = 0;
        wait_until(t, timeout, |t| {
            ready = 0;
            for i in 0..count {
                // SAFETY: the selector's pollfd array in native memory.
                let entry = unsafe { &mut *((address + i * 8) as *mut [i16; 4]) };
                let fd = entry[0] as u16 as i32 | (entry[1] as u16 as i32) << 16;
                let revents = readiness(t, fd, entry[2]);
                entry[3] = revents;
                if revents != 0 {
                    ready += 1;
                }
            }
            ready > 0
        });
        ret_i(ready)
    });

    n.add("sun/nio/ch/IOUtil.makePipe(Z)J", |t, _| {
        let (read_end, write_end) = crate::net::pipe();
        let (read_fd, write_fd) = (t.vm.fds.allocate(), t.vm.fds.allocate());
        t.vm.sockets.insert(read_fd, read_end);
        t.vm.sockets.insert(write_fd, write_end);
        ret_j(((read_fd as i64) << 32) | write_fd as i64)
    });
    n.add("sun/nio/ch/IOUtil.write1(IB)I", |t, a| {
        let Some(pipe) = t.vm.sockets.get(arg_i(a, 0)) else { return ret_i(IOSTATUS_EOF) };
        let written = pipe.lock().unwrap().write(&[a[1] as u8]);
        ret_i(written.ok().flatten().map_or(IOSTATUS_UNAVAILABLE, |n| n as i32))
    });
    n.add("sun/nio/ch/IOUtil.drain(I)Z", |t, a| {
        let Some(pipe) = t.vm.sockets.get(arg_i(a, 0)) else { return ret_z(false) };
        let mut buf = [0u8; 128];
        let mut any = false;
        while let Ok(Some(n)) = pipe.lock().unwrap().read(&mut buf) {
            any = true;
            if n < buf.len() {
                break;
            }
        }
        ret_z(any)
    });
    n.add("sun/nio/ch/IOUtil.drain1(I)I", |t, a| {
        let Some(pipe) = t.vm.sockets.get(arg_i(a, 0)) else { return ret_i(IOSTATUS_EOF) };
        let mut byte = [0u8];
        let read = pipe.lock().unwrap().read(&mut byte);
        ret_i(match read {
            Ok(Some(1)) => 1,
            _ => IOSTATUS_UNAVAILABLE,
        })
    });
}

fn descriptor(t: &Thread<'_>, fd: Ref) -> i32 {
    t.field(fd, "java/io/FileDescriptor", "fd") as i32
}

fn socket(t: &mut Thread<'_>, fd: Ref) -> JResult<crate::net::Socket> {
    let fd = t.null_check(fd)?;
    let fd = descriptor(t, fd);
    t.vm.sockets.get(fd).ok_or_else(|| t.throw_new("java/net/SocketException", Some("Bad file descriptor")))
}

fn with_socket<R>(t: &mut Thread<'_>, fd: Ref, f: impl FnOnce(&mut Endpoint) -> std::io::Result<R>) -> JResult<R> {
    let socket = socket(t, fd)?;
    let result = f(&mut socket.lock().unwrap());
    result.map_err(|e| exception(t, "java/net/SocketException", &e))
}

fn exception(t: &mut Thread<'_>, class: &str, error: &std::io::Error) -> crate::interp::Throw {
    use std::io::ErrorKind::*;
    let message = match error.kind() {
        AddrInUse => "Address already in use".to_owned(),
        ConnectionRefused => "Connection refused".to_owned(),
        ConnectionReset => "Connection reset".to_owned(),
        BrokenPipe => "Broken pipe".to_owned(),
        _ => error.to_string(),
    };
    t.throw_new(class, Some(&message))
}

fn socket_address(t: &mut Thread<'_>, address: Ref, port: i32) -> JResult<SocketAddr> {
    let address = t.null_check(address)?;
    let holder = t.field_ref(address, "java/net/InetAddress", "holder");
    let ip = t.field(holder, "java/net/InetAddress$InetAddressHolder", "address") as u32;
    Ok(SocketAddr::from((Ipv4Addr::from(ip), port as u16)))
}

fn inet_address(t: &mut Thread<'_>, address: SocketAddr) -> JResult<Ref> {
    let ip = match address.ip() {
        std::net::IpAddr::V4(ip) => u32::from(ip),
        std::net::IpAddr::V6(_) => 0,
    };
    super::net::inet4(t, &address.ip().to_string(), ip)
}

fn inet_socket_address(t: &mut Thread<'_>, address: SocketAddr) -> JResult<Ref> {
    let inet = inet_address(t, address)?;
    let class = t.load_class("java/net/InetSocketAddress")?;
    t.ensure_initialized(class)?;
    let obj = t.vm.new_object(class).map_err(|e| t.throw_vm_error(e))?;
    let init = class.find_method("<init>", "(Ljava/net/InetAddress;I)V").expect("InetSocketAddress(InetAddress, int)");
    t.invoke(init, &[obj.slot(), inet.slot(), address.port() as u64])?;
    Ok(obj)
}

fn readiness(t: &Thread<'_>, fd: i32, events: i16) -> i16 {
    match t.vm.sockets.get(fd) {
        Some(socket) => socket.lock().unwrap().ready(events),
        // Regular files are always ready.
        None if t.vm.fds.get(fd).is_some() => events & (POLLIN | POLLOUT),
        None => POLLNVAL,
    }
}

/// Re-checks `ready` until it succeeds, the timeout (ms; negative = forever,
/// 0 = once) expires or the thread is interrupted. Other threads run while
/// this one waits.
fn wait_until(t: &mut Thread<'_>, timeout: i64, mut ready: impl FnMut(&mut Thread<'_>) -> bool) {
    let deadline = (timeout > 0).then(|| std::time::Instant::now() + std::time::Duration::from_millis(timeout as u64));
    loop {
        if ready(t) || timeout == 0 || t.is_interrupted() {
            return;
        }
        if deadline.is_some_and(|d| std::time::Instant::now() >= d) {
            return;
        }
        t.blocking(|| std::thread::sleep(std::time::Duration::from_millis(1)));
    }
}

fn read(t: &mut Thread<'_>, fd: i32, address: i64, len: i32) -> JResult<i32> {
    let Some(socket) = t.vm.sockets.get(fd) else { return Err(t.throw_new("java/net/SocketException", Some("Bad file descriptor"))) };
    if len <= 0 {
        return Ok(0);
    }
    // SAFETY: a native buffer of `len` bytes.
    let buf = unsafe { std::slice::from_raw_parts_mut(address as usize as *mut u8, len as usize) };
    let result = socket.lock().unwrap().read(buf);
    match result {
        Ok(Some(0)) => Ok(IOSTATUS_EOF),
        Ok(Some(n)) => Ok(n as i32),
        Ok(None) => Ok(IOSTATUS_UNAVAILABLE),
        Err(e) => Err(exception(t, "java/net/SocketException", &e)),
    }
}

fn write(t: &mut Thread<'_>, fd: i32, address: i64, len: i32) -> JResult<i32> {
    let Some(socket) = t.vm.sockets.get(fd) else { return Err(t.throw_new("java/net/SocketException", Some("Bad file descriptor"))) };
    if len <= 0 {
        return Ok(0);
    }
    // SAFETY: a native buffer of `len` bytes.
    let data = unsafe { std::slice::from_raw_parts(address as usize as *const u8, len as usize) };
    let result = socket.lock().unwrap().write(data);
    match result {
        Ok(Some(n)) => Ok(n as i32),
        Ok(None) => Ok(IOSTATUS_UNAVAILABLE),
        Err(e) => Err(exception(t, "java/io/IOException", &e)),
    }
}
