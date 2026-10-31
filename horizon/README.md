# Horizon in RustCraft

[Horizon](https://github.com/horizon-rust/horizon) (MIT) is Minecraft: Java Edition 26.3
rewritten in Rust. The launcher offers it as a third choice next to Vanilla and Fabric. It is a
separate game, not Mojang's client: no mods, and no multiplayer yet.

- `horizon-web.patch`: our changes to Horizon at the commit `build.sh` pins, on top of
  winit 0.30.13 and cpal 0.16.0 from crates.io in `third_party/`:
  - a `wasi` platform for winit over the launcher's window and input;
  - a wgpu backend that draws through the page's WebGPU (`crates/horizon-web`);
  - a cpal host that plays through the page's AudioWorklet;
  - a locked allocator (wasi-libc's malloc is not safe when threads allocate at once there);
  - stand-ins for what WASI lacks (process ids, file locks, the clipboard, sockets);
  - `horizon-prepare` that runs its Java exporters on rjava, as child processes the launcher
    starts (`launcher/src/jvm/children.ts`).
- `build.sh <compiled exporters>` builds `dist/`: `horizon-client.wasm`,
  `horizon-prepare.wasm`, `root.pack` (Horizon's pins, exporter sources and classes) and
  `horizon.json`. The site build copies `dist/` to `/horizon/` (`launcher/vite.config.ts`); a
  site built without it shows that Horizon is unavailable.
- `pack.mjs` writes `root.pack`.

Horizon needs resources made from the player's own game files, which nobody may redistribute.
On the first launch the launcher downloads Mojang's 26.3 client and server jars, libraries and
assets into the browser's storage as for Vanilla, then runs `horizon-prepare.wasm` on them.
It imports the pinned files, runs Horizon's exporters (Horizon's Java code calling Mojang's) and
the game's own data generator on rjava, and copies the sounds. This takes a few minutes, once.
The game then runs `horizon-client.wasm` with those resources (`launcher/src/horizon.ts`).
