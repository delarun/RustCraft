#!/bin/sh
# Runs the Minecraft dedicated server (26.3 bundler jar from .scratch) on rjava.
# Usage: vm/run-server.sh [debug|release] [extra server args]
set -e
here=$(cd "$(dirname "$0")" && pwd)
profile=${1:-release}; [ $# -gt 0 ] && shift
cargo build -q --manifest-path "$here/Cargo.toml" -p rjava $( [ "$profile" = release ] && echo --release )
mkdir -p "$here/../.scratch/server"
cd "$here/../.scratch/server"
RJAVA_MODULES=../jre25/lib/modules exec "$here/target/$profile/rjava" -Xmx2g -cp ../server-26.3.jar net.minecraft.bundler.Main --nogui "$@"
