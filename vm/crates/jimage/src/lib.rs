//! Reader for the JDK runtime image `lib/modules` (the "jimage" format, see
//! `jdk.internal.jimage.BasicImageReader` in OpenJDK).
//!
//! Layout: header, redirect table, offsets table, location attributes, string
//! table, then resource bytes. Resources are named `/<module>/<path>`, e.g.
//! `/java.base/java/lang/Object.class`.

use std::fmt;

const MAGIC: u32 = 0xCAFE_DADA;
const HEADER_SIZE: usize = 7 * 4;
const HASH_MULTIPLIER: u32 = 0x0100_0193;

const ATTRIBUTE_END: u8 = 0;
const ATTRIBUTE_MODULE: u8 = 1;
const ATTRIBUTE_PARENT: u8 = 2;
const ATTRIBUTE_BASE: u8 = 3;
const ATTRIBUTE_EXTENSION: u8 = 4;
const ATTRIBUTE_OFFSET: u8 = 5;
const ATTRIBUTE_COMPRESSED: u8 = 6;
const ATTRIBUTE_UNCOMPRESSED: u8 = 7;
const ATTRIBUTE_COUNT: usize = 8;

const COMPRESSED_MAGIC: u32 = 0xCAFE_FAFA;
const COMPRESSED_HEADER_SIZE: usize = 29;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageError(pub String);

impl fmt::Display for ImageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "jimage: {}", self.0)
    }
}

impl std::error::Error for ImageError {}

pub type Result<T> = std::result::Result<T, ImageError>;

fn err<T>(message: impl Into<String>) -> Result<T> {
    Err(ImageError(message.into()))
}

pub struct Image {
    data: Box<[u8]>,
    /// Byte order of the image; it is written in the build machine's native order.
    big_endian: bool,
    table_length: usize,
    redirect_start: usize,
    offsets_start: usize,
    locations_start: usize,
    strings_start: usize,
    index_size: usize,
}

/// A resource's location attributes.
#[derive(Debug, Clone, Copy)]
pub struct Location {
    attributes: [u64; ATTRIBUTE_COUNT],
}

impl Image {
    pub fn new(data: impl Into<Box<[u8]>>) -> Result<Image> {
        let data = data.into();
        if data.len() < HEADER_SIZE {
            return err("file too small");
        }
        let big_endian = match u32::from_le_bytes(data[0..4].try_into().unwrap()) {
            MAGIC => false,
            m if m.swap_bytes() == MAGIC => true,
            m => return err(format!("bad magic {m:#010x}")),
        };
        let mut image = Image {
            data,
            big_endian,
            table_length: 0,
            redirect_start: 0,
            offsets_start: 0,
            locations_start: 0,
            strings_start: 0,
            index_size: 0,
        };
        let version = image.u32_at(4);
        if version >> 16 != 1 {
            return err(format!("unsupported version {}.{}", version >> 16, version & 0xFFFF));
        }
        let table_length = image.u32_at(16) as usize;
        let locations_size = image.u32_at(20) as usize;
        let strings_size = image.u32_at(24) as usize;
        image.table_length = table_length;
        image.redirect_start = HEADER_SIZE;
        image.offsets_start = image.redirect_start + table_length * 4;
        image.locations_start = image.offsets_start + table_length * 4;
        image.strings_start = image.locations_start + locations_size;
        image.index_size = image.strings_start + strings_size;
        if image.index_size > image.data.len() {
            return err("index extends past end of file");
        }
        Ok(image)
    }

