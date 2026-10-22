//! SDL3, as far as Minecraft uses it: windows are canvases (an OffscreenCanvas
//! with a WebGL2 context in the thread that renders, shown by the page),
//! input comes from the page as events, displays are the screen.
//!
//! The page's side is launcher/src/jvm/display.ts; this thread's side the
//! `rjava_web` imports (launcher/src/jvm/platform.ts).

use std::ffi::c_char;
use std::sync::Mutex;
use std::time::Instant;

use super::{c_str, calloc, leak_c_str, malloc};

#[cfg_attr(not(rjava_stub), link(wasm_import_module = "rjava_web"))]
unsafe extern "C" {
    /// Creates canvas `id` (a window) of `width`×`height` CSS pixels.
    fn window_create(id: u32, width: i32, height: i32, visible: bool);
    fn window_destroy(id: u32);
    fn window_show(id: u32, visible: bool);
    fn window_title(id: u32, title: *const u8, length: usize);
    /// The page's state: [CSS width, CSS height, pixel width, pixel height,
    /// device pixel ratio × 1000, focused, pointer locked, screen width,
    /// screen height].
    fn page_state(out: *mut i32);
    /// The next input event (8 words, see `next_event`); false if none.
    fn event_next(out: *mut i32) -> bool;
    /// Creates a WebGL2 context (on a canvas of its own) for drawing into
    /// window `window`; returns its id, 0 if unavailable.
    fn gl_context_create(window: u32) -> u32;
    /// Makes `context` current on this thread, drawing into `window` (GL lets
    /// a context draw into any compatible window); 0 for none.
    fn gl_make_current(window: u32, context: u32);
    /// Shows what the context drawing into window `id` drew since the last call.
    fn window_present(id: u32);
    fn cursor_set(kind: i32);
    fn mouse_relative(enabled: bool);
    fn clipboard_set(text: *const u8, length: usize);
    /// Opens a URL in a new browser tab.
    fn open_url(url: *const u8, length: usize);
}

/// The C type of SDL's windows here.
#[repr(C)]
struct Window {
    id: u32,
}

struct State {
    error: String,
    error_c: u32,
    gl_attributes: [i32; 28],
    /// Live windows: id and the handle given out for it (handles are
    /// compared, so each window has exactly one).
    windows: Vec<(u32, usize)>,
    current_window: u32,
    keys: [bool; 512],
    mods: u16,
    mouse: (f32, f32, u32),
    relative: bool,
    swap_interval: i32,
}

static STATE: Mutex<State> = Mutex::new(State {
    error: String::new(),
    error_c: 0,
    gl_attributes: [0; 28],
    windows: Vec::new(),
    current_window: 0,
    keys: [false; 512],
    mods: 0,
    mouse: (0.0, 0.0, 0),
    relative: false,
    swap_interval: 1,
});

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap()
}

fn start() -> Instant {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    *START.get_or_init(Instant::now)
}

fn ticks_ns() -> u64 {
    start().elapsed().as_nanos() as u64
}

fn page() -> [i32; 9] {
    let mut out = [0; 9];
    // SAFETY: the import writes 9 words.
    unsafe { page_state(out.as_mut_ptr()) };
    out
}

fn set_error(message: &str) -> bool {
    state().error = message.to_owned();
    false
}

fn window_id(window: *const Window) -> u32 {
    if window.is_null() { 0 } else { unsafe { (*window).id } }
}

// --- init, hints, errors, logging ------------------------------------------

extern "C" fn sdl_init(_flags: u32) -> bool {
    start();
    true
}
extern "C" fn sdl_quit() {}
extern "C" fn sdl_quit_sub_system(_flags: u32) {}
extern "C" fn sdl_was_init(flags: u32) -> u32 {
    flags
}
extern "C" fn sdl_set_hint(_name: *const c_char, _value: *const c_char) -> bool {
    true
}
extern "C" fn sdl_get_hint(_name: *const c_char) -> *const c_char {
    std::ptr::null()
}
extern "C" fn sdl_set_app_metadata_property(_name: *const c_char, _value: *const c_char) -> bool {
    true
}
extern "C" fn sdl_set_log_output_function(_callback: u32, _userdata: u32) {}
extern "C" fn sdl_set_log_priorities(_priority: i32) {}
extern "C" fn sdl_get_error() -> *const c_char {
    let mut state = state();
    let message = std::mem::take(&mut state.error);
    if state.error_c != 0 {
        unsafe { super::free(state.error_c as usize as *mut std::ffi::c_void) };
    }
    let bytes = format!("{message}\0");
    // SAFETY: a malloc'd copy, valid until the next call.
    let copy = unsafe { malloc(bytes.len()) } as *mut u8;
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), copy, bytes.len()) };
    state.error_c = copy as u32;
    state.error = message;
    copy as *const c_char
}
extern "C" fn sdl_clear_error() -> bool {
    state().error.clear();
    true
}
extern "C" fn sdl_free(pointer: *mut std::ffi::c_void) {
    unsafe { super::free(pointer) }
}
extern "C" fn sdl_get_version() -> i32 {
    3_004_018
}
extern "C" fn sdl_get_revision() -> *const c_char {
    static REVISION: &[u8] = b"rjava-web\0";
    REVISION.as_ptr() as *const c_char
}
extern "C" fn sdl_get_platform() -> *const c_char {
    static PLATFORM: &[u8] = b"Emscripten\0";
    PLATFORM.as_ptr() as *const c_char
}
extern "C" fn sdl_get_system_theme() -> i32 {
    0
}
extern "C" fn sdl_get_current_video_driver() -> *const c_char {
    static DRIVER: &[u8] = b"browser\0";
    DRIVER.as_ptr() as *const c_char
}

