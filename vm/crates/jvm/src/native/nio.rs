//! java.nio natives for Linux (sun.nio.fs.UnixNativeDispatcher, sun.nio.ch file
//! channels) on top of the VM file system. Paths arrive as NUL-terminated
//! strings in native memory; failures are thrown as `UnixException(errno)`.
//! Symbolic links, ownership and permissions are not modelled.

use super::*;
use crate::fs::{FileKind, Metadata, OpenOptions};
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

// UnixConstants (Linux).
const O_ACCMODE: i32 = 3;
const O_WRONLY: i32 = 1;
const O_RDWR: i32 = 2;
const O_APPEND: i32 = 0o2000;
const O_CREAT: i32 = 0o100;
const O_EXCL: i32 = 0o200;
const O_TRUNC: i32 = 0o1000;
const S_IFREG: i32 = 0o100000;
const S_IFDIR: i32 = 0o040000;
const W_OK: i32 = 2;

const ENOENT: i32 = 2;
const EIO: i32 = 5;
const EBADF: i32 = 9;
const EACCES: i32 = 13;
const EEXIST: i32 = 17;
const EXDEV: i32 = 18;
const ENOTDIR: i32 = 20;
const EISDIR: i32 = 21;
const EINVAL: i32 = 22;
const ENOTEMPTY: i32 = 39;
const ENOTSUP: i32 = 95;

// IOStatus / FileDispatcher.
const IOSTATUS_EOF: i32 = -1;
const IOSTATUS_UNSUPPORTED_CASE: i64 = -6;
const LOCKED: i32 = 0;

const MAP_RW: i32 = 1;