    /// The whole image file.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    pub fn resource_count(&self) -> usize {
        self.u32_at(12) as usize
    }

    fn u32_at(&self, at: usize) -> u32 {
        let bytes: [u8; 4] = self.data[at..at + 4].try_into().unwrap();
        if self.big_endian { u32::from_be_bytes(bytes) } else { u32::from_le_bytes(bytes) }
    }

    /// Looks up a resource by full name, e.g. `/java.base/java/lang/Object.class`.
    pub fn find(&self, name: &str) -> Option<Location> {
        if self.table_length == 0 {
            return None;
        }
        let count = self.table_length as u32;
        let redirect = self.u32_at(self.redirect_start + (hash(name, HASH_MULTIPLIER) % count) as usize * 4) as i32;
        let index = match redirect {
            0 => return None,
            r if r < 0 => (-r - 1) as u32,
            r => hash(name, r as u32) % count,
        };
        let location = self.location(index as usize).ok()?;
        (self.location_name(&location).ok()? == name).then_some(location)
    }

    /// Reads a resource by name; `None` if it does not exist.
    pub fn read(&self, name: &str) -> Result<Option<Vec<u8>>> {
        match self.find(name) {
            Some(location) => self.read_location(&location).map(Some),
            None => Ok(None),
        }
    }

    /// Finds a class in whichever module defines it, given its internal name
    /// (`java/lang/Object`). Returns the module name and the class bytes.
    pub fn find_class(&self, internal_name: &str) -> Result<Option<(String, Vec<u8>)>> {
        let package = internal_name.rsplit_once('/').map_or("", |(p, _)| p);
        for module in self.package_modules(package)? {
            if let Some(bytes) = self.read(&format!("/{module}/{internal_name}.class"))? {
                return Ok(Some((module, bytes)));
            }
        }
        Ok(None)
    }

    /// Modules containing a package. `/packages/<pkg.with.dots>` holds (isEmpty,
    /// moduleNameOffset) pairs of u32s in the image's byte order.
    pub fn package_modules(&self, package: &str) -> Result<Vec<String>> {
        let Some(bytes) = self.read(&format!("/packages/{}", package.replace('/', ".")))? else {
            return Ok(Vec::new());
        };
        bytes
            .chunks_exact(8)
            .filter(|entry| self.u32_of(&entry[0..4]) == 0)
            .map(|entry| self.string(self.u32_of(&entry[4..8]) as usize).map(str::to_owned))
            .collect()
    }

    /// All resource names, in table order.
    pub fn names(&self) -> impl Iterator<Item = Result<String>> + '_ {
        (0..self.table_length).map(|i| self.location(i).and_then(|l| self.location_name(&l)))
    }

    pub fn read_location(&self, location: &Location) -> Result<Vec<u8>> {
        let offset = self.index_size as u64 + location.attributes[ATTRIBUTE_OFFSET as usize];
        let compressed = location.attributes[ATTRIBUTE_COMPRESSED as usize];
        let uncompressed = location.attributes[ATTRIBUTE_UNCOMPRESSED as usize];
        let stored = if compressed != 0 { compressed } else { uncompressed };
        let end = offset.checked_add(stored).filter(|&e| e <= self.data.len() as u64);
        let Some(end) = end else { return err("resource extends past end of file") };
        let mut bytes = self.data[offset as usize..end as usize].to_vec();
        if compressed != 0 {
            // Compression plugins may be stacked; each layer has its own header.
            while bytes.len() >= COMPRESSED_HEADER_SIZE && self.u32_of(&bytes[0..4]) == COMPRESSED_MAGIC {
                bytes = self.decompress_layer(&bytes)?;
            }
            if bytes.len() as u64 != uncompressed {
                return err("decompressed size mismatch");
            }
        }
        Ok(bytes)
    }

    fn u32_of(&self, bytes: &[u8]) -> u32 {
        let bytes: [u8; 4] = bytes.try_into().unwrap();
        if self.big_endian { u32::from_be_bytes(bytes) } else { u32::from_le_bytes(bytes) }
    }

    fn u64_of(&self, bytes: &[u8]) -> u64 {
        let bytes: [u8; 8] = bytes.try_into().unwrap();
        if self.big_endian { u64::from_be_bytes(bytes) } else { u64::from_le_bytes(bytes) }
    }

    fn decompress_layer(&self, bytes: &[u8]) -> Result<Vec<u8>> {
        let compressed_size = self.u64_of(&bytes[4..12]) as usize;
        let uncompressed_size = self.u64_of(&bytes[12..20]) as usize;
        let decompressor = self.string(self.u32_of(&bytes[20..24]) as usize)?;
        let payload = bytes
            .get(COMPRESSED_HEADER_SIZE..COMPRESSED_HEADER_SIZE + compressed_size)
            .ok_or_else(|| ImageError("truncated compressed resource".into()))?;
        let out = match decompressor {
            "zip" => miniz_oxide::inflate::decompress_to_vec_zlib(payload)
                .map_err(|e| ImageError(format!("inflate failed: {e:?}")))?,
            other => return err(format!("unsupported decompressor {other:?}")),
        };
        if out.len() != uncompressed_size {
            return err("decompressed layer size mismatch");
        }
        Ok(out)
    }

    fn location(&self, index: usize) -> Result<Location> {
        if index >= self.table_length {
            return err("location index out of range");
        }
        let mut pos = self.locations_start + self.u32_at(self.offsets_start + index * 4) as usize;
        let mut attributes = [0u64; ATTRIBUTE_COUNT];
        loop {
            let Some(&byte) = self.data.get(pos).filter(|_| pos < self.strings_start) else {
                return err("unterminated location attributes");
            };
            let kind = byte >> 3;
            if kind == ATTRIBUTE_END {
                break;
            }
            if kind as usize >= ATTRIBUTE_COUNT {
                return err(format!("unknown location attribute {kind}"));
            }
            let len = (byte & 7) as usize + 1;
            let value = self.data[pos + 1..pos + 1 + len].iter().fold(0u64, |v, &b| (v << 8) | b as u64);
            attributes[kind as usize] = value;
            pos += 1 + len;
        }
        Ok(Location { attributes })
    }

    fn location_name(&self, location: &Location) -> Result<String> {
        let part = |kind: u8| self.string(location.attributes[kind as usize] as usize);
        let module = part(ATTRIBUTE_MODULE)?;
        let parent = part(ATTRIBUTE_PARENT)?;
        let base = part(ATTRIBUTE_BASE)?;
        let extension = part(ATTRIBUTE_EXTENSION)?;
        let mut name = String::with_capacity(module.len() + parent.len() + base.len() + extension.len() + 4);
        if !module.is_empty() {
            name.push('/');
            name.push_str(module);
            name.push('/');
        }
        if !parent.is_empty() {
            name.push_str(parent);
            name.push('/');
        }
        name.push_str(base);
        if !extension.is_empty() {
            name.push('.');
            name.push_str(extension);
        }
        Ok(name)
    }

    /// NUL-terminated (modified UTF-8) string at an offset into the string table.
    fn string(&self, offset: usize) -> Result<&str> {
        let start = self.strings_start + offset;
        let table = self.data.get(start..self.index_size).ok_or_else(|| ImageError("bad string offset".into()))?;
        let len = table.iter().position(|&b| b == 0).ok_or_else(|| ImageError("unterminated string".into()))?;
        std::str::from_utf8(&table[..len]).map_err(|_| ImageError("non-UTF-8 string".into()))
    }
}

/// `ImageStringsReader.unmaskedHashCode`, masked to a positive int. Names are
/// hashed over their (modified) UTF-8 bytes.
fn hash(name: &str, seed: u32) -> u32 {
    let mut h = seed;
    for &b in name.as_bytes() {
        h = h.wrapping_mul(HASH_MULTIPLIER) ^ b as u32;
    }
    h & 0x7FFF_FFFF
}
