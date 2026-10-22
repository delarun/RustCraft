//! java.io natives on top of the VM file system: FileDescriptor,
//! FileInputStream/FileOutputStream, RandomAccessFile, UnixFileSystem, plus
//! Console and signals. fd 0–2 are the VM's standard streams.

use super::*;
use crate::fs::{FileKind, OpenOptions};
use crate::heap::ARRAY_BASE_OFFSET;
use std::io::{Read, Write};

const FD: &str = "java/io/FileDescriptor";
const FIS: &str = "java/io/FileInputStream";
const FOS: &str = "java/io/FileOutputStream";
const RAF: &str = "java/io/RandomAccessFile";

pub(super) fn register(n: &mut Natives) {
    n.add("java/io/FileDescriptor.initIDs()V", no_op);
    n.add("java/io/FileDescriptor.getHandle(I)J", |_, _| ret_j(-1));
    n.add("java/io/FileDescriptor.getAppend(I)Z", return_false);
    n.add("java/io/FileDescriptor.close0()V", |t, a| {
        let descriptor = arg_ref(a, 0);
        let fd = t.field(descriptor, FD, "fd") as i32;
        if fd > 2 {
            t.vm.fds.close(fd);
        }
        t.set_field(descriptor, FD, "fd", (-1i32) as u32 as u64);
        ret_void()
    });
    n.add("java/io/FileDescriptor.sync0()V", |t, a| {
        let fd = t.field(arg_ref(a, 0), FD, "fd") as i32;
        if let Some(file) = t.vm.fds.get(fd) {
            let _ = file.lock().unwrap().sync();
        }
        ret_void()
    });

    n.add("java/io/FileOutputStream.initIDs()V", no_op);
    n.add("java/io/FileOutputStream.open0(Ljava/lang/String;Z)V", |t, a| {
        let append = arg_z(a, 2);
        let options = OpenOptions { write: true, create: true, append, truncate: !append, ..Default::default() };
        open_stream(t, arg_ref(a, 0), FOS, arg_ref(a, 1), &options)
    });
    n.add("java/io/FileOutputStream.writeBytes([BIIZ)V", |t, a| {
        let fd = stream_fd(t, arg_ref(a, 0), FOS)?;
        write_array(t, fd, arg_ref(a, 1), arg_i(a, 2), arg_i(a, 3))
    });
    n.add("java/io/FileOutputStream.write(IZ)V", |t, a| {
        let fd = stream_fd(t, arg_ref(a, 0), FOS)?;
        write_fd(t, fd, &[a[1] as u8])
    });

    n.add("java/io/FileInputStream.initIDs()V", no_op);
    n.add("java/io/FileInputStream.open0(Ljava/lang/String;)V", |t, a| {
        let options = OpenOptions { read: true, ..Default::default() };
        open_stream(t, arg_ref(a, 0), FIS, arg_ref(a, 1), &options)
    });
    n.add("java/io/FileInputStream.readBytes([BII)I", |t, a| {
        let fd = stream_fd(t, arg_ref(a, 0), FIS)?;
        read_array(t, fd, arg_ref(a, 1), arg_i(a, 2), arg_i(a, 3))
    });
    n.add("java/io/FileInputStream.read0()I", |t, a| {
        let fd = stream_fd(t, arg_ref(a, 0), FIS)?;
        let mut byte = [0u8];
        let n = read_fd(t, fd, &mut byte)?;
        ret_i(if n == 0 { -1 } else { byte[0] as i32 })
    });
    n.add("java/io/FileInputStream.available0()I", |t, a| {
        let fd = stream_fd(t, arg_ref(a, 0), FIS)?;
        let available = with_file(t, fd, |f| Ok(f.len()?.saturating_sub(f.position()?)))?.unwrap_or(0);
        ret_i(available.min(i32::MAX as u64) as i32)
    });
    n.add("java/io/FileInputStream.length0()J", |t, a| {
        let fd = stream_fd(t, arg_ref(a, 0), FIS)?;
        ret_j(with_file(t, fd, |f| f.len())?.unwrap_or(0) as i64)
    });
    n.add("java/io/FileInputStream.position0()J", |t, a| {
        let fd = stream_fd(t, arg_ref(a, 0), FIS)?;
        ret_j(with_file(t, fd, |f| f.position())?.unwrap_or(0) as i64)
    });
    n.add("java/io/FileInputStream.skip0(J)J", |t, a| {
        let fd = stream_fd(t, arg_ref(a, 0), FIS)?;
        let n = arg_j(a, 1);
        let skipped = with_file(t, fd, |f| {
            let pos = f.position()?;
            let target = (pos as i64 + n).max(0) as u64;
            f.seek(target)?;
            Ok(target as i64 - pos as i64)
        })?;
        ret_j(skipped.unwrap_or(0))
    });
    n.add("java/io/FileInputStream.isRegularFile0(Ljava/io/FileDescriptor;)Z", |t, a| {
        let fd = t.field(arg_ref(a, 1), FD, "fd") as i32;
        ret_z(t.vm.fds.get(fd).is_some())
    });

    n.add("java/io/RandomAccessFile.initIDs()V", no_op);
    n.add("java/io/RandomAccessFile.open0(Ljava/lang/String;I)V", |t, a| {
        // RandomAccessFile.O_RDONLY = 1, O_RDWR = 2 (O_SYNC/O_DSYNC are ignored).
        let write = arg_i(a, 2) & 2 != 0;
        let options = OpenOptions { read: true, write, create: write, ..Default::default() };
        open_stream(t, arg_ref(a, 0), RAF, arg_ref(a, 1), &options)
    });
    n.add("java/io/RandomAccessFile.read0()I", |t, a| {
        let fd = stream_fd(t, arg_ref(a, 0), RAF)?;
        let mut byte = [0u8];
        let n = read_fd(t, fd, &mut byte)?;
        ret_i(if n == 0 { -1 } else { byte[0] as i32 })
    });
    n.add("java/io/RandomAccessFile.readBytes0([BII)I", |t, a| {
        let fd = stream_fd(t, arg_ref(a, 0), RAF)?;
        read_array(t, fd, arg_ref(a, 1), arg_i(a, 2), arg_i(a, 3))
    });
    n.add("java/io/RandomAccessFile.write0(I)V", |t, a| {
        let fd = stream_fd(t, arg_ref(a, 0), RAF)?;
        write_fd(t, fd, &[a[1] as u8])
    });
    n.add("java/io/RandomAccessFile.writeBytes0([BII)V", |t, a| {
        let fd = stream_fd(t, arg_ref(a, 0), RAF)?;
        write_array(t, fd, arg_ref(a, 1), arg_i(a, 2), arg_i(a, 3))
    });
    n.add("java/io/RandomAccessFile.getFilePointer()J", |t, a| {
        let fd = stream_fd(t, arg_ref(a, 0), RAF)?;
        ret_j(with_file(t, fd, |f| f.position())?.unwrap_or(0) as i64)
    });
    n.add("java/io/RandomAccessFile.seek0(J)V", |t, a| {
        let fd = stream_fd(t, arg_ref(a, 0), RAF)?;
        let pos = arg_j(a, 1).max(0) as u64;
        with_file(t, fd, |f| f.seek(pos))?;
        ret_void()
    });
    n.add("java/io/RandomAccessFile.length0()J", |t, a| {
        let fd = stream_fd(t, arg_ref(a, 0), RAF)?;
        ret_j(with_file(t, fd, |f| f.len())?.unwrap_or(0) as i64)
    });
    n.add("java/io/RandomAccessFile.setLength0(J)V", |t, a| {
        let fd = stream_fd(t, arg_ref(a, 0), RAF)?;
        let len = arg_j(a, 1).max(0) as u64;
        with_file(t, fd, |f| {
            f.set_len(len)?;
            if f.position()? > len {
                f.seek(len)?;
            }
            Ok(())
        })?;
        ret_void()
    });

    register_unix_file_system(n);

    n.add("java/io/Console.istty()Z", return_false);

    n.add("jdk/internal/misc/Signal.findSignal0(Ljava/lang/String;)I", |t, a| {
        let name = t.vm.string_value(t.null_check(arg_ref(a, 0))?);
        ret_i(match name.as_str() {
            "HUP" => 1,
            "INT" => 2,
            "TERM" => 15,
            _ => -1,
        })
    });
    n.add("jdk/internal/misc/Signal.handle0(IJ)J", return_zero_long);
    n.add("jdk/internal/misc/Signal.raise0(I)V", no_op);
    n.add("jdk/internal/misc/ScopedMemoryAccess.registerNatives()V", no_op);
}