// --- time ------------------------------------------------------------------

extern "C" fn sdl_get_ticks_ns() -> u64 {
    ticks_ns()
}
extern "C" fn sdl_get_ticks() -> u64 {
    ticks_ns() / 1_000_000
}
extern "C" fn sdl_get_performance_counter() -> u64 {
    ticks_ns()
}
extern "C" fn sdl_get_performance_frequency() -> u64 {
    1_000_000_000
}
extern "C" fn sdl_delay_ns(ns: u64) {
    std::thread::sleep(std::time::Duration::from_nanos(ns));
}
extern "C" fn sdl_delay(ms: u32) {
    std::thread::sleep(std::time::Duration::from_millis(ms as u64));
}

// --- displays --------------------------------------------------------------

const DISPLAY: u32 = 1;
const PIXELFORMAT_XRGB8888: u32 = 0x1616_1804;

#[repr(C)]
struct DisplayMode {
    display_id: u32,
    format: u32,
    w: i32,
    h: i32,
    pixel_density: f32,
    refresh_rate: f32,
    refresh_rate_numerator: i32,
    refresh_rate_denominator: i32,
    internal: u32,
}

fn display_mode() -> DisplayMode {
    let page = page();
    DisplayMode {
        display_id: DISPLAY,
        format: PIXELFORMAT_XRGB8888,
        w: page[7].max(1),
        h: page[8].max(1),
        pixel_density: 1.0,
        refresh_rate: 60.0,
        refresh_rate_numerator: 60,
        refresh_rate_denominator: 1,
        internal: 0,
    }
}

/// A display mode in memory that stays valid (SDL owns it).
fn static_mode() -> *const DisplayMode {
    static MODE: Mutex<u32> = Mutex::new(0);
    let mut mode = MODE.lock().unwrap();
    if *mode == 0 {
        *mode = Box::leak(Box::new(display_mode())) as *const DisplayMode as u32;
    } else {
        // SAFETY: the leaked mode.
        unsafe { *(*mode as usize as *mut DisplayMode) = display_mode() };
    }
    *mode as usize as *const DisplayMode
}

extern "C" fn sdl_get_displays(count: *mut i32) -> *mut u32 {
    // SAFETY: a malloc'd, 0-terminated array the caller frees.
    unsafe {
        let ids = calloc(2, 4) as *mut u32;
        *ids = DISPLAY;
        if !count.is_null() {
            *count = 1;
        }
        ids
    }
}
extern "C" fn sdl_get_primary_display() -> u32 {
    DISPLAY
}
extern "C" fn sdl_get_display_for_window(_window: *const Window) -> u32 {
    DISPLAY
}
extern "C" fn sdl_get_display_name(_display: u32) -> *const c_char {
    static NAME: &[u8] = b"Browser\0";
    NAME.as_ptr() as *const c_char
}
#[repr(C)]
struct Rect {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}
extern "C" fn sdl_get_display_bounds(_display: u32, rect: *mut Rect) -> bool {
    let page = page();
    // SAFETY: the caller's rect.
    unsafe { *rect = Rect { x: 0, y: 0, w: page[7], h: page[8] } };
    true
}
extern "C" fn sdl_get_desktop_display_mode(_display: u32) -> *const DisplayMode {
    static_mode()
}
extern "C" fn sdl_get_current_display_mode(_display: u32) -> *const DisplayMode {
    static_mode()
}
extern "C" fn sdl_get_fullscreen_display_modes(_display: u32, count: *mut i32) -> *mut u32 {
    // SAFETY: one malloc'd block: a null-terminated pointer array, then the mode.
    unsafe {
        let block = calloc(1, 8 + std::mem::size_of::<DisplayMode>()) as *mut u32;
        let mode = block.add(2) as *mut DisplayMode;
        *mode = display_mode();
        *block = mode as u32;
        if !count.is_null() {
            *count = 1;
        }
        block
    }
}
extern "C" fn sdl_get_display_content_scale(_display: u32) -> f32 {
    page()[4] as f32 / 1000.0
}

// --- windows ---------------------------------------------------------------

const WINDOW_OPENGL: u64 = 0x2;
const WINDOW_HIDDEN: u64 = 0x8;
const WINDOW_RESIZABLE: u64 = 0x20;
const WINDOW_INPUT_FOCUS: u64 = 0x200;
const WINDOW_MOUSE_FOCUS: u64 = 0x400;
const WINDOW_HIGH_PIXEL_DENSITY: u64 = 0x2000;
const WINDOW_MOUSE_RELATIVE_MODE: u64 = 0x8000;

extern "C" fn sdl_create_window(title: *const c_char, w: i32, h: i32, flags: u64) -> *mut Window {
    let id = {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    };
    // SAFETY: imports with valid arguments.
    unsafe {
        window_create(id, w, h, flags & WINDOW_HIDDEN == 0);
        let title = c_str(title as u32);
        window_title(id, title.as_ptr(), title.len());
    }
    let window = Box::leak(Box::new(Window { id }));
    state().windows.push((id, window as *mut Window as usize));
    window
}

