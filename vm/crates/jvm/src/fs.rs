//! The VM's file system: a mount table of backends behind Java's file APIs.
//!
//! Natively the host file system is mounted; in the browser the game directory
//! will be backed by OPFS and the JDK files by downloaded data. Paths are
//! absolute, `/`-separated strings, normalised lexically.

use std::collections::{BTreeMap, HashMap};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    File,
    Dir,
}

#[derive(Debug, Clone, Copy)]
pub struct Metadata {
    pub kind: FileKind,
    pub len: u64,
    /// Milliseconds since the epoch.
    pub modified: i64,
    pub readonly: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct OpenOptions {
    pub read: bool,
    pub write: bool,
    pub append: bool,
    pub create: bool,
    pub create_new: bool,
    pub truncate: bool,
}

pub trait File: Send {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;
    fn write(&mut self, buf: &[u8]) -> io::Result<usize>;
    fn seek(&mut self, pos: u64) -> io::Result<()>;
    fn position(&mut self) -> io::Result<u64>;
    fn len(&mut self) -> io::Result<u64>;
    fn set_len(&mut self, len: u64) -> io::Result<()>;
    fn sync(&mut self) -> io::Result<()>;
}

pub trait FileSystem: Send + Sync {
    fn metadata(&self, path: &str) -> io::Result<Metadata>;
    fn open(&self, path: &str, options: &OpenOptions) -> io::Result<Box<dyn File>>;
    fn read_dir(&self, path: &str) -> io::Result<Vec<String>>;
    fn create_dir(&self, path: &str) -> io::Result<()>;
    fn remove_file(&self, path: &str) -> io::Result<()>;
    fn remove_dir(&self, path: &str) -> io::Result<()>;
    fn rename(&self, from: &str, to: &str) -> io::Result<()>;
    fn set_modified(&self, path: &str, millis: i64) -> io::Result<()>;
}

/// Lexically normalises an absolute path: collapses `//`, `.` and `..`.
pub fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    format!("/{}", parts.join("/"))
}

/// Mount table; the longest matching prefix wins.
#[derive(Default)]
pub struct Vfs {
    mounts: Vec<(String, Arc<dyn FileSystem>)>,
}

impl Vfs {
    pub fn mount(&mut self, prefix: &str, fs: Arc<dyn FileSystem>) {
        self.mounts.push((normalize(prefix), fs));
        self.mounts.sort_by_key(|(prefix, _)| std::cmp::Reverse(prefix.len()));
    }

    /// The backend for a path and the path relative to its mount point
    /// (always starting with `/`).
    fn resolve(&self, path: &str) -> io::Result<(&dyn FileSystem, String)> {
        let path = normalize(path);
        for (prefix, fs) in &self.mounts {
            let rest = if prefix == "/" {
                Some(path.as_str())
            } else {
                path.strip_prefix(prefix.as_str()).filter(|r| r.is_empty() || r.starts_with('/'))
            };
            if let Some(rest) = rest {
                let rest = if rest.is_empty() { "/".to_owned() } else { rest.to_owned() };
                return Ok((fs.as_ref(), rest));
            }
        }
        Err(io::Error::new(io::ErrorKind::NotFound, "no file system mounted"))
    }

    pub fn metadata(&self, path: &str) -> io::Result<Metadata> {
        let (fs, rel) = self.resolve(path)?;
        fs.metadata(&rel)
    }

    pub fn open(&self, path: &str, options: &OpenOptions) -> io::Result<Box<dyn File>> {
        let (fs, rel) = self.resolve(path)?;
        fs.open(&rel, options)
    }

    pub fn read_dir(&self, path: &str) -> io::Result<Vec<String>> {
        let (fs, rel) = self.resolve(path)?;
        let mut entries = fs.read_dir(&rel)?;
        // Mount points appear as directory entries of their parent.
        let normalized = normalize(path);
        for (prefix, _) in &self.mounts {
            if let Some((parent, name)) = prefix.rsplit_once('/') {
                let parent = if parent.is_empty() { "/" } else { parent };
                if parent == normalized && !name.is_empty() && !entries.iter().any(|e| e == name) {
                    entries.push(name.to_owned());
                }
            }
        }
        Ok(entries)
    }

    pub fn create_dir(&self, path: &str) -> io::Result<()> {
        let (fs, rel) = self.resolve(path)?;
        fs.create_dir(&rel)
    }

