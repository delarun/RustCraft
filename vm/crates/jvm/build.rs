// The browser build links the C/C++ libraries Minecraft uses through LWJGL
// (shaderc, SPIRV-Cross, stb), built for wasm32-wasip1-threads with wasi-sdk
// into $RJAVA_WASM_LIBS (default: .scratch/wasm-libs; see its build.sh), and
// the table of their functions by name (native/c_symbols.c).
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=native/c_symbols.c");
    println!("cargo:rerun-if-env-changed=RJAVA_WASM_LIBS");
    if std::env::var("CARGO_CFG_TARGET_FAMILY").as_deref() != Ok("wasm") {
        return;
    }
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let libs = std::env::var_os("RJAVA_WASM_LIBS")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest.join("../../../.scratch/wasm-libs"));
    let sdk = libs.join("wasi-sdk");
    let include = libs.join("dist/include");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let object = out.join("c_symbols.o");
    let status = Command::new(sdk.join("bin/clang"))
        .args(["--target=wasm32-wasip1-threads", "-O2", "-c"])
        .arg(format!("--sysroot={}", sdk.join("share/wasi-sysroot").display()))
        .arg("-I").arg(&include)
        .arg("-I").arg(include.join("spirv_cross"))
        .arg("-I").arg(include.join("stb"))
        .arg(manifest.join("native/c_symbols.c"))
        .arg("-o").arg(&object)
        .status()
        .expect("running wasi-sdk clang (set RJAVA_WASM_LIBS)");
    assert!(status.success(), "compiling native/c_symbols.c");
    let archive = out.join("librjava_c_symbols.a");
    let _ = std::fs::remove_file(&archive);
    let status = Command::new(sdk.join("bin/llvm-ar")).arg("crs").arg(&archive).arg(&object).status().unwrap();
    assert!(status.success(), "archiving c_symbols.o");
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-search=native={}", libs.join("dist/lib").display());
    println!("cargo:rustc-link-search=native={}", sdk.join("share/wasi-sysroot/lib/wasm32-wasip1-threads/noeh").display());
    for lib in [
        "rjava_c_symbols",
        "shaderc_combined",
        "spirv-cross-c",
        "spirv-cross-glsl",
        "spirv-cross-reflect",
        "spirv-cross-util",
        "spirv-cross-core",
        "stb",
        "c++",
        "c++abi",
    ] {
        println!("cargo:rustc-link-lib=static={lib}");
    }
}