// --- streams ------------------------------------------------------------------------

pub(crate) fn io_message(error: &std::io::Error) -> String {
    use std::io::ErrorKind::*;
    match error.kind() {
        NotFound => "No such file or directory".into(),
        PermissionDenied => "Permission denied".into(),
        AlreadyExists => "File exists".into(),
        IsADirectory => "Is a directory".into(),
        NotADirectory => "Not a directory".into(),
        DirectoryNotEmpty => "Directory not empty".into(),
        _ => error.to_string(),
    }
}

fn io_exception(t: &mut Thread<'_>, error: &std::io::Error) -> crate::interp::Throw {
    t.throw_new("java/io/IOException", Some(&io_message(error)))
}

/// Opens `path` and stores the new fd in the stream's FileDescriptor.
fn open_stream(t: &mut Thread<'_>, stream: Ref, class: &str, path: Ref, options: &OpenOptions) -> JResult<Option<u64>> {
    let path = t.null_check(path)?;
    let path = absolute(t, &t.vm.string_value(path));
    let file = match t.vm.vfs.metadata(&path) {
        Ok(meta) if meta.kind == FileKind::Dir => Err(std::io::Error::new(std::io::ErrorKind::IsADirectory, "dir")),
        _ => t.vm.vfs.open(&path, options),
    };
    match file {
        Ok(file) => {
            let fd = t.vm.fds.insert(file);
            let descriptor = t.field_ref(stream, class, "fd");
            t.set_field(descriptor, FD, "fd", fd as u32 as u64);
            ret_void()
        }
        Err(e) => {
            let message = format!("{path} ({})", io_message(&e));
            Err(t.throw_new("java/io/FileNotFoundException", Some(&message)))
        }
    }
}

