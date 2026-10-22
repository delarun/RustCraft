#!/bin/sh
# Prepares a work directory for build.sh: the wasi-sdk and the library
# sources at the versions rjava.wasm is built with.
#   vm/native-libs/fetch.sh <work dir>   (then <work dir>/build.sh; point
#   RJAVA_WASM_LIBS at <work dir> when building rjava.wasm)
set -e
W=${1:?usage: fetch.sh <work dir>}
H=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$W/src" "$W/tmp"
cp -r "$H/build.sh" "$H/nopic-launcher.sh" "$H/toolchain-wasip1-threads.cmake" "$H/patches" "$H/stbsrc" "$W/"
cd "$W"
if [ ! -d wasi-sdk ]; then
  curl -L https://github.com/WebAssembly/wasi-sdk/releases/download/wasi-sdk-34/wasi-sdk-34.0-x86_64-linux.tar.gz | tar xz
  mv wasi-sdk-34.0-x86_64-linux wasi-sdk
fi
[ -d src/shaderc ] || git clone --depth 1 --branch v2026.4 https://github.com/google/shaderc src/shaderc
(cd src/shaderc && ./utils/git-sync-deps)
[ -d src/SPIRV-Cross ] || git clone --depth 1 --branch vulkan-sdk-1.4.363.0 https://github.com/KhronosGroup/SPIRV-Cross src/SPIRV-Cross
if [ ! -d src/stb ]; then
  git clone https://github.com/nothings/stb src/stb
  git -C src/stb checkout a7d4c21
fi
echo "Sources ready in $W; now run $W/build.sh"