    pub fn remove(&self, path: &str) -> io::Result<()> {
        let (fs, rel) = self.resolve(path)?;
        match fs.metadata(&rel)?.kind {
            FileKind::Dir => fs.remove_dir(&rel),
            FileKind::File => fs.remove_file(&rel),
        }
    }

    pub fn rename(&self, from: &str, to: &str) -> io::Result<()> {
        let (fs, from_rel) = self.resolve(from)?;
        let (to_fs, to_rel) = self.resolve(to)?;
        if !std::ptr::addr_eq(fs as *const dyn FileSystem, to_fs as *const dyn FileSystem) {
            return Err(io::Error::new(io::ErrorKind::CrossesDevices, "rename across mounts"));
        }
        fs.rename(&from_rel, &to_rel)
    }

    pub fn set_modified(&self, path: &str, millis: i64) -> io::Result<()> {
        let (fs, rel) = self.resolve(path)?;
        fs.set_modified(&rel, millis)
    }
}

/// Open file descriptors. 0–2 are the standard streams and handled by callers.
pub struct FdTable {
    files: Mutex<HashMap<i32, Arc<Mutex<Box<dyn File>>>>>,
    next: AtomicI32,
}

impl Default for FdTable {
    fn default() -> Self {
        FdTable { files: Mutex::new(HashMap::new()), next: AtomicI32::new(3) }
    }
}

impl FdTable {
    /// A fresh descriptor number (sockets share the numbering with files).
    pub fn allocate(&self) -> i32 {
        self.next.fetch_add(1, Ordering::Relaxed)
    }

    pub fn insert(&self, file: Box<dyn File>) -> i32 {
        let fd = self.next.fetch_add(1, Ordering::Relaxed);
        self.files.lock().unwrap().insert(fd, Arc::new(Mutex::new(file)));
        fd
    }

    /// A second descriptor for the same open file (dup).
    pub fn insert_shared(&self, file: Arc<Mutex<Box<dyn File>>>) -> i32 {
        let fd = self.next.fetch_add(1, Ordering::Relaxed);
        self.files.lock().unwrap().insert(fd, file);
        fd
    }

    pub fn get(&self, fd: i32) -> Option<Arc<Mutex<Box<dyn File>>>> {
        self.files.lock().unwrap().get(&fd).cloned()
    }

    pub fn close(&self, fd: i32) -> bool {
        self.files.lock().unwrap().remove(&fd).is_some()
    }
}

// --- host file system ---------------------------------------------------------------

/// A host directory (native builds).
pub struct HostFs {
    root: PathBuf,
}

impl HostFs {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        HostFs { root: root.into() }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel.trim_start_matches('/'))
    }
}

struct HostFile(std::fs::File);

impl File for HostFile {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }
    fn seek(&mut self, pos: u64) -> io::Result<()> {
        self.0.seek(SeekFrom::Start(pos)).map(|_| ())
    }
    fn position(&mut self) -> io::Result<u64> {
        self.0.stream_position()
    }
    fn len(&mut self) -> io::Result<u64> {
        Ok(self.0.metadata()?.len())
    }
    fn set_len(&mut self, len: u64) -> io::Result<()> {
        self.0.set_len(len)
    }
    fn sync(&mut self) -> io::Result<()> {
        self.0.sync_all()
    }
}

fn millis(time: io::Result<SystemTime>) -> i64 {
    time.ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis() as i64)
}

impl FileSystem for HostFs {
    fn metadata(&self, path: &str) -> io::Result<Metadata> {
        let meta = std::fs::metadata(self.path(path))?;
        Ok(Metadata {
            kind: if meta.is_dir() { FileKind::Dir } else { FileKind::File },
            len: meta.len(),
            modified: millis(meta.modified()),
            readonly: meta.permissions().readonly(),
        })
    }

    fn open(&self, path: &str, o: &OpenOptions) -> io::Result<Box<dyn File>> {
        let file = std::fs::OpenOptions::new()
            .read(o.read)
            .write(o.write)
            .append(o.append)
            .create(o.create)
            .create_new(o.create_new)
            .truncate(o.truncate)
            .open(self.path(path))?;
        if file.metadata()?.is_dir() && o.write {
            return Err(io::Error::new(io::ErrorKind::IsADirectory, "Is a directory"));
        }
        Ok(Box::new(HostFile(file)))
    }

