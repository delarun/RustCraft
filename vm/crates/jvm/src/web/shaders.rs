//! Shaders for WebGL2: Minecraft cross-compiles its SPIR-V to desktop GLSL
//! with SPIRV-Cross; installing the compiler options here also makes it
//! emit GLSL ES 3.00 instead (with high default precision).

use super::libs::c_symbol;

const GLSL_BIT: u32 = 0x200_0000;
const GLSL_VERSION: u32 = 8 | GLSL_BIT;
const GLSL_ES: u32 = 9 | GLSL_BIT;
const GLSL_ES_DEFAULT_FLOAT_PRECISION_HIGHP: u32 = 11 | GLSL_BIT;
const GLSL_ES_DEFAULT_INT_PRECISION_HIGHP: u32 = 12 | GLSL_BIT;

/// The SPIRV-Cross function `name` as `F`.
///
/// # Safety
/// `F` must be its signature.
unsafe fn spvc<F: Copy>(name: &str) -> F {
    let address = c_symbol(name).unwrap_or_else(|| panic!("{name} missing"));
    // SAFETY: function pointers are table indices under wasm32.
    unsafe { std::mem::transmute_copy(&address) }
}

/// spvc_compiler_install_compiler_options, with GLSL ES 3.00 forced.
extern "C" fn install_compiler_options(compiler: u32, options: u32) -> i32 {
    // SAFETY: SPIRV-Cross's signatures.
    unsafe {
        let set_uint: extern "C" fn(u32, u32, u32) -> i32 = spvc("spvc_compiler_options_set_uint");
        let set_bool: extern "C" fn(u32, u32, i32) -> i32 = spvc("spvc_compiler_options_set_bool");
        set_uint(options, GLSL_VERSION, 300);
        set_bool(options, GLSL_ES, 1);
        set_bool(options, GLSL_ES_DEFAULT_FLOAT_PRECISION_HIGHP, 1);
        set_bool(options, GLSL_ES_DEFAULT_INT_PRECISION_HIGHP, 1);
        let install: extern "C" fn(u32, u32) -> i32 = spvc("spvc_compiler_install_compiler_options");
        install(compiler, options)
    }
}

/// libspirv-cross's functions, with the options hook.
pub(super) fn spirv_cross_symbol(name: &str) -> Option<usize> {
    match name {
        "spvc_compiler_install_compiler_options" => Some(install_compiler_options as *const () as usize),
        _ => c_symbol(name),
    }
}
