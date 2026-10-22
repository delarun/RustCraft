//! Where class bytes come from: the JDK runtime image for the platform
//! classes, then jars and directories on the class path.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub struct ClassBytes {
    pub bytes: Vec<u8>,
    /// Module name for classes from the runtime image.
    pub module: Option<String>,
}

pub enum Source {
    Image(jimage::Image),
    Jar(Mutex<zip::ZipArchive<std::io::Cursor<Vec<u8>>>>),
    Dir(PathBuf),
    /// In-memory classes, keyed by internal name (tests, generated code).
    Memory(HashMap<String, Vec<u8>>),
}

impl Source {
    pub fn image(path: &Path) -> std::io::Result<Source> {
        let image = jimage::Image::new(std::fs::read(path)?).map_err(std::io::Error::other)?;
        Ok(Source::Image(image))
    }

    pub fn jar(path: &Path) -> std::io::Result<Source> {
        Self::jar_bytes(std::fs::read(path)?)
    }

    pub fn jar_bytes(bytes: Vec<u8>) -> std::io::Result<Source> {
        let archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(std::io::Error::other)?;
        Ok(Source::Jar(Mutex::new(archive)))
    }

    /// A jar, a directory, or a runtime image (`lib/modules`), by path.
    pub fn open(path: &Path) -> std::io::Result<Source> {
        if path.is_dir() {
            Ok(Source::Dir(path.to_owned()))
        } else if path.file_name().is_some_and(|n| n == "modules") {
            Self::image(path)
        } else {
            Self::jar(path)
        }
    }

    pub fn find_class(&self, internal_name: &str) -> Option<ClassBytes> {
        match self {
            Source::Image(image) => {
                let (module, bytes) = image.find_class(internal_name).ok()??;
                Some(ClassBytes { bytes, module: Some(module) })
            }
            Source::Jar(jar) => {
                let mut jar = jar.lock().unwrap();
                let mut entry = jar.by_name(&format!("{internal_name}.class")).ok()?;
                let mut bytes = Vec::with_capacity(entry.size() as usize);
                entry.read_to_end(&mut bytes).ok()?;
                Some(ClassBytes { bytes, module: None })
            }
            Source::Dir(dir) => {
                let bytes = std::fs::read(dir.join(format!("{internal_name}.class"))).ok()?;
                Some(ClassBytes { bytes, module: None })
            }
            Source::Memory(classes) => {
                classes.get(internal_name).map(|bytes| ClassBytes { bytes: bytes.clone(), module: None })
            }
        }
    }
}

#[derive(Default)]
pub struct ClassPath {
    pub sources: Vec<Source>,
}

impl ClassPath {
    pub fn push(&mut self, source: Source) {
        self.sources.push(source);
    }

    pub fn find_class(&self, internal_name: &str) -> Option<ClassBytes> {
        self.sources.iter().find_map(|s| s.find_class(internal_name))
    }

    pub fn image(&self) -> Option<&jimage::Image> {
        self.sources.iter().find_map(|s| match s {
            Source::Image(image) => Some(image),
            _ => None,
        })
    }
}