    fn read_dir(&self, path: &str) -> io::Result<Vec<String>> {
        std::fs::read_dir(self.path(path))?.map(|e| Ok(e?.file_name().to_string_lossy().into_owned())).collect()
    }

    fn create_dir(&self, path: &str) -> io::Result<()> {
        std::fs::create_dir(self.path(path))
    }

    fn remove_file(&self, path: &str) -> io::Result<()> {
        std::fs::remove_file(self.path(path))
    }

    fn remove_dir(&self, path: &str) -> io::Result<()> {
        std::fs::remove_dir(self.path(path))
    }

    fn rename(&self, from: &str, to: &str) -> io::Result<()> {
        std::fs::rename(self.path(from), self.path(to))
    }

    fn set_modified(&self, path: &str, millis: i64) -> io::Result<()> {
        let file = std::fs::File::options().write(true).open(self.path(path))?;
        file.set_modified(UNIX_EPOCH + Duration::from_millis(millis.max(0) as u64))
    }
}

// --- in-memory file system --------------------------------------------------------------

/// A simple in-memory file system (tests; browser scratch space).
#[derive(Default)]
pub struct MemFs {
    nodes: Mutex<BTreeMap<String, MemNode>>,
}

enum MemNode {
    Dir { modified: i64 },
    File { data: Arc<Mutex<Vec<u8>>>, modified: i64 },
}

fn now_millis() -> i64 {
    millis(Ok(SystemTime::now()))
}

impl MemFs {
    pub fn new() -> Self {
        let fs = MemFs::default();
        fs.nodes.lock().unwrap().insert("/".into(), MemNode::Dir { modified: now_millis() });
        fs
    }

    fn parent_exists(nodes: &BTreeMap<String, MemNode>, path: &str) -> bool {
        let parent = path.rsplit_once('/').map_or("/", |(p, _)| if p.is_empty() { "/" } else { p });
        matches!(nodes.get(parent), Some(MemNode::Dir { .. }))
    }
}

struct MemFile {
    data: Arc<Mutex<Vec<u8>>>,
    pos: u64,
    append: bool,
}

impl File for MemFile {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let data = self.data.lock().unwrap();
        let start = (self.pos as usize).min(data.len());
        let n = buf.len().min(data.len() - start);
        buf[..n].copy_from_slice(&data[start..start + n]);
        self.pos += n as u64;
        Ok(n)
    }
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut data = self.data.lock().unwrap();
        if self.append {
            self.pos = data.len() as u64;
        }
        let end = self.pos as usize + buf.len();
        if data.len() < end {
            data.resize(end, 0);
        }
        data[self.pos as usize..end].copy_from_slice(buf);
        self.pos = end as u64;
        Ok(buf.len())
    }
    fn seek(&mut self, pos: u64) -> io::Result<()> {
        self.pos = pos;
        Ok(())
    }
    fn position(&mut self) -> io::Result<u64> {
        Ok(self.pos)
    }
    fn len(&mut self) -> io::Result<u64> {
        Ok(self.data.lock().unwrap().len() as u64)
    }
    fn set_len(&mut self, len: u64) -> io::Result<()> {
        self.data.lock().unwrap().resize(len as usize, 0);
        Ok(())
    }
    fn sync(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn not_found() -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, "No such file or directory")
}

impl FileSystem for MemFs {
    fn metadata(&self, path: &str) -> io::Result<Metadata> {
        match self.nodes.lock().unwrap().get(path) {
            Some(MemNode::Dir { modified }) => Ok(Metadata { kind: FileKind::Dir, len: 0, modified: *modified, readonly: false }),
            Some(MemNode::File { data, modified }) => {
                Ok(Metadata { kind: FileKind::File, len: data.lock().unwrap().len() as u64, modified: *modified, readonly: false })
            }
            None => Err(not_found()),
        }
    }