/// The handle of window `id` (null if there is none).
fn window_handle(id: u32) -> *mut Window {
    state().windows.iter().find(|w| w.0 == id).map_or(std::ptr::null_mut(), |w| w.1 as *mut Window)
}
extern "C" fn sdl_destroy_window(window: *mut Window) {
    let id = window_id(window);
    state().windows.retain(|w| w.0 != id);
    unsafe { window_destroy(id) };
}
extern "C" fn sdl_get_window_id(window: *const Window) -> u32 {
    window_id(window)
}
extern "C" fn sdl_get_window_from_id(id: u32) -> *mut Window {
    window_handle(id)
}
extern "C" fn sdl_get_window_flags(_window: *const Window) -> u64 {
    let page = page();
    let mut flags = WINDOW_OPENGL | WINDOW_RESIZABLE | WINDOW_HIGH_PIXEL_DENSITY;
    if page[5] != 0 {
        flags |= WINDOW_INPUT_FOCUS | WINDOW_MOUSE_FOCUS;
    }
    if state().relative {
        flags |= WINDOW_MOUSE_RELATIVE_MODE;
    }
    flags
}
extern "C" fn sdl_set_window_title(window: *mut Window, title: *const c_char) -> bool {
    let title = c_str(title as u32);
    unsafe { window_title(window_id(window), title.as_ptr(), title.len()) };
    true
}
extern "C" fn sdl_get_window_size(_window: *const Window, w: *mut i32, h: *mut i32) -> bool {
    let page = page();
    write(w, page[0]);
    write(h, page[1]);
    true
}
extern "C" fn sdl_get_window_size_in_pixels(_window: *const Window, w: *mut i32, h: *mut i32) -> bool {
    let page = page();
    write(w, page[2]);
    write(h, page[3]);
    true
}
extern "C" fn sdl_get_window_pixel_density(_window: *const Window) -> f32 {
    let page = page();
    if page[0] > 0 { page[2] as f32 / page[0] as f32 } else { 1.0 }
}
extern "C" fn sdl_get_window_display_scale(_window: *const Window) -> f32 {
    page()[4] as f32 / 1000.0
}
extern "C" fn sdl_get_window_position(_window: *const Window, x: *mut i32, y: *mut i32) -> bool {
    write(x, 0);
    write(y, 0);
    true
}
extern "C" fn sdl_window_noop(_window: *mut Window) -> bool {
    true
}
extern "C" fn sdl_window_noop_ii(_window: *mut Window, _a: i32, _b: i32) -> bool {
    true
}
extern "C" fn sdl_window_noop_p(_window: *mut Window, _p: u32) -> bool {
    true
}
extern "C" fn sdl_show_window(window: *mut Window) -> bool {
    unsafe { window_show(window_id(window), true) };
    true
}
extern "C" fn sdl_hide_window(window: *mut Window) -> bool {
    unsafe { window_show(window_id(window), false) };
    true
}
extern "C" fn sdl_set_window_fullscreen(_window: *mut Window, _fullscreen: bool) -> bool {
    true
}
extern "C" fn sdl_get_window_fullscreen_mode(_window: *const Window) -> *const DisplayMode {
    std::ptr::null()
}
extern "C" fn sdl_get_window_properties(_window: *const Window) -> u32 {
    1
}
extern "C" fn sdl_get_number_property(_props: u32, _name: *const c_char, default: i64) -> i64 {
    default
}

fn write(p: *mut i32, v: i32) {
    if !p.is_null() {
        // SAFETY: the caller's out parameter.
        unsafe { *p = v };
    }
}

// --- surfaces (window icons, cursors) --------------------------------------

#[repr(C)]
struct Surface {
    flags: u32,
    format: u32,
    w: i32,
    h: i32,
    pitch: i32,
    pixels: u32,
    refcount: i32,
    reserved: u32,
}

extern "C" fn sdl_create_surface_from(w: i32, h: i32, format: u32, pixels: u32, pitch: i32) -> *mut Surface {
    Box::leak(Box::new(Surface { flags: 0, format, w, h, pitch, pixels, refcount: 1, reserved: 0 }))
}
extern "C" fn sdl_destroy_surface(surface: *mut Surface) {
    if !surface.is_null() {
        // SAFETY: made by sdl_create_surface_from.
        drop(unsafe { Box::from_raw(surface) });
    }
}
extern "C" fn sdl_add_surface_alternate_image(_surface: *mut Surface, _image: *mut Surface) -> bool {
    true
}

#[repr(C)]
struct PixelFormatDetails {
    format: u32,
    bits_per_pixel: u8,
    bytes_per_pixel: u8,
    padding: [u8; 2],
    masks: [u32; 4],
    bits: [u8; 4],
    shifts: [u8; 4],
}

extern "C" fn sdl_get_pixel_format_details(format: u32) -> *const PixelFormatDetails {
    // Details of 32-bit formats from their layout code: ARGB/RGBA/ABGR/BGRA.
    let order = (format >> 20) & 0xf;
    let shifts: [u8; 4] = match order {
        3 => [16, 8, 0, 24],  // ARGB8888
        4 => [24, 16, 8, 0],  // RGBA8888
        7 => [0, 8, 16, 24],  // ABGR8888
        8 => [8, 16, 24, 0],  // BGRA8888
        _ => [16, 8, 0, 24],
    };
    let details = PixelFormatDetails {
        format,
        bits_per_pixel: 32,
        bytes_per_pixel: 4,
        padding: [0; 2],
        masks: shifts.map(|s| 0xffu32 << s),
        bits: [8; 4],
        shifts,
    };
    Box::leak(Box::new(details))
}

// --- OpenGL ----------------------------------------------------------------