pub(super) fn register(n: &mut Natives) {
    n.add("sun/nio/fs/UnixNativeDispatcher.init()I", return_zero_int); // no openat, xattr or birthtime
    n.add("sun/nio/fs/UnixNativeDispatcher.getcwd()[B", |t, _| {
        let dir = t.vm.config.user_dir.clone();
        ret_ref(super::lang::new_byte_array(t, dir.as_bytes())?)
    });
    n.add("sun/nio/fs/UnixNativeDispatcher.strerror(I)[B", |t, a| {
        let message = strerror(arg_i(a, 0));
        ret_ref(super::lang::new_byte_array(t, message.as_bytes())?)
    });
    n.add("sun/nio/fs/UnixNativeDispatcher.dup(I)I", |t, a| {
        let fd = arg_i(a, 0);
        match t.vm.fds.get(fd) {
            Some(file) => ret_i(t.vm.fds.insert_shared(file)),
            None => Err(unix_exception(t, EBADF)),
        }
    });
    n.add("sun/nio/fs/UnixNativeDispatcher.open0(JII)I", |t, a| {
        let path = c_path(t, arg_j(a, 0));
        let flags = arg_i(a, 2);
        let options = OpenOptions {
            read: flags & O_ACCMODE != O_WRONLY,
            write: flags & O_ACCMODE != 0,
            append: flags & O_APPEND != 0,
            create: flags & O_CREAT != 0,
            create_new: flags & O_CREAT != 0 && flags & O_EXCL != 0,
            truncate: flags & O_TRUNC != 0,
        };
        let options = OpenOptions { read: options.read || flags & O_ACCMODE == O_RDWR, ..options };
        match t.vm.vfs.open(&path, &options) {
            Ok(file) => ret_i(t.vm.fds.insert(file)),
            Err(e) => Err(unix_exception(t, errno(&e))),
        }
    });
    n.add("sun/nio/fs/UnixNativeDispatcher.close0(I)V", |t, a| {
        t.vm.fds.close(arg_i(a, 0));
        ret_void()
    });
    n.add("sun/nio/fs/UnixNativeDispatcher.unlink0(J)V", |t, a| {
        let path = c_path(t, arg_j(a, 0));
        match t.vm.vfs.metadata(&path) {
            Ok(meta) if meta.kind == FileKind::Dir => Err(unix_exception(t, EISDIR)),
            Ok(_) => t.vm.vfs.remove(&path).map_or_else(|e| Err(unix_exception(t, errno(&e))), |_| ret_void()),
            Err(e) => Err(unix_exception(t, errno(&e))),
        }
    });
    n.add("sun/nio/fs/UnixNativeDispatcher.rmdir0(J)V", |t, a| {
        let path = c_path(t, arg_j(a, 0));
        match t.vm.vfs.metadata(&path) {
            Ok(meta) if meta.kind != FileKind::Dir => Err(unix_exception(t, ENOTDIR)),
            Ok(_) => t.vm.vfs.remove(&path).map_or_else(|e| Err(unix_exception(t, errno(&e))), |_| ret_void()),
            Err(e) => Err(unix_exception(t, errno(&e))),
        }
    });
    n.add("sun/nio/fs/UnixNativeDispatcher.mkdir0(JI)V", |t, a| {
        let path = c_path(t, arg_j(a, 0));
        t.vm.vfs.create_dir(&path).map_or_else(|e| Err(unix_exception(t, errno(&e))), |_| ret_void())
    });
    n.add("sun/nio/fs/UnixNativeDispatcher.rename0(JJ)V", |t, a| {
        let from = c_path(t, arg_j(a, 0));
        let to = c_path(t, arg_j(a, 2));
        // POSIX rename replaces an existing target file.
        if let (Ok(source), Ok(target)) = (t.vm.vfs.metadata(&from), t.vm.vfs.metadata(&to)) {
            if source.kind == FileKind::File && target.kind == FileKind::File {
                let _ = t.vm.vfs.remove(&to);
            }
        }
        t.vm.vfs.rename(&from, &to).map_or_else(|e| Err(unix_exception(t, errno(&e))), |_| ret_void())
    });
    n.add("sun/nio/fs/UnixNativeDispatcher.link0(JJ)V", |t, _| Err(unix_exception(t, ENOTSUP)));
    n.add("sun/nio/fs/UnixNativeDispatcher.symlink0(JJ)V", |t, _| Err(unix_exception(t, ENOTSUP)));
    n.add("sun/nio/fs/UnixNativeDispatcher.readlink0(J)[B", |t, _| Err(unix_exception(t, EINVAL)));
    n.add("sun/nio/fs/UnixNativeDispatcher.realpath0(J)[B", |t, a| {
        let path = crate::fs::normalize(&c_path(t, arg_j(a, 0)));
        match t.vm.vfs.metadata(&path) {
            Ok(_) => ret_ref(super::lang::new_byte_array(t, path.as_bytes())?),
            Err(e) => Err(unix_exception(t, errno(&e))),
        }
    });
    n.add("sun/nio/fs/UnixNativeDispatcher.stat0(JLsun/nio/fs/UnixFileAttributes;)I", |t, a| {
        let path = c_path(t, arg_j(a, 0));
        match t.vm.vfs.metadata(&path) {
            Ok(meta) => {
                fill_attributes(t, arg_ref(a, 2), &path, &meta);
                ret_i(0)
            }
            Err(e) => ret_i(errno(&e)),
        }
    });
    n.add("sun/nio/fs/UnixNativeDispatcher.lstat0(JLsun/nio/fs/UnixFileAttributes;)V", |t, a| {
        let path = c_path(t, arg_j(a, 0));
        match t.vm.vfs.metadata(&path) {
            Ok(meta) => {
                fill_attributes(t, arg_ref(a, 2), &path, &meta);
                ret_void()
            }
            Err(e) => Err(unix_exception(t, errno(&e))),
        }
    });
    n.add("sun/nio/fs/UnixNativeDispatcher.fstat0(ILsun/nio/fs/UnixFileAttributes;)V", |t, a| {
        let fd = arg_i(a, 0);
        let Some(file) = t.vm.fds.get(fd) else { return Err(unix_exception(t, EBADF)) };
        let len = file.lock().unwrap().len().unwrap_or(0);
        let meta = Metadata { kind: FileKind::File, len, modified: 0, readonly: false };
        fill_attributes(t, arg_ref(a, 1), &format!("fd:{fd}"), &meta);
        ret_void()
    });
    n.add("sun/nio/fs/UnixNativeDispatcher.access0(JI)I", |t, a| {
        let path = c_path(t, arg_j(a, 0));
        let mode = arg_i(a, 2);
        ret_i(match t.vm.vfs.metadata(&path) {
            Ok(meta) if mode & W_OK != 0 && meta.readonly => EACCES,
            Ok(_) => 0,
            Err(e) => errno(&e),
        })
    });
    n.add("sun/nio/fs/UnixNativeDispatcher.chmod0(JI)V", |t, a| {
        let path = c_path(t, arg_j(a, 0));
        t.vm.vfs.metadata(&path).map_or_else(|e| Err(unix_exception(t, errno(&e))), |_| ret_void())
    });
    n.add("sun/nio/fs/UnixNativeDispatcher.fchmod0(II)V", no_op);
    n.add("sun/nio/fs/UnixNativeDispatcher.chown0(JII)V", no_op);
    n.add("sun/nio/fs/UnixNativeDispatcher.lchown0(JII)V", no_op);
    n.add("sun/nio/fs/UnixNativeDispatcher.fchown0(III)V", no_op);
    n.add("sun/nio/fs/UnixNativeDispatcher.fchmodatNoFollowSupported0()Z", return_false);
    n.add("sun/nio/fs/UnixNativeDispatcher.utimensat0(IJJJI)V", |t, a| {
        // (dfd, path, atime, mtime, flags); times in nanoseconds.
        let path = c_path(t, arg_j(a, 1));
        let millis = arg_j(a, 5) / 1_000_000;
        t.vm.vfs.set_modified(&path, millis).map_or_else(|e| Err(unix_exception(t, errno(&e))), |_| ret_void())
    });
    n.add("sun/nio/fs/UnixNativeDispatcher.futimens0(IJJ)V", no_op);
    n.add("sun/nio/fs/UnixNativeDispatcher.opendir0(J)J", |t, a| {
        let path = c_path(t, arg_j(a, 0));
        match t.vm.vfs.read_dir(&path) {
            Ok(mut entries) => {
                entries.sort();
                ret_j(DIRS.open(entries))
            }
            Err(e) => Err(unix_exception(t, errno(&e))),
        }
    });
    n.add("sun/nio/fs/UnixNativeDispatcher.readdir0(J)[B", |t, a| match DIRS.next(arg_j(a, 0)) {
        Some(name) => ret_ref(super::lang::new_byte_array(t, name.as_bytes())?),
        None => ret_ref(Ref::NULL),
    });
    n.add("sun/nio/fs/UnixNativeDispatcher.closedir(J)V", |_, a| {
        DIRS.close(arg_j(a, 0));
        ret_void()
    });
    n.add("sun/nio/fs/UnixNativeDispatcher.read0(IJI)I", |t, a| {
        let n = fd_read(t, arg_i(a, 0), arg_j(a, 1), arg_i(a, 3), None)?;
        ret_i(n.max(0))
    });
    n.add("sun/nio/fs/UnixNativeDispatcher.write0(IJI)I", |t, a| ret_i(fd_write(t, arg_i(a, 0), arg_j(a, 1), arg_i(a, 3), None)?));
    n.add("sun/nio/fs/UnixNativeDispatcher.getpwuid(I)[B", |t, _| {
        let name = t.vm.config.user_name.clone();
        ret_ref(super::lang::new_byte_array(t, name.as_bytes())?)
    });
    n.add("sun/nio/fs/UnixNativeDispatcher.getgrgid(I)[B", |t, _| {
        let name = t.vm.config.user_name.clone();
        ret_ref(super::lang::new_byte_array(t, name.as_bytes())?)
    });
    n.add("sun/nio/fs/UnixNativeDispatcher.getpwnam0(J)I", |_, _| ret_i(1000));
    n.add("sun/nio/fs/UnixNativeDispatcher.getgrnam0(J)I", |_, _| ret_i(1000));
    n.add("sun/nio/fs/UnixNativeDispatcher.statvfs0(JLsun/nio/fs/UnixFileStoreAttributes;)V", |t, a| {
        let attrs = arg_ref(a, 2);
        let class = "sun/nio/fs/UnixFileStoreAttributes";
        t.set_field(attrs, class, "f_frsize", 4096);
        t.set_field(attrs, class, "f_blocks", 1 << 24);
        t.set_field(attrs, class, "f_bfree", 1 << 23);
        t.set_field(attrs, class, "f_bavail", 1 << 23);
        ret_void()
    });

    n.add("sun/nio/fs/LinuxNativeDispatcher.init()V", no_op);
    n.add("sun/nio/fs/LinuxNativeDispatcher.posix_fadvise(IJJI)I", return_zero_int);
    // The mount table has a single entry: the VM file system at "/" (st_dev 1).
    n.add("sun/nio/fs/LinuxNativeDispatcher.setmntent0(JJ)J", |_, _| {
        ret_j(MOUNT_TABLES.fetch_add(1, std::sync::atomic::Ordering::Relaxed) as i64 * 2 + 2)
    });
    n.add("sun/nio/fs/LinuxNativeDispatcher.getmntent0(JLsun/nio/fs/UnixMountEntry;JI)I", |t, a| {
        // Even handles have not returned their entry yet; reading makes them odd.
        let handle = arg_j(a, 0);
        let mut read = MOUNTS_READ.lock().unwrap();
        if !read.insert(handle) {
            return ret_i(-1);
        }
        drop(read);
        let entry = arg_ref(a, 2);
        let class = "sun/nio/fs/UnixMountEntry";
        for (field, value) in [("name", "vmfs"), ("dir", "/"), ("fstype", "vmfs"), ("opts", "rw")] {
            let bytes = super::lang::new_byte_array(t, value.as_bytes())?;
            t.set_field(entry, class, field, bytes.slot());
        }
        t.set_field(entry, class, "dev", 1);
        ret_i(0)
    });
    // Line lengths of the mount table are only used to size a buffer.
    n.add("sun/nio/fs/UnixNativeDispatcher.getlinelen(J)I", |_, _| ret_i(-1));
    n.add("sun/nio/fs/UnixNativeDispatcher.rewind(J)V", |_, a| {
        MOUNTS_READ.lock().unwrap().remove(&arg_j(a, 0));
        ret_void()
    });
    n.add("sun/nio/fs/LinuxNativeDispatcher.endmntent(J)V", |_, a| {
        MOUNTS_READ.lock().unwrap().remove(&arg_j(a, 0));
        ret_void()
    });
    n.add("sun/nio/fs/LinuxNativeDispatcher.directCopy0(IIJ)I", |t, a| {
        let (dst, src) = (arg_i(a, 0), arg_i(a, 1));
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = with_fd(t, src, |f| f.read(&mut buf))?;
            if n == 0 {
                return ret_i(0);
            }
            with_fd(t, dst, |f| write_all(f, &buf[..n]))?;
        }
    });

    n.add("sun/nio/ch/IOUtil.initIDs()V", no_op);
    n.add("sun/nio/ch/IOUtil.fdVal(Ljava/io/FileDescriptor;)I", |t, a| ret_i(t.field(arg_ref(a, 0), "java/io/FileDescriptor", "fd") as i32));
    n.add("sun/nio/ch/IOUtil.setfdVal(Ljava/io/FileDescriptor;I)V", |t, a| {
        t.set_field(arg_ref(a, 0), "java/io/FileDescriptor", "fd", a[1] as u32 as u64);
        ret_void()
    });
    n.add("sun/nio/ch/IOUtil.configureBlocking(Ljava/io/FileDescriptor;Z)V", no_op);
    n.add("sun/nio/ch/IOUtil.fdLimit()I", |_, _| ret_i(65536));
    n.add("sun/nio/ch/IOUtil.iovMax()I", |_, _| ret_i(1024));
    n.add("sun/nio/ch/IOUtil.writevMax()J", |_, _| ret_j(i32::MAX as i64));
    n.add("sun/nio/ch/IOUtil.randomBytes([B)Z", return_false);

    n.add("sun/nio/ch/NativeThread.init()V", no_op);
    n.add("sun/nio/ch/NativeThread.current0()J", |t, _| ret_j(t.id() as i64));
    n.add("sun/nio/ch/NativeThread.signal0(J)V", no_op);

    n.add("sun/nio/ch/UnixDispatcher.init()V", no_op);
    n.add("sun/nio/ch/UnixDispatcher.close0(Ljava/io/FileDescriptor;)V", |t, a| {
        let fd = t.field(arg_ref(a, 0), "java/io/FileDescriptor", "fd") as i32;
        if fd > 2 && !t.vm.sockets.close(fd) {
            t.vm.fds.close(fd);
        }
        ret_void()
    });
    n.add("sun/nio/ch/UnixDispatcher.preClose0(Ljava/io/FileDescriptor;)V", no_op);

    // FileKey identifies the open file for lock tables: (st_dev, st_ino).
    n.add("sun/nio/ch/FileKey.initIDs()V", no_op);
    n.add("sun/nio/ch/FileKey.init(Ljava/io/FileDescriptor;[J)V", |t, a| {
        let fd = descriptor_fd(t, arg_ref(a, 0));
        let Some(file) = t.vm.fds.get(fd) else { return Err(t.throw_new("java/io/IOException", Some("Bad file descriptor"))) };
        let info = t.null_check(arg_ref(a, 1))?;
        t.vm.heap.write::<i64>(info, crate::heap::ARRAY_BASE_OFFSET, 1);
        t.vm.heap.write::<i64>(info, crate::heap::ARRAY_BASE_OFFSET + 8, std::sync::Arc::as_ptr(&file) as *const () as usize as i64);
        ret_void()
    });
    n.add("sun/nio/ch/FileDispatcherImpl.init0()V", no_op);
    n.add("sun/nio/ch/FileDispatcherImpl.transferTo0(Ljava/io/FileDescriptor;JJLjava/io/FileDescriptor;Z)J", |_, _| {
        ret_j(IOSTATUS_UNSUPPORTED_CASE)
    });
    n.add("sun/nio/ch/FileDispatcherImpl.transferFrom0(Ljava/io/FileDescriptor;Ljava/io/FileDescriptor;JJZ)J", |_, _| {
        ret_j(IOSTATUS_UNSUPPORTED_CASE)
    });

    n.add("sun/nio/ch/UnixFileDispatcherImpl.read0(Ljava/io/FileDescriptor;JI)I", |t, a| {
        let fd = descriptor_fd(t, arg_ref(a, 0));
        ret_i(fd_read(t, fd, arg_j(a, 1), arg_i(a, 3), None)?)
    });
    n.add("sun/nio/ch/UnixFileDispatcherImpl.pread0(Ljava/io/FileDescriptor;JIJ)I", |t, a| {
        let fd = descriptor_fd(t, arg_ref(a, 0));
        ret_i(fd_read(t, fd, arg_j(a, 1), arg_i(a, 3), Some(arg_j(a, 4) as u64))?)
    });
    n.add("sun/nio/ch/UnixFileDispatcherImpl.write0(Ljava/io/FileDescriptor;JI)I", |t, a| {
        let fd = descriptor_fd(t, arg_ref(a, 0));
        ret_i(fd_write(t, fd, arg_j(a, 1), arg_i(a, 3), None)?)
    });
    n.add("sun/nio/ch/UnixFileDispatcherImpl.pwrite0(Ljava/io/FileDescriptor;JIJ)I", |t, a| {
        let fd = descriptor_fd(t, arg_ref(a, 0));
        ret_i(fd_write(t, fd, arg_j(a, 1), arg_i(a, 3), Some(arg_j(a, 4) as u64))?)
    });
    n.add("sun/nio/ch/UnixFileDispatcherImpl.readv0(Ljava/io/FileDescriptor;JI)J", |t, a| {
        let fd = descriptor_fd(t, arg_ref(a, 0));
        let mut total = 0i64;
        for (base, len) in iovecs(arg_j(a, 1), arg_i(a, 3)) {
            let n = fd_read(t, fd, base, len as i32, None)?;
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
    n.add("sun/nio/ch/UnixFileDispatcherImpl.writev0(Ljava/io/FileDescriptor;JI)J", |t, a| {
        let fd = descriptor_fd(t, arg_ref(a, 0));
        let mut total = 0i64;
        for (base, len) in iovecs(arg_j(a, 1), arg_i(a, 3)) {
            total += fd_write(t, fd, base, len as i32, None)? as i64;
        }
        ret_j(total)
    });
    n.add("sun/nio/ch/UnixFileDispatcherImpl.force0(Ljava/io/FileDescriptor;Z)I", |t, a| {
        let fd = descriptor_fd(t, arg_ref(a, 0));
        with_fd(t, fd, |f| f.sync())?;
        ret_i(0)
    });
    n.add("sun/nio/ch/UnixFileDispatcherImpl.seek0(Ljava/io/FileDescriptor;J)J", |t, a| {
        let fd = descriptor_fd(t, arg_ref(a, 0));
        let offset = arg_j(a, 1);
        ret_j(with_fd(t, fd, |f| {
            if offset >= 0 {
                f.seek(offset as u64)?;
            }
            f.position()
        })? as i64)
    });
    n.add("sun/nio/ch/UnixFileDispatcherImpl.truncate0(Ljava/io/FileDescriptor;J)I", |t, a| {
        let fd = descriptor_fd(t, arg_ref(a, 0));
        let size = arg_j(a, 1).max(0) as u64;
        with_fd(t, fd, |f| f.set_len(size))?;
        ret_i(0)
    });
    n.add("sun/nio/ch/UnixFileDispatcherImpl.size0(Ljava/io/FileDescriptor;)J", |t, a| {
        let fd = descriptor_fd(t, arg_ref(a, 0));
        ret_j(with_fd(t, fd, |f| f.len())? as i64)
    });
    n.add("sun/nio/ch/UnixFileDispatcherImpl.available0(Ljava/io/FileDescriptor;)I", |t, a| {
        let fd = descriptor_fd(t, arg_ref(a, 0));
        let available = with_fd(t, fd, |f| Ok(f.len()?.saturating_sub(f.position()?)))?;
        ret_i(available.min(i32::MAX as u64) as i32)
    });
    n.add("sun/nio/ch/UnixFileDispatcherImpl.isOther0(Ljava/io/FileDescriptor;)Z", return_false);
    n.add("sun/nio/ch/UnixFileDispatcherImpl.lock0(Ljava/io/FileDescriptor;ZJJZ)I", |_, _| ret_i(LOCKED));
    n.add("sun/nio/ch/UnixFileDispatcherImpl.release0(Ljava/io/FileDescriptor;JJ)V", no_op);
    n.add("sun/nio/ch/UnixFileDispatcherImpl.closeIntFD(I)V", |t, a| {
        let fd = arg_i(a, 0);
        if !t.vm.sockets.close(fd) {
            t.vm.fds.close(fd);
        }
        ret_void()
    });
    n.add("sun/nio/ch/UnixFileDispatcherImpl.allocationGranularity0()J", |_, _| ret_j(4096));
    n.add("sun/nio/ch/UnixFileDispatcherImpl.map0(Ljava/io/FileDescriptor;IJJZ)J", file_map);
    n.add("sun/nio/ch/UnixFileDispatcherImpl.unmap0(JJ)I", |t, a| {
        unmap(t, arg_j(a, 0));
        ret_i(0)
    });
    n.add("sun/nio/ch/UnixFileDispatcherImpl.setDirect0(Ljava/io/FileDescriptor;)I", |_, _| ret_i(-1));
}

static MOUNT_TABLES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static MOUNTS_READ: Mutex<std::collections::BTreeSet<i64>> = Mutex::new(std::collections::BTreeSet::new());

// --- helpers ------------------------------------------------------------------------

fn c_path(t: &Thread<'_>, address: i64) -> String {
    // SAFETY: NativeBuffer holds a NUL-terminated path at `address`.
    let bytes = unsafe { std::ffi::CStr::from_ptr(address as usize as *const std::ffi::c_char) }.to_bytes();
    let path = String::from_utf8_lossy(bytes).into_owned();
    if path.starts_with('/') { path } else { format!("{}/{path}", t.vm.config.user_dir) }
}

pub(crate) fn errno(error: &std::io::Error) -> i32 {
    use std::io::ErrorKind::*;
    match error.kind() {
        NotFound => ENOENT,
        PermissionDenied => EACCES,
        AlreadyExists => EEXIST,
        NotADirectory => ENOTDIR,
        IsADirectory => EISDIR,
        DirectoryNotEmpty => ENOTEMPTY,
        CrossesDevices => EXDEV,
        InvalidInput => EINVAL,
        Unsupported => ENOTSUP,
        _ => error.raw_os_error().unwrap_or(EIO),
    }
}

fn strerror(errno: i32) -> &'static str {
    match errno {
        ENOENT => "No such file or directory",
        EIO => "Input/output error",
        EBADF => "Bad file descriptor",
        EACCES => "Permission denied",
        EEXIST => "File exists",
        EXDEV => "Invalid cross-device link",
        ENOTDIR => "Not a directory",
        EISDIR => "Is a directory",
        EINVAL => "Invalid argument",
        ENOTEMPTY => "Directory not empty",
        ENOTSUP => "Operation not supported",
        _ => "Unknown error",
    }
}

fn unix_exception(t: &mut Thread<'_>, errno: i32) -> crate::interp::Throw {
    let result = (|| {
        let class = t.load_class("sun/nio/fs/UnixException")?;
        t.ensure_initialized(class)?;
        let exception = t.vm.new_object(class).map_err(|e| t.throw_vm_error(e))?;
        let init = class.find_method("<init>", "(I)V").expect("UnixException(int)");
        t.invoke(init, &[exception.slot(), errno as u32 as u64])?;
        Ok(exception)
    })();
    match result {
        Ok(exception) => crate::interp::Throw(exception),
        Err(throw) => throw,
    }
}

fn fill_attributes(t: &mut Thread<'_>, attrs: Ref, path: &str, meta: &Metadata) {
    let class = "sun/nio/fs/UnixFileAttributes";
    let kind = if meta.kind == FileKind::Dir { S_IFDIR | 0o755 } else { S_IFREG | 0o644 };
    let mode = if meta.readonly { kind & !0o222 } else { kind };
    // A stable inode number so that file keys / isSameFile work.
    let inode = path.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3));
    let (sec, nsec) = (meta.modified.div_euclid(1000), meta.modified.rem_euclid(1000) * 1_000_000);
    let fields: [(&str, u64); 14] = [
        ("st_mode", mode as u64),
        ("st_ino", inode),
        ("st_dev", 1),
        ("st_rdev", 0),
        ("st_nlink", 1),
        ("st_uid", 1000),
        ("st_gid", 1000),
        ("st_size", meta.len),
        ("st_atime_sec", sec as u64),
        ("st_atime_nsec", nsec as u64),
        ("st_mtime_sec", sec as u64),
        ("st_mtime_nsec", nsec as u64),
        ("st_ctime_sec", sec as u64),
        ("st_ctime_nsec", nsec as u64),
    ];
    for (name, value) in fields {
        t.set_field(attrs, class, name, value);
    }
}