fn stream_fd(t: &mut Thread<'_>, stream: Ref, class: &str) -> JResult<i32> {
    let descriptor = t.field_ref(stream, class, "fd");
    let descriptor = t.null_check(descriptor)?;
    let fd = t.field(descriptor, FD, "fd") as i32;
    if fd < 0 {
        return Err(t.throw_new("java/io/IOException", Some("Stream Closed")));
    }
    Ok(fd)
}

/// Runs `f` on an open file; `None` for the standard streams.
fn with_file<R>(t: &mut Thread<'_>, fd: i32, f: impl FnOnce(&mut dyn crate::fs::File) -> std::io::Result<R>) -> JResult<Option<R>> {
    if (0..=2).contains(&fd) {
        return Ok(None);
    }
    let Some(file) = t.vm.fds.get(fd) else {
        return Err(t.throw_new("java/io/IOException", Some("Bad file descriptor")));
    };
    let result = f(file.lock().unwrap().as_mut());
    result.map(Some).map_err(|e| io_exception(t, &e))
}

fn read_fd(t: &mut Thread<'_>, fd: i32, buf: &mut [u8]) -> JResult<usize> {
    if fd == 0 {
        // Standard input may block (a terminal, a pipe): not under the GIL.
        let vm = t.vm;
        return match t.blocking(|| vm.stdin.lock().unwrap().read(buf)) {
            Ok(n) => Ok(n),
            Err(e) => Err(io_exception(t, &e)),
        };
    }
    Ok(with_file(t, fd, |f| f.read(buf))?.unwrap_or(0))
}