extern "C" fn sdl_gl_load_library(_path: *const c_char) -> bool {
    true
}
extern "C" fn sdl_gl_set_attribute(attribute: i32, value: i32) -> bool {
    if let Some(slot) = state().gl_attributes.get_mut(attribute as usize) {
        *slot = value;
    }
    true
}
extern "C" fn sdl_gl_get_attribute(attribute: i32, value: *mut i32) -> bool {
    // What a WebGL2 context provides.
    let v = match attribute {
        0..=2 => 8,
        3 => 8,
        4 => 32,
        5 => 1,
        6 => 24,
        7 => 8,
        15 => 1,
        17 => 3,
        18 => 3,
        20 => 1, // core profile
        a => state().gl_attributes.get(a as usize).copied().unwrap_or(0),
    };
    write(value, v);
    true
}
extern "C" fn sdl_gl_create_context(window: *mut Window) -> u32 {
    let id = window_id(window);
    // SAFETY: imports.
    let context = unsafe { gl_context_create(id) };
    if context == 0 {
        set_error("WebGL2 is not available");
        return 0;
    }
    unsafe { gl_make_current(id, context) };
    state().current_window = id;
    context
}
extern "C" fn sdl_gl_make_current(window: *mut Window, context: u32) -> bool {
    let id = window_id(window);
    unsafe { gl_make_current(id, context) };
    state().current_window = if context == 0 { 0 } else { id };
    true
}
extern "C" fn sdl_gl_destroy_context(_context: u32) -> bool {
    true
}
extern "C" fn sdl_gl_get_current_window() -> *mut Window {
    let id = state().current_window;
    window_handle(id)
}
extern "C" fn sdl_gl_set_swap_interval(interval: i32) -> bool {
    state().swap_interval = interval;
    true
}
extern "C" fn sdl_gl_get_swap_interval(interval: *mut i32) -> bool {
    write(interval, state().swap_interval);
    true
}
extern "C" fn sdl_gl_swap_window(window: *mut Window) -> bool {
    unsafe { window_present(window_id(window)) };
    true
}
extern "C" fn sdl_gl_get_proc_address(name: *const c_char) -> usize {
    super::gl::proc_address(c_str(name as u32))
}
extern "C" fn sdl_gl_extension_supported(_extension: *const c_char) -> bool {
    false
}

extern "C" fn sdl_get_closest_fullscreen_display_mode(
    _display: u32,
    _w: i32,
    _h: i32,
    _refresh: f32,
    _high_density: bool,
    closest: *mut DisplayMode,
) -> bool {
    if !closest.is_null() {
        // SAFETY: the caller's display mode.
        unsafe { *closest = display_mode() };
    }
    true
}
extern "C" fn sdl_set_window_bordered(_window: *mut Window, _bordered: bool) -> bool {
    true
}
extern "C" fn sdl_gl_unload_library() {}

// --- message boxes, URLs -------------------------------------------------------

extern "C" fn sdl_show_simple_message_box(_flags: u32, title: *const c_char, message: *const c_char, _window: *mut Window) -> bool {
    eprintln!("[SDL message box] {}: {}", c_str(title as u32), c_str(message as u32));
    true
}
/// SDL_MessageBoxData with 32-bit pointers: flags, window, title, message, ...
extern "C" fn sdl_show_message_box(data: *const u32, button: *mut i32) -> bool {
    // SAFETY: the caller's SDL_MessageBoxData.
    let (title, message) = unsafe { (*data.add(2), *data.add(3)) };
    eprintln!("[SDL message box] {}: {}", c_str(title), c_str(message));
    write(button, -1);
    true
}
extern "C" fn sdl_open_url(url: *const c_char) -> bool {
    let url = c_str(url as u32);
    unsafe { open_url(url.as_ptr(), url.len()) };
    true
}

// --- Vulkan: not available --------------------------------------------------

extern "C" fn sdl_vulkan_load_library(_path: *const c_char) -> bool {
    set_error("Vulkan is not available in the browser")
}
extern "C" fn sdl_vulkan_unload_library() {}
extern "C" fn sdl_vulkan_get_instance_extensions(count: *mut u32) -> u32 {
    if !count.is_null() {
        unsafe { *count = 0 };
    }
    0
}
extern "C" fn sdl_vulkan_get_vk_get_instance_proc_addr() -> usize {
    0
}
extern "C" fn sdl_vulkan_create_surface(_window: *mut Window, _instance: u32, _allocator: u32, _surface: u32) -> bool {
    set_error("Vulkan is not available in the browser")
}
extern "C" fn sdl_vulkan_get_presentation_support(_instance: u32, _device: u32, _family: u32) -> bool {
    false
}

// --- events ----------------------------------------------------------------

const EVENT_QUIT: u32 = 0x100;
const EVENT_WINDOW_SHOWN: u32 = 0x202;
const EVENT_WINDOW_RESIZED: u32 = 0x206;
const EVENT_WINDOW_PIXEL_SIZE_CHANGED: u32 = 0x207;
const EVENT_WINDOW_MOUSE_ENTER: u32 = 0x20C;
const EVENT_WINDOW_MOUSE_LEAVE: u32 = 0x20D;
const EVENT_WINDOW_FOCUS_GAINED: u32 = 0x20E;
const EVENT_WINDOW_FOCUS_LOST: u32 = 0x20F;
const EVENT_WINDOW_CLOSE_REQUESTED: u32 = 0x210;
const EVENT_KEY_DOWN: u32 = 0x300;
const EVENT_KEY_UP: u32 = 0x301;
const EVENT_TEXT_INPUT: u32 = 0x303;
const EVENT_MOUSE_MOTION: u32 = 0x400;
const EVENT_MOUSE_BUTTON_DOWN: u32 = 0x401;
const EVENT_MOUSE_BUTTON_UP: u32 = 0x402;
const EVENT_MOUSE_WHEEL: u32 = 0x403;

