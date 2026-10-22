# RustCraft — Java in Browser

RustCraft runs the **original, unmodified Minecraft: Java Edition client** in a web
browser. No installation, no plugins, and no CheerpJ: the game's own JAR runs on
**rjava**, a Java virtual machine written in Rust and compiled to WebAssembly. Rendering
goes through WebGL2 and sound through Web Audio.

Website: <https://rustcraft.online>

> **NOT AN OFFICIAL MINECRAFT PRODUCT. NOT APPROVED BY OR ASSOCIATED WITH MOJANG OR MICROSOFT.**
> Minecraft is a trademark of Mojang AB. RustCraft does not host or redistribute
> any game files. The player's browser downloads them from Mojang's servers, and playing
> requires your own Minecraft: Java Edition license (Microsoft account sign-in).

## Status

- Minecraft **26.3** (Java 25) starts in Chrome and is playable: title screen,
  singleplayer world creation, input, sound and saves.
- The integrated server is the bottleneck. It keeps up at smaller simulation distances, so
  the launcher defaults to render distance 8 and simulation distance 5. JIT work is ongoing.
- Multiplayer works through **RustCraft Bridge** ([bridge/](bridge/README.md)), a small
  program the player runs on their computer: a web page cannot open TCP connections, so the
  game's sockets go over a WebSocket to the bridge, which connects to the server.

## How it works

```
launcher/          the web page (TypeScript, Vite)
  src/             version picker, Microsoft sign-in, downloads into OPFS, game launch
  src/jvm/         runtime for rjava.wasm: WASI over Web Workers (one per Java thread),
                   I/O worker, OPFS/HTTP/memory file systems, WebGL2 (GL 3.3 → WebGL2,
                   GLSL rewriting), display and input, OpenAL → AudioWorklet
  proxy/           small production server: static files with COOP/COEP headers, and a
                   CORS proxy for the Microsoft, Xbox and Minecraft services endpoints
vm/                the JVM (Rust)
  crates/classfile class file parser
  crates/jimage    reader of the JDK runtime image (lib/modules)
  crates/jvm       interpreter, JIT (bytecode → WebAssembly), garbage collector,
                   threads and monitors, class library natives; web/ holds the browser
                   platform: LWJGL, SDL3, OpenGL, OpenAL, stb and shaderc/SPIRV-Cross
                   bindings
  crates/jit-web   JIT backend for the browser (modules instantiated through the JS API
                   on the VM's shared memory)
  crates/jit-wasmtime  JIT backend for native development and tests (wasmtime)
  crates/rjava     the `rjava` launcher binary (native and wasm32-wasip1-threads)
  native-libs/     scripts that build the C/C++ libraries linked into rjava.wasm
bridge/            RustCraft Bridge: a local WebSocket → TCP bridge for multiplayer (Rust, std only)
```

- **The VM.** It interprets bytecode and compiles hot methods into WebAssembly modules.
  The compiled code works on the interpreter's own frames in linear memory, so the two can
  hand frames back and forth. Each Java thread is a Web Worker, and all of them share one
  `WebAssembly.Memory`. The garbage collector is a parallel mark-sweep with sliding
  compaction.
- **The libraries.** LWJGL's Java side runs unchanged. Its native functions are
  implemented in Rust on top of browser APIs: OpenGL calls become WebGL2 calls, and SDL3
  becomes canvas, input and windowing code. Minecraft's shader pipeline (shaderc →
  SPIRV-Cross) is compiled to WebAssembly too, and set to emit GLSL ES 3.00.
- **The launcher.** It downloads the version JSON, client JAR, libraries, assets and
  Mojang's Java 25 runtime (the JDK class library `lib/modules`) from Mojang into the
  browser's Origin Private File System, and verifies their hashes. It then starts rjava
  with the player's Microsoft/Minecraft session.

## Building

Requirements:
- Rust (stable) with the `wasm32-wasip1-threads` target;
- Node.js ≥ 23.6 and pnpm;
- for the C/C++ libraries: CMake, Ninja, git and curl.

1. Build the C/C++ libraries for wasm (shaderc, SPIRV-Cross, stb) once:
   ```sh
   vm/native-libs/fetch.sh "$PWD/.scratch/wasm-libs"   # wasi-sdk 34 and pinned sources
   .scratch/wasm-libs/build.sh
   ```
   The default location is `.scratch/wasm-libs`; otherwise set `RJAVA_WASM_LIBS`.
2. Build the VM for the browser:
   ```sh
   cd vm
   rustup target add wasm32-wasip1-threads
   cargo build --release -p rjava --target wasm32-wasip1-threads
   ```
3. Run the launcher in development:
   ```sh
   cd launcher
   pnpm install
   pnpm dev         # http://localhost:5173/
   ```

See [launcher/README.md](launcher/README.md) for sign-in configuration (the Microsoft
application client ID), the production proxy and deployment.

### Native development and tests

rjava also builds natively (x86-64 Linux), with a wasmtime JIT backend. This is used for
development and for the test suite, which runs Java programs on a reference JVM and on rjava
(interpreted and JIT-compiled) and compares their output:

```sh
cd vm
cargo test --release -p jvm     # needs javac/java ≥ 25; the JDK 25 image is read from .scratch/jre25/lib/modules
cargo run --release -p rjava -- --modules <jdk25>/lib/modules -cp <classes> Main
```

Diagnostics are switched on with environment variables:
- `RJAVA_GC_LOG`: garbage collector log;
- `RJAVA_JIT_STATS`: JIT statistics;
- `RJAVA_JIT_LOG`: methods as they are compiled;
- `RJAVA_JSAMPLE=<thread>`: a sampling profiler of Java stacks;
- `RJAVA_DUMP_EVERY=<s>`: periodic thread dumps;
- `RJAVA_JIT_DUMP=<dir>`: the compiled modules, written to a directory.

## License

RustCraft is free software: you can redistribute it and/or modify it under the terms of
the [GNU Affero General Public License, version 3](LICENSE) (AGPL-3.0-only). If you run a
modified version as a public website, the AGPL requires you to offer its source code to
its users (the launcher links to its source in the footer).

Third-party components compiled into
`rjava.wasm` and the site are listed with their licenses in
[launcher/public/third-party-notices.txt](launcher/public/third-party-notices.txt).
Minecraft, LWJGL and the Java runtime are not part of this repository and are not
distributed by this project.