fn read_array(t: &mut Thread<'_>, fd: i32, array: Ref, offset: i32, len: i32) -> JResult<Option<u64>> {
    let array = t.null_check(array)?;
    let length = t.vm.heap.array_length(array);
    if offset < 0 || len < 0 || offset as i64 + len as i64 > length as i64 {
        return Err(t.throw_new("java/lang/IndexOutOfBoundsException", None));
    }
    if len == 0 {
        return ret_i(0);
    }
    let mut buf = vec![0u8; len as usize];
    let n = read_fd(t, fd, &mut buf)?;
    if n == 0 {
        return ret_i(-1);
    }
    t.vm.heap.bytes_mut(array, ARRAY_BASE_OFFSET + offset as u32, n).copy_from_slice(&buf[..n]);
    ret_i(n as i32)
}

fn write_array(t: &mut Thread<'_>, fd: i32, array: Ref, offset: i32, len: i32) -> JResult<Option<u64>> {
    let array = t.null_check(array)?;
    let length = t.vm.heap.array_length(array);
    if offset < 0 || len < 0 || offset as i64 + len as i64 > length as i64 {
        return Err(t.throw_new("java/lang/IndexOutOfBoundsException", None));
    }
    let data = t.vm.heap.bytes(array, ARRAY_BASE_OFFSET + offset as u32, len as usize).to_vec();
    write_fd(t, fd, &data)
}

pub(crate) fn write_fd(t: &mut Thread<'_>, fd: i32, data: &[u8]) -> JResult<Option<u64>> {
    let result = match fd {
        1 => t.vm.stdout.lock().unwrap().write_all(data),
        2 => t.vm.stderr.lock().unwrap().write_all(data),
        _ => {
            with_file(t, fd, |f| f.write_all_compat(data))?;
            Ok(())
        }
    };
    if let Err(e) = result {
        return Err(io_exception(t, &e));
    }
    ret_void()
}

trait WriteAll {
    fn write_all_compat(&mut self, data: &[u8]) -> std::io::Result<()>;
}

impl WriteAll for dyn crate::fs::File + '_ {
    fn write_all_compat(&mut self, mut data: &[u8]) -> std::io::Result<()> {
        while !data.is_empty() {
            let n = self.write(data)?;
            if n == 0 {
                return Err(std::io::ErrorKind::WriteZero.into());
            }
            data = &data[n..];
        }
        Ok(())
    }
}

// --- java.io.UnixFileSystem ----------------------------------------------------------

const BA_EXISTS: i32 = 0x01;
const BA_REGULAR: i32 = 0x02;
const BA_DIRECTORY: i32 = 0x04;

fn file_path(t: &mut Thread<'_>, file: Ref) -> JResult<String> {
    let file = t.null_check(file)?;
    let path = t.field_ref(file, "java/io/File", "path");
    Ok(t.vm.string_value(path))
}

fn file_abs_path(t: &mut Thread<'_>, file: Ref) -> JResult<String> {
    let path = file_path(t, file)?;
    Ok(absolute(t, &path))
}

/// Makes a Java path absolute against user.dir (File paths may be relative).
fn absolute(t: &Thread<'_>, path: &str) -> String {
    if path.starts_with('/') { path.to_owned() } else { format!("{}/{path}", t.vm.config.user_dir) }
}