/// An SDL_Event (128 bytes) being filled.
struct Event([u8; 128]);

impl Event {
    fn new(kind: u32, window: u32) -> Event {
        let mut e = Event([0; 128]);
        e.u32(0, kind);
        e.u64(8, ticks_ns());
        e.u32(16, window);
        e
    }
    fn u8(&mut self, at: usize, v: u8) {
        self.0[at] = v;
    }
    fn u16(&mut self, at: usize, v: u16) {
        self.0[at..at + 2].copy_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, at: usize, v: u32) {
        self.0[at..at + 4].copy_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, at: usize, v: u64) {
        self.0[at..at + 8].copy_from_slice(&v.to_le_bytes());
    }
    fn f32(&mut self, at: usize, v: f32) {
        self.u32(at, v.to_bits());
    }
}

/// Events made from the page's input, not yet polled.
static PENDING: Mutex<std::collections::VecDeque<Event>> = Mutex::new(std::collections::VecDeque::new());

/// The window input goes to: the most recent visible one.
fn input_window() -> u32 {
    state().windows.last().map_or(0, |w| w.0)
}

/// Converts the page's input events (8 words: kind, then per kind) to SDL
/// events and tracks keyboard and mouse state.
fn pump() {
    let mut raw = [0i32; 8];
    let mut pending = PENDING.lock().unwrap();
    // SAFETY: the import writes 8 words.
    while unsafe { event_next(raw.as_mut_ptr()) } {
        let window = input_window();
        if super::tracing() && raw[0] != 3 {
            eprintln!("[web] input event {raw:?} -> window {window}");
        }
        let f = |i: usize| f32::from_bits(raw[i] as u32);
        match raw[0] {
            // Key: scancode, down, repeat, modifiers.
            1 => {
                let scancode = raw[1] as u32;
                let down = raw[2] != 0;
                let mods = raw[4] as u16;
                {
                    let mut state = state();
                    if let Some(k) = state.keys.get_mut(scancode as usize) {
                        *k = down;
                    }
                    state.mods = mods;
                }
                let mut e = Event::new(if down { EVENT_KEY_DOWN } else { EVENT_KEY_UP }, window);
                e.u32(20, 1);
                e.u32(24, scancode);
                e.u32(28, keycode(scancode));
                e.u16(32, mods);
                e.u16(34, scancode as u16);
                e.u8(36, down as u8);
                e.u8(37, (raw[3] != 0) as u8);
                pending.push_back(e);
            }
            // Text: up to 28 UTF-8 bytes in words 1..8.
            2 => {
                let bytes: Vec<u8> = raw[1..].iter().flat_map(|w| w.to_le_bytes()).take_while(|&b| b != 0).collect();
                let text = leak_c_str(&String::from_utf8_lossy(&bytes));
                let mut e = Event::new(EVENT_TEXT_INPUT, window);
                e.u32(20, text);
                pending.push_back(e);
            }
            // Motion: x, y, dx, dy (floats), buttons.
            3 => {
                state().mouse = (f(1), f(2), raw[5] as u32);
                let mut e = Event::new(EVENT_MOUSE_MOTION, window);
                e.u32(20, 1);
                e.u32(24, raw[5] as u32);
                e.f32(28, f(1));
                e.f32(32, f(2));
                e.f32(36, f(3));
                e.f32(40, f(4));
                pending.push_back(e);
            }
            // Button: button, down, clicks, x, y (floats).
            4 => {
                let down = raw[2] != 0;
                {
                    let mut state = state();
                    let bit = 1u32 << (raw[1] - 1);
                    state.mouse.2 = if down { state.mouse.2 | bit } else { state.mouse.2 & !bit };
                }
                let mut e = Event::new(if down { EVENT_MOUSE_BUTTON_DOWN } else { EVENT_MOUSE_BUTTON_UP }, window);
                e.u32(20, 1);
                e.u8(24, raw[1] as u8);
                e.u8(25, down as u8);
                e.u8(26, raw[3] as u8);
                e.f32(28, f(4));
                e.f32(32, f(5));
                pending.push_back(e);
            }
            // Wheel: dx, dy, mouse x, y (floats).
            5 => {
                let mut e = Event::new(EVENT_MOUSE_WHEEL, window);
                e.u32(20, 1);
                e.f32(24, f(1));
                e.f32(28, f(2));
                e.f32(36, f(3));
                e.f32(40, f(4));
                e.u32(44, f(1) as i32 as u32);
                e.u32(48, f(2) as i32 as u32);
                pending.push_back(e);
            }
            // Resize: CSS and pixel sizes.
            6 => {
                let mut e = Event::new(EVENT_WINDOW_RESIZED, window);
                e.u32(20, raw[1] as u32);
                e.u32(24, raw[2] as u32);
                pending.push_back(e);
                let mut e = Event::new(EVENT_WINDOW_PIXEL_SIZE_CHANGED, window);
                e.u32(20, raw[3] as u32);
                e.u32(24, raw[4] as u32);
                pending.push_back(e);
            }
            7 => pending.push_back(Event::new(if raw[1] != 0 { EVENT_WINDOW_FOCUS_GAINED } else { EVENT_WINDOW_FOCUS_LOST }, window)),
            8 => pending.push_back(Event::new(if raw[1] != 0 { EVENT_WINDOW_MOUSE_ENTER } else { EVENT_WINDOW_MOUSE_LEAVE }, window)),
            9 => {
                pending.push_back(Event::new(EVENT_WINDOW_CLOSE_REQUESTED, window));
                pending.push_back(Event::new(EVENT_QUIT, 0));
            }
            10 => pending.push_back(Event::new(EVENT_WINDOW_SHOWN, window)),
            _ => {}
        }
    }
}

