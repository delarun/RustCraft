# Wrapper around wasi-sdk-pthread.cmake that targets wasm32-wasip1-threads explicitly.
include(${CMAKE_CURRENT_LIST_DIR}/wasi-sdk/share/cmake/wasi-sdk-pthread.cmake)
set(WASI_SDK_PREFIX ${CMAKE_CURRENT_LIST_DIR}/wasi-sdk)
set(triple wasm32-wasip1-threads)
set(CMAKE_C_COMPILER_TARGET ${triple})
set(CMAKE_CXX_COMPILER_TARGET ${triple})
set(CMAKE_ASM_COMPILER_TARGET ${triple})
set(CMAKE_SYSROOT ${WASI_SDK_PREFIX}/share/wasi-sysroot)