fn register_unix_file_system(n: &mut Natives) {
    n.add("java/io/UnixFileSystem.initIDs()V", no_op);
    n.add("java/io/UnixFileSystem.canonicalize0(Ljava/lang/String;)Ljava/lang/String;", |t, a| {
        let path = t.vm.string_value(t.null_check(arg_ref(a, 1))?);
        let canonical = crate::fs::normalize(&absolute(t, &path));
        ret_ref(t.new_string(&canonical)?)
    });
    n.add("java/io/UnixFileSystem.getBooleanAttributes0(Ljava/io/File;)I", |t, a| {
        let path = file_abs_path(t, arg_ref(a, 1))?;
        ret_i(match t.vm.vfs.metadata(&path) {
            Ok(meta) if meta.kind == FileKind::Dir => BA_EXISTS | BA_DIRECTORY,
            Ok(_) => BA_EXISTS | BA_REGULAR,
            Err(_) => 0,
        })
    });
    n.add("java/io/UnixFileSystem.checkAccess0(Ljava/io/File;I)Z", |t, a| {
        let path = file_abs_path(t, arg_ref(a, 1))?;
        let write = arg_i(a, 2) & 2 != 0;
        ret_z(t.vm.vfs.metadata(&path).is_ok_and(|m| !(write && m.readonly)))
    });
    n.add("java/io/UnixFileSystem.getLastModifiedTime0(Ljava/io/File;)J", |t, a| {
        let path = file_abs_path(t, arg_ref(a, 1))?;
        ret_j(t.vm.vfs.metadata(&path).map_or(0, |m| m.modified))
    });
    n.add("java/io/UnixFileSystem.getLength0(Ljava/io/File;)J", |t, a| {
        let path = file_abs_path(t, arg_ref(a, 1))?;
        ret_j(t.vm.vfs.metadata(&path).map_or(0, |m| m.len as i64))
    });
    n.add("java/io/UnixFileSystem.setPermission0(Ljava/io/File;IZZ)Z", |t, a| {
        let path = file_abs_path(t, arg_ref(a, 1))?;
        ret_z(t.vm.vfs.metadata(&path).is_ok())
    });
    n.add("java/io/UnixFileSystem.createFileExclusively0(Ljava/lang/String;)Z", |t, a| {
        let path = t.null_check(arg_ref(a, 1))?;
        let path = absolute(t, &t.vm.string_value(path));
        let options = OpenOptions { write: true, create_new: true, ..Default::default() };
        match t.vm.vfs.open(&path, &options) {
            Ok(_) => ret_z(true),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => ret_z(false),
            Err(e) => Err(io_exception(t, &e)),
        }
    });
    n.add("java/io/UnixFileSystem.delete0(Ljava/io/File;)Z", |t, a| {
        let path = file_abs_path(t, arg_ref(a, 1))?;
        ret_z(t.vm.vfs.remove(&path).is_ok())
    });
    n.add("java/io/UnixFileSystem.list0(Ljava/io/File;)[Ljava/lang/String;", |t, a| {
        let path = file_abs_path(t, arg_ref(a, 1))?;
        let Ok(mut entries) = t.vm.vfs.read_dir(&path) else { return ret_ref(Ref::NULL) };
        entries.sort();
        let mut strings = Vec::with_capacity(entries.len());
        for entry in &entries {
            strings.push(t.new_string(entry)?);
        }
        ret_ref(t.new_object_array("java/lang/String", &strings)?)
    });
    n.add("java/io/UnixFileSystem.createDirectory0(Ljava/io/File;)Z", |t, a| {
        let path = file_abs_path(t, arg_ref(a, 1))?;
        ret_z(t.vm.vfs.create_dir(&path).is_ok())
    });
    n.add("java/io/UnixFileSystem.rename0(Ljava/io/File;Ljava/io/File;)Z", |t, a| {
        let from = file_abs_path(t, arg_ref(a, 1))?;
        let to = file_abs_path(t, arg_ref(a, 2))?;
        ret_z(t.vm.vfs.rename(&from, &to).is_ok())
    });
    n.add("java/io/UnixFileSystem.setLastModifiedTime0(Ljava/io/File;J)Z", |t, a| {
        let path = file_abs_path(t, arg_ref(a, 1))?;
        ret_z(t.vm.vfs.set_modified(&path, arg_j(a, 2)).is_ok())
    });
    n.add("java/io/UnixFileSystem.setReadOnly0(Ljava/io/File;)Z", |t, a| {
        let path = file_abs_path(t, arg_ref(a, 1))?;
        ret_z(t.vm.vfs.metadata(&path).is_ok())
    });
    n.add("java/io/UnixFileSystem.getSpace0(Ljava/io/File;I)J", |_, _| ret_j(1 << 34));
    n.add("java/io/UnixFileSystem.getNameMax0(Ljava/lang/String;)J", |_, _| ret_j(255));
}