extern "C" fn sdl_pump_events() {
    pump();
}
extern "C" fn sdl_poll_event(event: *mut u8) -> bool {
    pump();
    let Some(e) = PENDING.lock().unwrap().pop_front() else { return false };
    if !event.is_null() {
        // SAFETY: the caller's SDL_Event.
        unsafe { std::ptr::copy_nonoverlapping(e.0.as_ptr(), event, 128) };
    }
    true
}
extern "C" fn sdl_flush_events(min: u32, max: u32) {
    pump();
    PENDING.lock().unwrap().retain(|e| {
        let kind = u32::from_le_bytes(e.0[0..4].try_into().unwrap());
        !(min..=max).contains(&kind)
    });
}
extern "C" fn sdl_get_window_from_event(event: *const u8) -> *mut Window {
    // SAFETY: the caller's SDL_Event.
    let id = unsafe { (event.add(16) as *const u32).read_unaligned() };
    window_handle(id)
}

// --- keyboard ----------------------------------------------------------------

const SCANCODE_MASK: u32 = 1 << 30;

/// The key of a scancode in a US layout (characters as code points).
fn keycode(scancode: u32) -> u32 {
    const ROW: &[u8] = b"\r\x1b\x08\t -=[]\\#;'`,./";
    match scancode {
        4..=29 => b'a' as u32 + scancode - 4,
        30..=38 => b'1' as u32 + scancode - 30,
        39 => b'0' as u32,
        40..=56 => ROW[(scancode - 40) as usize] as u32,
        76 => 0x7f,
        0 => 0,
        _ => scancode | SCANCODE_MASK,
    }
}

extern "C" fn sdl_get_key_from_scancode(scancode: u32, _mods: u16, _key_event: bool) -> u32 {
    keycode(scancode)
}

extern "C" fn sdl_get_keyboard_state(count: *mut i32) -> *const bool {
    static KEYS: Mutex<u32> = Mutex::new(0);
    pump();
    let mut keys = KEYS.lock().unwrap();
    if *keys == 0 {
        *keys = Box::leak(Box::new([false; 512])) as *const [bool; 512] as u32;
    }
    // SAFETY: the leaked array, refreshed from the tracked state.
    unsafe { *(*keys as usize as *mut [bool; 512]) = state().keys };
    write(count, 512);
    *keys as usize as *const bool
}
extern "C" fn sdl_get_mod_state() -> u16 {
    state().mods
}
extern "C" fn sdl_get_key_name(key: u32) -> *const c_char {
    static NAMES: Mutex<Option<std::collections::HashMap<u32, u32>>> = Mutex::new(None);
    let mut names = NAMES.lock().unwrap();
    let names = names.get_or_insert_with(Default::default);
    *names.entry(key).or_insert_with(|| leak_c_str(&key_name(key))) as usize as *const c_char
}

fn key_name(key: u32) -> String {
    if key & SCANCODE_MASK == 0 {
        return match key {
            0 => String::new(),
            0x0d => "Return".into(),
            0x1b => "Escape".into(),
            0x08 => "Backspace".into(),
            0x09 => "Tab".into(),
            0x20 => "Space".into(),
            0x7f => "Delete".into(),
            c => char::from_u32(c).map(|c| c.to_uppercase().to_string()).unwrap_or_default(),
        };
    }
    let scancode = key & !SCANCODE_MASK;
    match scancode {
        57 => "CapsLock".into(),
        58..=69 => format!("F{}", scancode - 57),
        70 => "PrintScreen".into(),
        71 => "ScrollLock".into(),
        72 => "Pause".into(),
        73 => "Insert".into(),
        74 => "Home".into(),
        75 => "PageUp".into(),
        77 => "End".into(),
        78 => "PageDown".into(),
        79 => "Right".into(),
        80 => "Left".into(),
        81 => "Down".into(),
        82 => "Up".into(),
        83 => "Numlock".into(),
        84 => "Keypad /".into(),
        85 => "Keypad *".into(),
        86 => "Keypad -".into(),
        87 => "Keypad +".into(),
        88 => "Keypad Enter".into(),
        89..=97 => format!("Keypad {}", scancode - 88),
        98 => "Keypad 0".into(),
        99 => "Keypad .".into(),
        101 => "Application".into(),
        104..=115 => format!("F{}", scancode - 104 + 13),
        224 => "Left Ctrl".into(),
        225 => "Left Shift".into(),
        226 => "Left Alt".into(),
        227 => "Left GUI".into(),
        228 => "Right Ctrl".into(),
        229 => "Right Shift".into(),
        230 => "Right Alt".into(),
        231 => "Right GUI".into(),
        _ => String::new(),
    }
}

extern "C" fn sdl_start_text_input(_window: *mut Window) -> bool {
    true
}
extern "C" fn sdl_stop_text_input(_window: *mut Window) -> bool {
    true
}
extern "C" fn sdl_text_input_active(_window: *const Window) -> bool {
    true
}
extern "C" fn sdl_set_text_input_area(_window: *mut Window, _rect: u32, _cursor: i32) -> bool {
    true
}
extern "C" fn sdl_clear_composition(_window: *mut Window) -> bool {
    true
}
extern "C" fn sdl_get_clipboard_text() -> *mut c_char {
    // SAFETY: an empty malloc'd string the caller frees.
    unsafe { calloc(1, 1) as *mut c_char }
}
extern "C" fn sdl_set_clipboard_text(text: *const c_char) -> bool {
    let text = c_str(text as u32);
    unsafe { clipboard_set(text.as_ptr(), text.len()) };
    true
}