fn descriptor_fd(t: &Thread<'_>, descriptor: Ref) -> i32 {
    t.field(descriptor, "java/io/FileDescriptor", "fd") as i32
}

fn with_fd<R>(t: &mut Thread<'_>, fd: i32, f: impl FnOnce(&mut dyn crate::fs::File) -> std::io::Result<R>) -> JResult<R> {
    let Some(file) = t.vm.fds.get(fd) else {
        return Err(t.throw_new("java/io/IOException", Some("Bad file descriptor")));
    };
    let result = f(file.lock().unwrap().as_mut());
    result.map_err(|e| t.throw_new("java/io/IOException", Some(&super::io::io_message(&e))))
}

fn write_all(f: &mut dyn crate::fs::File, mut data: &[u8]) -> std::io::Result<()> {
    while !data.is_empty() {
        let n = f.write(data)?;
        if n == 0 {
            return Err(std::io::ErrorKind::WriteZero.into());
        }
        data = &data[n..];
    }
    Ok(())
}

/// Reads into native memory; returns bytes read or IOStatus.EOF.
fn fd_read(t: &mut Thread<'_>, fd: i32, address: i64, len: i32, position: Option<u64>) -> JResult<i32> {
    if fd == 0 || len <= 0 {
        return Ok(if len <= 0 { 0 } else { IOSTATUS_EOF });
    }
    // SAFETY: the caller passes a native buffer of `len` bytes.
    let buf = unsafe { std::slice::from_raw_parts_mut(address as usize as *mut u8, len as usize) };
    let n = with_fd(t, fd, |f| match position {
        Some(pos) => {
            let saved = f.position()?;
            f.seek(pos)?;
            let n = read_full(f, buf);
            f.seek(saved)?;
            n
        }
        None => f.read(buf),
    })?;
    Ok(if n == 0 { IOSTATUS_EOF } else { n as i32 })
}

