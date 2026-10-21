#!/bin/sh
# Copyright 2026 RealBrakedownPerf Authors
# Licensed under Apache-2.0.

# The pinned upstream LCPC oracle has an unconditional `#![feature(test)]`.
# Enable its stable public API build with crate-scoped bootstrap only for the
# source file in Cargo's checkout of the pinned upstream revision. In
# particular, do not pass bootstrap to the fork crate with the same crate name.

set -eu

if [ "$#" -lt 1 ]; then
    echo "rustc-oracle-wrapper: expected rustc path and arguments" >&2
    exit 2
fi

rustc=$1
shift

is_upstream_brakedown_pc=0
expects_crate_name=0
is_brakedown_crate=0

for arg do
    if [ "$expects_crate_name" -eq 1 ]; then
        if [ "$arg" = "lcpc_brakedown_pc" ]; then
            is_brakedown_crate=1
        fi
        expects_crate_name=0
    fi

    case "$arg" in
        --crate-name)
            expects_crate_name=1
            ;;
        */git/checkouts/lcpc-*/f0e2116/lcpc-brakedown-pc/src/lib.rs)
            is_upstream_brakedown_pc=1
            ;;
    esac
done

if [ "$is_upstream_brakedown_pc" -eq 1 ] && [ "$is_brakedown_crate" -eq 1 ]; then
    RUSTC_BOOTSTRAP=lcpc_brakedown_pc exec "$rustc" "$@"
fi

# Keep this workspace's fork and all other crates on the normal stable gates,
# even if the caller's environment exports a broad RUSTC_BOOTSTRAP value.
unset RUSTC_BOOTSTRAP
exec "$rustc" "$@"