// --- mouse -------------------------------------------------------------------

extern "C" fn sdl_get_mouse_state(x: *mut f32, y: *mut f32) -> u32 {
    pump();
    let (mx, my, buttons) = state().mouse;
    if !x.is_null() {
        unsafe { *x = mx };
    }
    if !y.is_null() {
        unsafe { *y = my };
    }
    buttons
}
extern "C" fn sdl_get_relative_mouse_state(x: *mut f32, y: *mut f32) -> u32 {
    if !x.is_null() {
        unsafe { *x = 0.0 };
    }
    if !y.is_null() {
        unsafe { *y = 0.0 };
    }
    state().mouse.2
}
extern "C" fn sdl_warp_mouse_in_window(_window: *mut Window, x: f32, y: f32) {
    let mut state = state();
    state.mouse.0 = x;
    state.mouse.1 = y;
}
extern "C" fn sdl_set_window_mouse_grab(_window: *mut Window, _grabbed: bool) -> bool {
    true
}
extern "C" fn sdl_set_window_relative_mouse_mode(_window: *mut Window, enabled: bool) -> bool {
    state().relative = enabled;
    unsafe { mouse_relative(enabled) };
    true
}
/// Cursors are their system cursor id + 1.
extern "C" fn sdl_create_system_cursor(id: i32) -> u32 {
    id as u32 + 1
}
extern "C" fn sdl_create_color_cursor(_surface: *mut Surface, _x: i32, _y: i32) -> u32 {
    1
}
extern "C" fn sdl_get_default_cursor() -> u32 {
    1
}
extern "C" fn sdl_set_cursor(cursor: u32) -> bool {
    unsafe { cursor_set(cursor as i32 - 1) };
    true
}
extern "C" fn sdl_destroy_cursor(_cursor: u32) {}
extern "C" fn sdl_show_cursor() -> bool {
    true
}
extern "C" fn sdl_hide_cursor() -> bool {
    true
}