fn read_full(f: &mut dyn crate::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut total = 0;
    while total < buf.len() {
        let n = f.read(&mut buf[total..])?;
        if n == 0 {
            break;
        }
        total += n;
    }
    Ok(total)
}

fn fd_write(t: &mut Thread<'_>, fd: i32, address: i64, len: i32, position: Option<u64>) -> JResult<i32> {
    if len <= 0 {
        return Ok(0);
    }
    // SAFETY: the caller passes a native buffer of `len` bytes.
    let data = unsafe { std::slice::from_raw_parts(address as usize as *const u8, len as usize) };
    if fd == 1 || fd == 2 {
        super::io::write_fd(t, fd, data)?;
        return Ok(len);
    }
    with_fd(t, fd, |f| {
        match position {
            Some(pos) => {
                let saved = f.position()?;
                f.seek(pos)?;
                write_all(f, data)?;
                f.seek(saved)?;
            }
            None => write_all(f, data)?,
        }
        Ok(())
    })?;
    Ok(len)
}

/// (base, len) pairs of a native `struct iovec` array.
pub(crate) fn iovecs(address: i64, count: i32) -> Vec<(i64, usize)> {
    let word = std::mem::size_of::<usize>();
    (0..count.max(0) as usize)
        .map(|i| {
            // SAFETY: IOVecWrapper builds `count` iovecs at `address`.
            unsafe {
                let entry = (address as usize + i * 2 * word) as *const usize;
                (entry.read_unaligned() as i64, entry.add(1).read_unaligned())
            }
        })
        .collect()
}

