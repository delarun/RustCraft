#!/bin/sh
# CMake compiler launcher: drops -fPIC/-fpic so wasm objects are non-PIC (static link into rust-lld output).
cmd="$1"; shift
for a in "$@"; do shift; case "$a" in -fPIC|-fpic|-fPIE|-fpie) ;; *) set -- "$@" "$a";; esac; done
exec "$cmd" "$@"