/// The function named `name`, for dlsym on libSDL3.
pub(super) fn symbol(name: &str) -> Option<usize> {
    macro_rules! table {
        ($($name:literal => $f:expr,)*) => {
            match name {
                $($name => Some($f as *const () as usize),)*
                _ => None,
            }
        };
    }
    table! {
        "SDL_Init" => sdl_init,
        "SDL_InitSubSystem" => sdl_init,
        "SDL_Quit" => sdl_quit,
        "SDL_QuitSubSystem" => sdl_quit_sub_system,
        "SDL_WasInit" => sdl_was_init,
        "SDL_SetHint" => sdl_set_hint,
        "SDL_GetHint" => sdl_get_hint,
        "SDL_SetAppMetadataProperty" => sdl_set_app_metadata_property,
        "SDL_SetLogOutputFunction" => sdl_set_log_output_function,
        "SDL_SetLogPriorities" => sdl_set_log_priorities,
        "SDL_GetError" => sdl_get_error,
        "SDL_ClearError" => sdl_clear_error,
        "SDL_free" => sdl_free,
        "SDL_GetVersion" => sdl_get_version,
        "SDL_GetRevision" => sdl_get_revision,
        "SDL_GetPlatform" => sdl_get_platform,
        "SDL_GetSystemTheme" => sdl_get_system_theme,
        "SDL_GetCurrentVideoDriver" => sdl_get_current_video_driver,
        "SDL_GetTicksNS" => sdl_get_ticks_ns,
        "SDL_GetTicks" => sdl_get_ticks,
        "SDL_GetPerformanceCounter" => sdl_get_performance_counter,
        "SDL_GetPerformanceFrequency" => sdl_get_performance_frequency,
        "SDL_DelayNS" => sdl_delay_ns,
        "SDL_Delay" => sdl_delay,
        "SDL_GetDisplays" => sdl_get_displays,
        "SDL_GetPrimaryDisplay" => sdl_get_primary_display,
        "SDL_GetDisplayForWindow" => sdl_get_display_for_window,
        "SDL_GetDisplayName" => sdl_get_display_name,
        "SDL_GetDisplayBounds" => sdl_get_display_bounds,
        "SDL_GetDisplayUsableBounds" => sdl_get_display_bounds,
        "SDL_GetDesktopDisplayMode" => sdl_get_desktop_display_mode,
        "SDL_GetCurrentDisplayMode" => sdl_get_current_display_mode,
        "SDL_GetFullscreenDisplayModes" => sdl_get_fullscreen_display_modes,
        "SDL_GetDisplayContentScale" => sdl_get_display_content_scale,
        "SDL_CreateWindow" => sdl_create_window,
        "SDL_DestroyWindow" => sdl_destroy_window,
        "SDL_GetWindowID" => sdl_get_window_id,
        "SDL_GetWindowFromID" => sdl_get_window_from_id,
        "SDL_GetWindowFlags" => sdl_get_window_flags,
        "SDL_SetWindowTitle" => sdl_set_window_title,
        "SDL_GetWindowSize" => sdl_get_window_size,
        "SDL_GetWindowSizeInPixels" => sdl_get_window_size_in_pixels,
        "SDL_GetWindowPixelDensity" => sdl_get_window_pixel_density,
        "SDL_GetWindowDisplayScale" => sdl_get_window_display_scale,
        "SDL_GetWindowPosition" => sdl_get_window_position,
        "SDL_SetWindowPosition" => sdl_window_noop_ii,
        "SDL_SetWindowSize" => sdl_window_noop_ii,
        "SDL_SetWindowMinimumSize" => sdl_window_noop_ii,
        "SDL_SetWindowMaximumSize" => sdl_window_noop_ii,
        "SDL_SetWindowIcon" => sdl_window_noop_p,
        "SDL_SetWindowResizable" => sdl_window_noop_p,
        "SDL_SyncWindow" => sdl_window_noop,
        "SDL_ShowWindow" => sdl_show_window,
        "SDL_HideWindow" => sdl_hide_window,
        "SDL_RaiseWindow" => sdl_window_noop,
        "SDL_MaximizeWindow" => sdl_window_noop,
        "SDL_MinimizeWindow" => sdl_window_noop,
        "SDL_RestoreWindow" => sdl_window_noop,
        "SDL_SetWindowFullscreen" => sdl_set_window_fullscreen,
        "SDL_GetWindowFullscreenMode" => sdl_get_window_fullscreen_mode,
        "SDL_SetWindowFullscreenMode" => sdl_window_noop_p,
        "SDL_GetWindowProperties" => sdl_get_window_properties,
        "SDL_GetNumberProperty" => sdl_get_number_property,
        "SDL_CreateSurfaceFrom" => sdl_create_surface_from,
        "SDL_DestroySurface" => sdl_destroy_surface,
        "SDL_AddSurfaceAlternateImage" => sdl_add_surface_alternate_image,
        "SDL_GetPixelFormatDetails" => sdl_get_pixel_format_details,
        "SDL_GL_LoadLibrary" => sdl_gl_load_library,
        "SDL_GL_SetAttribute" => sdl_gl_set_attribute,
        "SDL_GL_GetAttribute" => sdl_gl_get_attribute,
        "SDL_GL_CreateContext" => sdl_gl_create_context,
        "SDL_GL_MakeCurrent" => sdl_gl_make_current,
        "SDL_GL_DestroyContext" => sdl_gl_destroy_context,
        "SDL_GL_GetCurrentWindow" => sdl_gl_get_current_window,
        "SDL_GL_SetSwapInterval" => sdl_gl_set_swap_interval,
        "SDL_GL_GetSwapInterval" => sdl_gl_get_swap_interval,
        "SDL_GL_SwapWindow" => sdl_gl_swap_window,
        "SDL_GL_GetProcAddress" => sdl_gl_get_proc_address,
        "SDL_GL_ExtensionSupported" => sdl_gl_extension_supported,
        "SDL_Vulkan_LoadLibrary" => sdl_vulkan_load_library,
        "SDL_Vulkan_UnloadLibrary" => sdl_vulkan_unload_library,
        "SDL_Vulkan_GetInstanceExtensions" => sdl_vulkan_get_instance_extensions,
        "SDL_Vulkan_GetVkGetInstanceProcAddr" => sdl_vulkan_get_vk_get_instance_proc_addr,
        "SDL_Vulkan_GetPresentationSupport" => sdl_vulkan_get_presentation_support,
        "SDL_PumpEvents" => sdl_pump_events,
        "SDL_PollEvent" => sdl_poll_event,
        "SDL_FlushEvents" => sdl_flush_events,
        "SDL_GetWindowFromEvent" => sdl_get_window_from_event,
        "SDL_GetKeyFromScancode" => sdl_get_key_from_scancode,
        "SDL_GetKeyboardState" => sdl_get_keyboard_state,
        "SDL_GetModState" => sdl_get_mod_state,
        "SDL_GetKeyName" => sdl_get_key_name,
        "SDL_StartTextInput" => sdl_start_text_input,
        "SDL_StopTextInput" => sdl_stop_text_input,
        "SDL_TextInputActive" => sdl_text_input_active,
        "SDL_SetTextInputArea" => sdl_set_text_input_area,
        "SDL_GetClipboardText" => sdl_get_clipboard_text,
        "SDL_SetClipboardText" => sdl_set_clipboard_text,
        "SDL_ClearComposition" => sdl_clear_composition,
        "SDL_GetClosestFullscreenDisplayMode" => sdl_get_closest_fullscreen_display_mode,
        "SDL_SetWindowBordered" => sdl_set_window_bordered,
        "SDL_GL_UnloadLibrary" => sdl_gl_unload_library,
        "SDL_ShowSimpleMessageBox" => sdl_show_simple_message_box,
        "SDL_ShowMessageBox" => sdl_show_message_box,
        "SDL_OpenURL" => sdl_open_url,
        "SDL_Vulkan_CreateSurface" => sdl_vulkan_create_surface,
        "SDL_GetMouseState" => sdl_get_mouse_state,
        "SDL_GetGlobalMouseState" => sdl_get_mouse_state,
        "SDL_GetRelativeMouseState" => sdl_get_relative_mouse_state,
        "SDL_WarpMouseInWindow" => sdl_warp_mouse_in_window,
        "SDL_SetWindowMouseGrab" => sdl_set_window_mouse_grab,
        "SDL_SetWindowKeyboardGrab" => sdl_set_window_mouse_grab,
        "SDL_SetWindowRelativeMouseMode" => sdl_set_window_relative_mouse_mode,
        "SDL_CreateSystemCursor" => sdl_create_system_cursor,
        "SDL_CreateColorCursor" => sdl_create_color_cursor,
        "SDL_GetDefaultCursor" => sdl_get_default_cursor,
        "SDL_SetCursor" => sdl_set_cursor,
        "SDL_DestroyCursor" => sdl_destroy_cursor,
        "SDL_ShowCursor" => sdl_show_cursor,
        "SDL_HideCursor" => sdl_hide_cursor,
    }
}
