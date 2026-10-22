# RustCraft Bridge

A web page cannot open TCP connections, but Minecraft servers speak TCP.
**RustCraft Bridge** is a small program that runs on your computer while you play
RustCraft. It lets the RustCraft page connect to Minecraft servers. Singleplayer does not
need it.

- It listens on `127.0.0.1:25580`, the loopback interface, so nothing on your network can
  reach it.
- It only accepts the RustCraft page (`https://rustcraft.online`, and `localhost:5173` for
  development). The browser's `Origin` header decides; other websites are refused.
- It connects only to the server you choose in the game. A name with the default port
  uses the server's Minecraft SRV record, as Minecraft does.
- Your traffic goes from your browser to the bridge on your computer and from there
  straight to the server. It does not pass through RustCraft's servers.

## Use

1. Download the file for your system from the
   [releases](https://github.com/delarun/RustCraft/releases/latest):
   - Windows: `rustcraft-bridge-windows-x86_64.exe`;
   - macOS: `rustcraft-bridge-macos-arm64` (Apple silicon) or `-macos-x86_64`;
   - Linux: `rustcraft-bridge-linux-x86_64`.
2. Start it. On macOS and Linux, run `chmod +x rustcraft-bridge-*` first. The binaries are
   not signed: Windows SmartScreen and macOS Gatekeeper may warn. On macOS, right-click →
   Open.
3. Keep its window open while you play on servers. The launcher's *Multiplayer* settings
   show whether it is found.

Chrome may ask whether the page may access devices on your local network the first time
it talks to the bridge. Allow it, or the bridge cannot be reached.

## Options

```
rustcraft-bridge [--port <port>] [--allow-origin <origin>]... [--verbose]
```

- `--port`: the port to listen on (default 25580). Change the address in the launcher's
  settings to match.
- `--allow-origin`: another page origin to accept, e.g. a self-hosted RustCraft or a
  development server opened by its LAN address (`--allow-origin http://192.168.1.10:5173`).
  The bridge refuses any page whose origin is not on the list, and the launcher then shows
  it as not found.
- `--verbose`: log connections.

## Building

```sh
cargo build --release      # target/release/rustcraft-bridge
cargo test                 # RUSTCRAFT_DNS_TEST=1 also checks a live SRV lookup
```

It has no dependencies beyond the Rust standard library. Pushing a tag `bridge-v<version>`
builds the release binaries (`.github/workflows/bridge.yml`).
