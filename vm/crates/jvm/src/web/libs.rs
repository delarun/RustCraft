//! `dlopen`/`dlsym` over the libraries the browser build provides: each is a
//! name pattern and a symbol table of functions linked into the VM.

use std::sync::Mutex;

struct Library {
    /// The file name starts with one of these.
    names: &'static [&'static str],
    /// The function's address (table index), if provided.
    symbol: fn(&str) -> Option<usize>,
}

const LIBRARIES: &[Library] = &[
    // LWJGL's JNI libraries: their natives are the VM's.
    Library { names: &["liblwjgl"], symbol: |_| None },
    Library { names: &["libc.", "libm.", "libdl."], symbol: libc_symbol },
    Library { names: &["libjemalloc"], symbol: jemalloc_symbol },
    Library { names: &["libSDL3"], symbol: |name| super::sdl::symbol(name).or_else(|| missing(name)) },
    Library { names: &["libGL.", "libGLX.", "libOpenGL."], symbol: super::gl::glx_symbol },
    Library { names: &["libopenal"], symbol: |name| super::al::symbol(name).or_else(|| missing(name)) },
    Library { names: &["libshaderc"], symbol: c_symbol },
    // Loaded at startup but unused by Minecraft so far: entry points that
    // only exist (calling one panics).
    Library { names: &["libfreetype"], symbol: |_| Some(not_provided as *const () as usize) },
    Library { names: &["libspirv-cross"], symbol: super::shaders::spirv_cross_symbol },
];

unsafe extern "C" {
    /// native/c_symbols.c: the C libraries' functions by name.
    fn rjava_c_symbol(name: *const std::ffi::c_char) -> usize;
}

/// A function of the C libraries linked into the VM.
pub(super) fn c_symbol(name: &str) -> Option<usize> {
    let name = std::ffi::CString::new(name).ok()?;
    // SAFETY: a lookup in a static table.
    match unsafe { rjava_c_symbol(name.as_ptr()) } {
        0 => None,
        address => Some(address),
    }
}

/// A function the library has but the browser build does not implement:
/// LWJGL binds every function of a class at once, so it gets an address
/// (logged; calling it panics, see RJAVA_WEB_TRACE for which).
fn missing(name: &str) -> Option<usize> {
    if super::tracing() {
        eprintln!("[web] not implemented: {name}");
    }
    Some(not_provided as *const () as usize)
}

/// Whether `address` stands for a function that is not implemented.
pub(super) fn is_placeholder(address: usize) -> bool {
    address == not_provided as *const () as usize || address == super::gl::not_callable_address()
}

extern "C" fn not_provided() {
    panic!("a function of a library the browser build only pretends to provide was called");
}

static ERROR: Mutex<Option<String>> = Mutex::new(None);
/// Paths opened, by handle - 1.
static OPENED: Mutex<Vec<(String, usize)>> = Mutex::new(Vec::new());

/// A handle for `path` (0 if the library is not provided).
pub(super) fn open(path: &str) -> u32 {
    let name = path.rsplit('/').next().unwrap_or(path);
    let Some(index) = LIBRARIES.iter().position(|l| l.names.iter().any(|n| name.starts_with(n))) else {
        *ERROR.lock().unwrap() = Some(format!("{path}: not available in the browser"));
        return 0;
    };
    let mut opened = OPENED.lock().unwrap();
    opened.push((path.to_owned(), index));
    opened.len() as u32
}

/// Names of the functions dlsym handed out, by address (for tracing).
static NAMES: Mutex<Option<std::collections::HashMap<usize, String>>> = Mutex::new(None);

/// Notes the name of an address handed out (for tracing).
pub(super) fn remember(address: usize, name: &str) {
    if super::tracing() && address != 0 {
        NAMES.lock().unwrap().get_or_insert_with(Default::default).entry(address).or_insert_with(|| name.to_owned());
    }
}

pub(super) fn name_of(address: usize) -> Option<String> {
    NAMES.lock().unwrap().as_ref()?.get(&address).cloned()
}

pub(super) fn symbol(handle: u32, name: &str) -> usize {
    let opened = OPENED.lock().unwrap();
    let Some((path, index)) = opened.get((handle as usize).wrapping_sub(1)) else { return 0 };
    match (LIBRARIES[*index].symbol)(name) {
        Some(address) => {
            if super::tracing() {
                NAMES.lock().unwrap().get_or_insert_with(Default::default).entry(address).or_insert_with(|| name.to_owned());
            }
            address
        }
        None => {
            eprintln!("[web] {path}: no symbol {name}");
            0
        }
    }
}

pub(super) fn path(handle: u32) -> String {
    OPENED.lock().unwrap().get((handle as usize).wrapping_sub(1)).map(|(p, _)| p.clone()).unwrap_or_default()
}

/// The last error as a C string (0 if none), as dlerror.
pub(super) fn error() -> u32 {
    match ERROR.lock().unwrap().take() {
        Some(message) => super::leak_c_str(&message),
        None => 0,
    }
}

fn libc_symbol(name: &str) -> Option<usize> {
    Some(match name {
        "malloc" => super::malloc as *const () as usize,
        "calloc" => super::calloc as *const () as usize,
        "realloc" => super::realloc as *const () as usize,
        "free" => super::free as *const () as usize,
        "aligned_alloc" => super::aligned_alloc as *const () as usize,
        _ => return None,
    })
}

fn jemalloc_symbol(name: &str) -> Option<usize> {
    libc_symbol(name.strip_prefix("je_").unwrap_or(name))
}
