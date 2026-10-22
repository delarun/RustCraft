//! LWJGL's stb natives (liblwjgl_stb), on the stb compiled into the VM.

use super::libs::c_symbol;
use super::invoke_support::array;
use crate::native::*;

unsafe extern "C" {
    fn rjava_stbi_write_png_compression_level() -> u32;
    fn rjava_stbi_write_force_png_filter() -> u32;
    fn rjava_stbi_write_tga_with_rle() -> u32;
}

/// LWJGL's stbi_zlib_compress override (a function pointer; null: stb's own).
static ZLIB_COMPRESS: u32 = 0;

/// The stb function `name` as `F`.
///
/// # Safety
/// `F` must be its signature.
unsafe fn stb<F: Copy>(name: &str) -> F {
    let address = c_symbol(name).unwrap_or_else(|| panic!("stb function {name} missing"));
    // SAFETY: function pointers are table indices under wasm32.
    unsafe { std::mem::transmute_copy(&address) }
}

pub(super) fn register(n: &mut Natives) {
    // LWJGL's allocator hooks: our stb uses the C library's.
    n.add("org/lwjgl/stb/LibSTB.setupMalloc(JJJJJJ)V", no_op);
    n.add("org/lwjgl/stb/STBImage.nstbi_load_from_memory(JIJJJI)J", |t, a| {
        let f: extern "C" fn(u32, i32, u32, u32, u32, i32) -> u32 = unsafe { stb("stbi_load_from_memory") };
        let r = super::enter_native(t, || f(a[0] as u32, a[2] as i32, a[3] as u32, a[5] as u32, a[7] as u32, a[9] as i32));
        ret_j(r as i64)
    });
    n.add("org/lwjgl/stb/STBImage.nstbi_load_from_memory(JI[I[I[II)J", |t, a| {
        let f: extern "C" fn(u32, i32, u32, u32, u32, i32) -> u32 = unsafe { stb("stbi_load_from_memory") };
        let (x, y, c) = (array(t, a[3]), array(t, a[4]), array(t, a[5]));
        let r = super::enter_native(t, || f(a[0] as u32, a[2] as i32, x, y, c, a[6] as i32));
        ret_j(r as i64)
    });
    n.add("org/lwjgl/stb/STBImage.nstbi_info_from_memory(JIJJJ)I", |_, a| {
        let f: extern "C" fn(u32, i32, u32, u32, u32) -> i32 = unsafe { stb("stbi_info_from_memory") };
        ret_i(f(a[0] as u32, a[2] as i32, a[3] as u32, a[5] as u32, a[7] as u32))
    });
    n.add("org/lwjgl/stb/STBImage.nstbi_info_from_memory(JI[I[I[I)I", |t, a| {
        let f: extern "C" fn(u32, i32, u32, u32, u32) -> i32 = unsafe { stb("stbi_info_from_memory") };
        ret_i(f(a[0] as u32, a[2] as i32, array(t, a[3]), array(t, a[4]), array(t, a[5])))
    });
    n.add("org/lwjgl/stb/STBImage.nstbi_failure_reason()J", |_, _| {
        let f: extern "C" fn() -> u32 = unsafe { stb("stbi_failure_reason") };
        ret_j(f() as i64)
    });
    n.add("org/lwjgl/stb/STBImage.nstbi_image_free(J)V", |_, a| {
        let f: extern "C" fn(u32) = unsafe { stb("stbi_image_free") };
        f(a[0] as u32);
        ret_void()
    });
    n.add("org/lwjgl/stb/STBImage.nstbi_set_flip_vertically_on_load(I)V", |_, a| {
        let f: extern "C" fn(i32) = unsafe { stb("stbi_set_flip_vertically_on_load") };
        f(a[0] as i32);
        ret_void()
    });
    n.add("org/lwjgl/stb/STBImageWrite.nstbi_write_png_compression_level()J", |_, _| {
        ret_j(unsafe { rjava_stbi_write_png_compression_level() } as i64)
    });
    n.add("org/lwjgl/stb/STBImageWrite.nstbi_write_force_png_filter()J", |_, _| {
        ret_j(unsafe { rjava_stbi_write_force_png_filter() } as i64)
    });
    n.add("org/lwjgl/stb/STBImageWrite.nstbi_write_tga_with_rle()J", |_, _| ret_j(unsafe { rjava_stbi_write_tga_with_rle() } as i64));
    n.add("org/lwjgl/stb/STBImageWrite.nstbi_zlib_compress()J", |_, _| ret_j(&ZLIB_COMPRESS as *const u32 as u32 as i64));
    n.add("org/lwjgl/stb/STBImageWrite.nstbi_flip_vertically_on_write(I)V", |_, a| {
        let f: extern "C" fn(i32) = unsafe { stb("stbi_flip_vertically_on_write") };
        f(a[0] as i32);
        ret_void()
    });
    // stbi_write_{bmp,tga,hdr}(filename, w, h, comp, data) and jpg(..., quality).
    n.add("org/lwjgl/stb/STBImageWrite.nstbi_write_bmp(JIIIJ)I", |t, a| write_file(t, "stbi_write_bmp", a, None));
    n.add("org/lwjgl/stb/STBImageWrite.nstbi_write_tga(JIIIJ)I", |t, a| write_file(t, "stbi_write_tga", a, None));
    n.add("org/lwjgl/stb/STBImageWrite.nstbi_write_hdr(JIIIJ)I", |t, a| write_file(t, "stbi_write_hdr", a, None));
    n.add("org/lwjgl/stb/STBImageWrite.nstbi_write_jpg(JIIIJI)I", |t, a| write_file(t, "stbi_write_jpg", a, Some(a[7] as i32)));
    // stbi_write_{bmp,tga,hdr}_to_func(func, context, w, h, comp, data) and jpg(..., quality).
    n.add("org/lwjgl/stb/STBImageWrite.nstbi_write_bmp_to_func(JJIIIJ)I", |t, a| write_func(t, "stbi_write_bmp_to_func", a, None));
    n.add("org/lwjgl/stb/STBImageWrite.nstbi_write_tga_to_func(JJIIIJ)I", |t, a| write_func(t, "stbi_write_tga_to_func", a, None));
    n.add("org/lwjgl/stb/STBImageWrite.nstbi_write_hdr_to_func(JJIIIJ)I", |t, a| write_func(t, "stbi_write_hdr_to_func", a, None));
    n.add("org/lwjgl/stb/STBImageWrite.nstbi_write_jpg_to_func(JJIIIJI)I", |t, a| write_func(t, "stbi_write_jpg_to_func", a, Some(a[9] as i32)));
    n.add("org/lwjgl/stb/STBImageWrite.nstbi_write_png(JIIIJI)I", |t, a| {
        let f: extern "C" fn(u32, i32, i32, i32, u32, i32) -> i32 = unsafe { stb("stbi_write_png") };
        ret_i(super::enter_native(t, || f(a[0] as u32, a[2] as i32, a[3] as i32, a[4] as i32, a[5] as u32, a[7] as i32)))
    });
    // The callback is a Java upcall (see ffi).
    n.add("org/lwjgl/stb/STBImageWrite.nstbi_write_png_to_func(JJIIIJI)I", |t, a| {
        let f: extern "C" fn(u32, u32, i32, i32, i32, u32, i32) -> i32 = unsafe { stb("stbi_write_png_to_func") };
        ret_i(super::enter_native(t, || f(a[0] as u32, a[2] as u32, a[4] as i32, a[5] as i32, a[6] as i32, a[7] as u32, a[9] as i32)))
    });
    n.add("org/lwjgl/stb/STBImageResize.nstbir_resize_uint8_linear(JIIIJIIII)J", |t, a| {
        let f: extern "C" fn(u32, i32, i32, i32, u32, i32, i32, i32, i32) -> u32 = unsafe { stb("stbir_resize_uint8_linear") };
        let r = super::enter_native(t, || {
            f(a[0] as u32, a[2] as i32, a[3] as i32, a[4] as i32, a[5] as u32, a[7] as i32, a[8] as i32, a[9] as i32, a[10] as i32)
        });
        ret_j(r as i64)
    });
}

