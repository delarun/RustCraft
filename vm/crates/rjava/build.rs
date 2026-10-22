// The browser build exports the JIT backend's functions that compiled
// modules import (see crates/jit-web), and the allocator.
fn main() {
    if std::env::var("CARGO_CFG_TARGET_FAMILY").as_deref() == Ok("wasm") {
        for name in ["step", "invoke", "select", "interpret", "safepoint", "overflow", "call", "subtype", "pure", "fmod", "monitor", "table_size"] {
            println!("cargo:rustc-link-arg-bins=--export=rjava_jit_{name}");
        }
        // The platform's JS side allocates in the VM's memory (mapped GL
        // buffers, strings).
        for name in ["malloc", "free"] {
            println!("cargo:rustc-link-arg-bins=--export={name}");
        }
        // The JIT puts compiled modules' functions into the module's own table.
        println!("cargo:rustc-link-arg-bins=--export-table");
        println!("cargo:rustc-link-arg-bins=--growable-table");
    }
}
