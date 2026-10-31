#!/bin/sh
# Builds Horizon (github.com/horizon-rust/horizon, MIT: Minecraft: Java Edition 26.3 rewritten in
# Rust) for the launcher's "Horizon" choice, into horizon/dist:
#   horizon-client.wasm   the game
#   horizon-prepare.wasm  prepares its resources from the player's own Minecraft, in the browser
#   root.pack             what horizon-prepare reads from Horizon's tree (pins, exporter sources
#                         and their compiled classes: Horizon's own code, no Mojang files)
#   horizon.json          the files' names and the pack's tag
#
#   horizon/build.sh <compiled exporters dir>
#
# The exporters are compiled against Mojang's jars, which are not ours to ship: run
# `horizon-prepare --prepare ...` then `horizon-prepare compile-exporters` in a native checkout of
# the same commit and pass its .local/exporters/<key> folder.
#
# Needs: git, curl, node, Rust with wasm32-wasip1-threads, and wasi-sdk 34 (RJAVA_WASM_LIBS, as for
# the VM: vm/native-libs/fetch.sh).
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
EXPORTERS=$(cd "${1:?usage: horizon/build.sh <compiled exporters dir>}" && pwd)
LIBS=${RJAVA_WASM_LIBS:-$HERE/../.scratch/wasm-libs}
WORK=${HORIZON_WORK:-$HERE/../.scratch/horizon-build}
COMMIT=9069aea3e2f5b002e61373f74086e1e7a0b9ccd1
WINIT_SHA256=a6755fa58a9f8350bd1e472d4c3fcc25f824ec358933bba33306d0b63df5978d
CPAL_SHA256=cbd307f43cc2a697e2d1f8bc7a1d824b5269e052209e28883e5bc04d095aaa3f

# Upstream at the pinned commit, winit and cpal from crates.io, then our changes.
if [ ! -d "$WORK/src/.git" ]; then
  git clone --filter=blob:none https://github.com/horizon-rust/horizon.git "$WORK/src"
fi
cd "$WORK/src"
git checkout -q --force "$COMMIT"
git clean -qfdx -e target
crate() {
  curl -fsSL "https://static.crates.io/crates/$1/$1-$2.crate" -o "$WORK/$1.crate"
  echo "$3  $WORK/$1.crate" | sha256sum -c --quiet
  mkdir -p "third_party/$1"
  tar xzf "$WORK/$1.crate" -C "third_party/$1" --strip-components=1
}
crate winit 0.30.13 "$WINIT_SHA256"
crate cpal 0.16.0 "$CPAL_SHA256"
git apply --whitespace=nowarn "$HERE/horizon-web.patch"

# wasi-sdk's wasi-libc (not Rust's own: its thread exit is the one we checked), 4 GiB of memory
# at most for the game, 2 for the preparer, and the main thread's stack as a native one's.
WASI_SDK=$LIBS/wasi-sdk
SYSROOT=$WASI_SDK/share/wasi-sysroot/lib/wasm32-wasip1-threads
export CARGO_TARGET_DIR="$WORK/target"
export CC_wasm32_wasip1_threads=$WASI_SDK/bin/clang AR_wasm32_wasip1_threads=$WASI_SDK/bin/llvm-ar
export CFLAGS_wasm32_wasip1_threads="--sysroot=$WASI_SDK/share/wasi-sysroot"
flags() {
  echo "-C link-arg=--max-memory=$1 -C link-arg=-zstack-size=8388608 -C link-self-contained=no -L native=$SYSROOT -C link-arg=$SYSROOT/crt1-command.o"
}
CARGO_TARGET_WASM32_WASIP1_THREADS_RUSTFLAGS=$(flags 4294967296) \
  cargo build --profile fast -p horizon-client --bin horizon-client --no-default-features --features audio --target wasm32-wasip1-threads
CARGO_TARGET_WASM32_WASIP1_THREADS_RUSTFLAGS=$(flags 2147483648) \
  cargo build --release -p horizon-prepare --bin horizon-prepare --target wasm32-wasip1-threads

DIST=$HERE/dist
mkdir -p "$DIST"
cp "$WORK/target/wasm32-wasip1-threads/fast/horizon-client.wasm" "$DIST/"
cp "$WORK/target/wasm32-wasip1-threads/release/horizon-prepare.wasm" "$DIST/"
cp LICENSE "$DIST/LICENSE.txt"
TAG=$(node "$HERE/pack.mjs" . "$EXPORTERS" "$DIST/root.pack")
VERSION=$(node -e 'console.log(JSON.parse(require("fs").readFileSync("research-lock.json")).minecraft.version)')
cat > "$DIST/horizon.json" <<EOF
{
  "commit": "$COMMIT",
  "version": "$VERSION",
  "client": "horizon-client.wasm",
  "prepare": "horizon-prepare.wasm",
  "pack": "root.pack",
  "packTag": "$TAG"
}
EOF
echo "horizon/dist: Horizon $COMMIT for Minecraft $VERSION"