// --- directory streams ------------------------------------------------------------

struct DirTable {
    dirs: Mutex<(i64, HashMap<i64, VecDeque<String>>)>,
}

static DIRS: std::sync::LazyLock<DirTable> = std::sync::LazyLock::new(|| DirTable { dirs: Mutex::new((0, HashMap::new())) });

impl DirTable {
    fn open(&self, entries: Vec<String>) -> i64 {
        let mut dirs = self.dirs.lock().unwrap();
        dirs.0 += 1;
        let handle = dirs.0;
        dirs.1.insert(handle, entries.into());
        handle
    }

    fn next(&self, handle: i64) -> Option<String> {
        self.dirs.lock().unwrap().1.get_mut(&handle)?.pop_front()
    }

    fn close(&self, handle: i64) {
        self.dirs.lock().unwrap().1.remove(&handle);
    }
}

// --- memory-mapped files -------------------------------------------------------------
// Emulated by copying: the mapping is a native buffer filled from the file and,
// for read-write mappings, written back when unmapped.

struct Mapping {
    fd: Option<std::sync::Arc<std::sync::Mutex<Box<dyn crate::fs::File>>>>,
    position: u64,
    len: usize,
}

static MAPPINGS: Mutex<Option<HashMap<i64, Mapping>>> = Mutex::new(None);