    fn open(&self, path: &str, o: &OpenOptions) -> io::Result<Box<dyn File>> {
        let mut nodes = self.nodes.lock().unwrap();
        let data = match nodes.get(path) {
            Some(MemNode::Dir { .. }) => return Err(io::Error::new(io::ErrorKind::IsADirectory, "Is a directory")),
            Some(MemNode::File { .. }) if o.create_new => {
                return Err(io::Error::new(io::ErrorKind::AlreadyExists, "File exists"));
            }
            Some(MemNode::File { data, .. }) => {
                if o.truncate {
                    data.lock().unwrap().clear();
                }
                data.clone()
            }
            None if o.create || o.create_new => {
                if !Self::parent_exists(&nodes, path) {
                    return Err(not_found());
                }
                let data = Arc::new(Mutex::new(Vec::new()));
                nodes.insert(path.to_owned(), MemNode::File { data: data.clone(), modified: now_millis() });
                data
            }
            None => return Err(not_found()),
        };
        Ok(Box::new(MemFile { data, pos: 0, append: o.append }))
    }

    fn read_dir(&self, path: &str) -> io::Result<Vec<String>> {
        let nodes = self.nodes.lock().unwrap();
        if !matches!(nodes.get(path), Some(MemNode::Dir { .. })) {
            return Err(not_found());
        }
        let prefix = if path == "/" { "/".to_owned() } else { format!("{path}/") };
        Ok(nodes
            .range(prefix.clone()..)
            .take_while(|(k, _)| k.starts_with(&prefix))
            .filter_map(|(k, _)| {
                let rest = &k[prefix.len()..];
                (!rest.is_empty() && !rest.contains('/')).then(|| rest.to_owned())
            })
            .collect())
    }

    fn create_dir(&self, path: &str) -> io::Result<()> {
        let mut nodes = self.nodes.lock().unwrap();
        if nodes.contains_key(path) {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, "File exists"));
        }
        if !Self::parent_exists(&nodes, path) {
            return Err(not_found());
        }
        nodes.insert(path.to_owned(), MemNode::Dir { modified: now_millis() });
        Ok(())
    }

    fn remove_file(&self, path: &str) -> io::Result<()> {
        let mut nodes = self.nodes.lock().unwrap();
        match nodes.get(path) {
            Some(MemNode::File { .. }) => {
                nodes.remove(path);
                Ok(())
            }
            _ => Err(not_found()),
        }
    }

    fn remove_dir(&self, path: &str) -> io::Result<()> {
        if !self.read_dir(path)?.is_empty() {
            return Err(io::Error::new(io::ErrorKind::DirectoryNotEmpty, "Directory not empty"));
        }
        self.nodes.lock().unwrap().remove(path);
        Ok(())
    }

    fn rename(&self, from: &str, to: &str) -> io::Result<()> {
        let mut nodes = self.nodes.lock().unwrap();
        if !nodes.contains_key(from) {
            return Err(not_found());
        }
        let moved: Vec<String> = nodes
            .keys()
            .filter(|k| *k == from || k.starts_with(&format!("{from}/")))
            .cloned()
            .collect();
        for key in moved {
            let node = nodes.remove(&key).unwrap();
            nodes.insert(format!("{to}{}", &key[from.len()..]), node);
        }
        Ok(())
    }

    fn set_modified(&self, path: &str, millis: i64) -> io::Result<()> {
        match self.nodes.lock().unwrap().get_mut(path) {
            Some(MemNode::Dir { modified } | MemNode::File { modified, .. }) => {
                *modified = millis;
                Ok(())
            }
            None => Err(not_found()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_paths() {
        assert_eq!(normalize("/a//b/./c/../d"), "/a/b/d");
        assert_eq!(normalize("/.."), "/");
        assert_eq!(normalize("/"), "/");
    }

    #[test]
    fn mounts_and_memory_files() {
        let mut vfs = Vfs::default();
        vfs.mount("/", Arc::new(MemFs::new()));
        vfs.mount("/jdk", Arc::new(MemFs::new()));
        vfs.create_dir("/game").unwrap();
        let mut file = vfs.open("/game/a.txt", &OpenOptions { write: true, create: true, ..Default::default() }).unwrap();
        file.write(b"hello").unwrap();
        assert_eq!(vfs.metadata("/game/a.txt").unwrap().len, 5);
        assert_eq!(vfs.read_dir("/").unwrap(), ["game", "jdk"]);
        vfs.rename("/game", "/world").unwrap();
        let mut file = vfs.open("/world/a.txt", &OpenOptions { read: true, ..Default::default() }).unwrap();
        let mut buf = [0; 8];
        assert_eq!(file.read(&mut buf).unwrap(), 5);
        assert!(vfs.rename("/world/a.txt", "/jdk/a.txt").is_err());
        assert!(vfs.remove("/world").is_err(), "non-empty directory");
    }
}