/// stbi_write_* to a file: (filename, w, h, comp, data[, quality]).
fn write_file(t: &mut crate::interp::Thread<'_>, name: &str, a: &[u64], quality: Option<i32>) -> crate::interp::JResult<Option<u64>> {
    let r = match quality {
        None => {
            let f: extern "C" fn(u32, i32, i32, i32, u32) -> i32 = unsafe { stb(name) };
            super::enter_native(t, || f(a[0] as u32, a[2] as i32, a[3] as i32, a[4] as i32, a[5] as u32))
        }
        Some(q) => {
            let f: extern "C" fn(u32, i32, i32, i32, u32, i32) -> i32 = unsafe { stb(name) };
            super::enter_native(t, || f(a[0] as u32, a[2] as i32, a[3] as i32, a[4] as i32, a[5] as u32, q))
        }
    };
    ret_i(r)
}

/// stbi_write_*_to_func: (func, context, w, h, comp, data[, quality]); the
/// function is a Java callback (see ffi).
fn write_func(t: &mut crate::interp::Thread<'_>, name: &str, a: &[u64], quality: Option<i32>) -> crate::interp::JResult<Option<u64>> {
    let r = match quality {
        None => {
            let f: extern "C" fn(u32, u32, i32, i32, i32, u32) -> i32 = unsafe { stb(name) };
            super::enter_native(t, || f(a[0] as u32, a[2] as u32, a[4] as i32, a[5] as i32, a[6] as i32, a[7] as u32))
        }
        Some(q) => {
            let f: extern "C" fn(u32, u32, i32, i32, i32, u32, i32) -> i32 = unsafe { stb(name) };
            super::enter_native(t, || f(a[0] as u32, a[2] as u32, a[4] as i32, a[5] as i32, a[6] as i32, a[7] as u32, q))
        }
    };
    ret_i(r)
}