fn file_map(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
    let fd = descriptor_fd(t, arg_ref(a, 0));
    let prot = arg_i(a, 1);
    let (position, len) = (arg_j(a, 2) as u64, arg_j(a, 4) as usize);
    let Some(file) = t.vm.fds.get(fd) else { return Err(t.throw_new("java/io/IOException", Some("Bad file descriptor"))) };
    let layout = std::alloc::Layout::from_size_align(len.max(1), 4096).unwrap();
    // SAFETY: non-zero layout.
    let address = unsafe { std::alloc::alloc_zeroed(layout) };
    {
        let mut f = file.lock().unwrap();
        let saved = f.position().unwrap_or(0);
        if f.seek(position).is_ok() {
            // SAFETY: freshly allocated buffer of `len` bytes.
            let buf = unsafe { std::slice::from_raw_parts_mut(address, len) };
            let _ = read_full(f.as_mut(), buf);
        }
        let _ = f.seek(saved);
    }
    let mapping = Mapping { fd: (prot == MAP_RW).then_some(file), position, len };
    MAPPINGS.lock().unwrap().get_or_insert_with(HashMap::new).insert(address as i64, mapping);
    ret_j(address as i64)
}

fn unmap(_t: &mut Thread<'_>, address: i64) {
    let Some(mapping) = MAPPINGS.lock().unwrap().as_mut().and_then(|m| m.remove(&address)) else { return };
    // SAFETY: `address` came from `file_map` with this length.
    let data = unsafe { std::slice::from_raw_parts(address as usize as *const u8, mapping.len) };
    if let Some(file) = &mapping.fd {
        let mut f = file.lock().unwrap();
        let saved = f.position().unwrap_or(0);
        if f.seek(mapping.position).is_ok() {
            let _ = write_all(f.as_mut(), data);
        }
        let _ = f.seek(saved);
    }
    // SAFETY: allocated in `file_map` with this layout.
    unsafe { std::alloc::dealloc(address as usize as *mut u8, std::alloc::Layout::from_size_align(mapping.len.max(1), 4096).unwrap()) };
}
