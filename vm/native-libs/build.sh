#!/bin/sh
# Reproduces the wasm32-wasip1-threads static libs in ./dist (sources already fetched into ./src, see report).
set -e
W=$(cd "$(dirname "$0")" && pwd); export TMPDIR=$W/tmp
FLAGS="-O2 -DNDEBUG -fno-exceptions -fno-rtti -ffunction-sections -fdata-sections"
TC="-DCMAKE_TOOLCHAIN_FILE=$W/toolchain-wasip1-threads.cmake -DCMAKE_C_COMPILER_LAUNCHER=$W/nopic-launcher.sh -DCMAKE_CXX_COMPILER_LAUNCHER=$W/nopic-launcher.sh -DCMAKE_BUILD_TYPE=Release"
# glslang patch (keep thread support on wasi-threads) - idempotent
grep -q '_REENTRANT' "$W/src/shaderc/third_party/glslang/glslang/Include/Common.h" || \
  (cd "$W/src/shaderc/third_party" && patch -p0 < "$W/patches/glslang-wasi-threads.patch")
cmake -S "$W/src/shaderc" -B "$W/build/shaderc" -G Ninja $TC -DCMAKE_C_FLAGS_RELEASE="$FLAGS" -DCMAKE_CXX_FLAGS_RELEASE="$FLAGS" \
 -DSHADERC_SKIP_TESTS=ON -DSHADERC_SKIP_EXAMPLES=ON -DSHADERC_SKIP_EXECUTABLES=ON -DSHADERC_SKIP_INSTALL=OFF -DSHADERC_SKIP_COPYRIGHT_CHECK=ON \
 -DSHADERC_ENABLE_WERROR_COMPILE=OFF -DSHADERC_ENABLE_HLSL=ON -DSPIRV_SKIP_EXECUTABLES=ON -DSPIRV_SKIP_TESTS=ON -DSPIRV_TOOLS_BUILD_STATIC=ON \
 -DSPIRV_WERROR=OFF -DENABLE_GLSLANG_BINARIES=OFF -DGLSLANG_TESTS=OFF -DENABLE_EXCEPTIONS=OFF -DENABLE_RTTI=OFF -DENABLE_OPT=ON \
 -DSPIRV_HEADERS_SKIP_EXAMPLES=ON -DBUILD_SHARED_LIBS=OFF -DCMAKE_INSTALL_PREFIX="$W/install/shaderc"
ninja -C "$W/build/shaderc" shaderc shaderc_combined shaderc_util SPIRV-Tools-static SPIRV-Tools-opt
cmake -S "$W/src/SPIRV-Cross" -B "$W/build/spirv-cross" -G Ninja $TC -DCMAKE_C_FLAGS_RELEASE="$FLAGS" -DCMAKE_CXX_FLAGS_RELEASE="$FLAGS" \
 -DSPIRV_CROSS_EXCEPTIONS_TO_ASSERTIONS=ON -DSPIRV_CROSS_STATIC=ON -DSPIRV_CROSS_SHARED=OFF -DSPIRV_CROSS_CLI=OFF -DSPIRV_CROSS_ENABLE_TESTS=OFF \
 -DSPIRV_CROSS_ENABLE_GLSL=ON -DSPIRV_CROSS_ENABLE_HLSL=OFF -DSPIRV_CROSS_ENABLE_MSL=OFF -DSPIRV_CROSS_ENABLE_CPP=OFF -DSPIRV_CROSS_ENABLE_REFLECT=ON \
 -DSPIRV_CROSS_ENABLE_C_API=ON -DSPIRV_CROSS_ENABLE_UTIL=ON -DSPIRV_CROSS_SKIP_INSTALL=ON
ninja -C "$W/build/spirv-cross"
mkdir -p "$W/build/stb"
"$W/wasi-sdk/bin/clang" --target=wasm32-wasip1-threads --sysroot="$W/wasi-sdk/share/wasi-sysroot" -pthread -O2 -DNDEBUG -ffunction-sections -fdata-sections \
  -I"$W/src/stb" -c "$W/stbsrc/stb_impl.c" -o "$W/build/stb/stb_impl.o"
rm -f "$W/build/stb/libstb.a"; "$W/wasi-sdk/bin/llvm-ar" rcs "$W/build/stb/libstb.a" "$W/build/stb/stb_impl.o"
D=$W/dist; mkdir -p $D/lib $D/include/shaderc $D/include/spirv_cross $D/include/stb
B=$W/build/shaderc
cp $B/libshaderc/libshaderc_combined.a $B/libshaderc/libshaderc.a $B/libshaderc_util/libshaderc_util.a $B/third_party/glslang/glslang/libglslang.a \
   $B/third_party/spirv-tools/source/libSPIRV-Tools.a $B/third_party/spirv-tools/source/opt/libSPIRV-Tools-opt.a \
   $W/build/spirv-cross/libspirv-cross-*.a $W/build/stb/libstb.a $D/lib/
cp $W/src/shaderc/libshaderc/include/shaderc/*.h $D/include/shaderc/
cp $W/src/SPIRV-Cross/spirv_cross_c.h $W/src/SPIRV-Cross/spirv.h $D/include/spirv_cross/
cp $W/src/stb/stb_image.h $W/src/stb/stb_image_write.h $W/src/stb/deprecated/stb_image_resize2.h $D/include/stb/
