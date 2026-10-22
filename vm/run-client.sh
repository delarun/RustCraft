#!/bin/sh
# Runs the Minecraft 26.3 client on rjava (natively; LWJGL's native libraries
# from .scratch/client/natives). Prepare with .scratch/tools/fetch-client.py.
# Multi-release jars are read as their base versions: LWJGL then calls its
# JNI natives (liblwjgl.so) instead of the Java 22+ FFM path, which the VM
# does not implement yet.
# Usage: [JVM_OPTS=...] [EXTRA_CP=...] vm/run-client.sh [debug|release] [extra game args]
set -e
here=$(cd "$(dirname "$0")" && pwd)
scratch="$here/../.scratch"
profile=${1:-release}; [ $# -gt 0 ] && shift
cargo build -q --manifest-path "$here/Cargo.toml" -p rjava $( [ "$profile" = release ] && echo --release )
mkdir -p "$scratch/client/game"
cd "$scratch/client/game"
natives="$scratch/client/natives"
RJAVA_MODULES="$scratch/jre25/lib/modules" exec "$here/target/$profile/rjava" -Xmx2g \
    -Djava.library.path="$natives" -Dorg.lwjgl.librarypath="$natives" \
    -Djdk.util.jar.enableMultiRelease=false $JVM_OPTS \
    -cp "$(cat "$scratch/client/classpath.txt")${EXTRA_CP:+:$EXTRA_CP}" net.minecraft.client.main.Main \
    --username Delarun --version 26.3 --gameDir . --assetsDir "$(cat "$scratch/client/assets-root.txt")" \
    --assetIndex 34 --uuid 8c0e91f0-26e8-35ff-a0b6-55ef0a301da5 --accessToken 0 --versionType release "$@"
