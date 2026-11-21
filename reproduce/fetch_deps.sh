#!/bin/sh
# Copyright 2026 RealBrakedownPerf Authors
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

# Idempotent dependency fetcher for RBP-LCPC.
# Clones pinned upstream baselines (SIMD-Spielman-for-ZKPs, arkworks poly-commit),
# downloads pinned Boost 1.86.0 headers when missing on x86_64, populates Cargo's
# cache with `cargo fetch --locked`, and writes deps/env.sh.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
CODE_DIR=$(CDPATH='' cd -- "$SCRIPT_DIR/.." && pwd)

if [ -f "${CARGO_HOME:-$HOME/.cargo}/env" ]; then
    # shellcheck disable=SC1091
    . "${CARGO_HOME:-$HOME/.cargo}/env"
fi

if [ -z "${RUSTUP_TOOLCHAIN:-}" ] && [ -f "$CODE_DIR/rust-toolchain.toml" ]; then
    _rbd_tc=$(sed -n 's/^[[:space:]]*channel[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$CODE_DIR/rust-toolchain.toml" | head -n 1)
    if [ -n "$_rbd_tc" ]; then
        RUSTUP_TOOLCHAIN="$_rbd_tc"
        export RUSTUP_TOOLCHAIN
    fi
fi

DEPS_DIR="${RBD_DEPS_DIR:-$CODE_DIR/deps}"
SKIP_STEPS_FETCH=0

SIMD_URL="https://github.com/flokrieger/SIMD-Spielman-for-ZKPs.git"
SIMD_COMMIT="7b399dbfecc406c79349015f1e21accc4def1be7"
POLY_URL="https://github.com/arkworks-rs/poly-commit.git"
POLY_COMMIT="1eaec5090ba5bc87d75a69570ed38f6fcf8b9417"

BOOST_VERSION="1.86.0"
BOOST_DIR_NAME="boost_1_86_0"
BOOST_URL="https://archives.boost.io/release/1.86.0/source/boost_1_86_0.tar.bz2"
BOOST_SHA256="1bed88e40401b2cb7a1f76d4bab499e352fa4d0c5f31c0dbae64e24d34d7513b"

usage() {
    cat <<EOF
Usage: $0 [--deps DIR] [--skip-steps-fetch]

Fetches pinned baselines and Cargo crates into DIR (default: \$RBD_DEPS_DIR or <repo>/deps)
and writes DIR/env.sh.
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --deps) [ $# -ge 2 ] || { echo "[ERROR] --deps requires an argument" >&2; exit 2; }; DEPS_DIR="$2"; shift 2 ;;
        --skip-steps-fetch) SKIP_STEPS_FETCH=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "[ERROR] Unknown option: $1" >&2; usage >&2; exit 2 ;;
    esac
done

mkdir -p "$DEPS_DIR"
DEPS_DIR=$(CDPATH='' cd -- "$DEPS_DIR" && pwd)

OFFLINE=0
if [ "${RBD_OFFLINE:-0}" = "1" ] || [ -f "$DEPS_DIR/.offline" ]; then
    OFFLINE=1
fi

log_info() { printf '[fetch_deps] %s\n' "$*"; }
log_warn() { printf '[fetch_deps] WARN: %s\n' "$*" >&2; }

SIMD_DIR="$DEPS_DIR/SIMD-Spielman-for-ZKPs"
POLY_DIR="$DEPS_DIR/poly-commit"
BOOST_ROOT_DIR="$DEPS_DIR/$BOOST_DIR_NAME"

# 1. SIMD-Spielman-for-ZKPs at pinned commit (required by C0 TV tests and C3)
TV_RESOLVED=""
if [ -d "$SIMD_DIR/.git" ] && [ "$(git -C "$SIMD_DIR" rev-parse HEAD 2>/dev/null || true)" = "$SIMD_COMMIT" ]; then
    log_info "SIMD-Spielman-for-ZKPs already at $SIMD_COMMIT"
    TV_RESOLVED="$SIMD_DIR/tv"
elif [ -f "$SIMD_DIR/tv/bd_spielman_extension_N12_in.txt" ] && [ "$OFFLINE" -eq 1 ]; then
    log_info "SIMD-Spielman-for-ZKPs test vectors present in $SIMD_DIR (offline mode)"
    TV_RESOLVED="$SIMD_DIR/tv"
elif [ -n "${SIMD_SPIELMAN_TV_DIR:-}" ] && [ -f "$SIMD_SPIELMAN_TV_DIR/bd_spielman_extension_N12_in.txt" ] && [ "$OFFLINE" -eq 1 ]; then
    log_info "Using pre-configured SIMD_SPIELMAN_TV_DIR=$SIMD_SPIELMAN_TV_DIR (offline mode)"
    TV_RESOLVED="$SIMD_SPIELMAN_TV_DIR"
elif [ "$OFFLINE" -eq 1 ]; then
    echo "[ERROR] Offline mode enabled but $SIMD_DIR is missing; run fetch_deps.sh online first." >&2
    exit 1
else
    log_info "Fetching SIMD-Spielman-for-ZKPs at $SIMD_COMMIT into $SIMD_DIR"
    if [ ! -d "$SIMD_DIR/.git" ]; then
        git clone --no-checkout "$SIMD_URL" "$SIMD_DIR"
    fi
    git -C "$SIMD_DIR" fetch --depth 1 origin "$SIMD_COMMIT"
    git -C "$SIMD_DIR" checkout --quiet --force --detach "$SIMD_COMMIT"
    TV_RESOLVED="$SIMD_DIR/tv"
fi

# 2. arkworks poly-commit at pinned commit
if [ -d "$POLY_DIR/.git" ] && [ "$(git -C "$POLY_DIR" rev-parse HEAD 2>/dev/null || true)" = "$POLY_COMMIT" ]; then
    log_info "arkworks poly-commit already at $POLY_COMMIT"
elif [ "$OFFLINE" -eq 0 ]; then
    _cargo_poly=""
    for _cand in "${CARGO_HOME:-$HOME/.cargo}"/git/checkouts/poly-commit-*/1eaec50; do
        [ -d "$_cand/.git" ] || continue
        if [ "$(git -C "$_cand" rev-parse HEAD 2>/dev/null || true)" = "$POLY_COMMIT" ]; then
            _cargo_poly="$_cand"
            break
        fi
    done
    if [ -n "$_cargo_poly" ]; then
        log_info "Cloning arkworks poly-commit from Cargo git cache $_cargo_poly"
        rm -rf "$POLY_DIR"
        git clone --quiet "$_cargo_poly" "$POLY_DIR"
        git -C "$POLY_DIR" checkout --quiet --force --detach "$POLY_COMMIT"
    else
        log_info "Fetching arkworks poly-commit at $POLY_COMMIT into $POLY_DIR"
        if [ ! -d "$POLY_DIR/.git" ]; then
            git clone --no-checkout "$POLY_URL" "$POLY_DIR"
        fi
        git -C "$POLY_DIR" fetch --depth 1 origin "$POLY_COMMIT"
        git -C "$POLY_DIR" checkout --quiet --force --detach "$POLY_COMMIT"
    fi
else
    log_warn "Offline mode: $POLY_DIR not present; polycommit.sh will rely on Cargo git cache"
fi

# 3. Boost 1.86.0 headers on x86_64 when system g++ lacks boost/multiprecision/cpp_int.hpp
boost_usable() {
    command -v g++ >/dev/null 2>&1 || return 1
    printf '#include <boost/multiprecision/cpp_int.hpp>\nint main() {}\n' | \
        g++ -x c++ -fsyntax-only ${1:+-I"$1"} - >/dev/null 2>&1
}

EXTRA_INC="${SIMD_SPIELMAN_EXTRA_INCLUDE:-}"
if [ "$(uname -m)" = "x86_64" ] && command -v g++ >/dev/null 2>&1; then
    if boost_usable; then
        log_info "System Boost headers are usable"
    elif [ -n "$EXTRA_INC" ] && boost_usable "$EXTRA_INC"; then
        log_info "Using Boost headers from SIMD_SPIELMAN_EXTRA_INCLUDE=$EXTRA_INC"
    elif [ -f "$BOOST_ROOT_DIR/boost/multiprecision/cpp_int.hpp" ] && boost_usable "$BOOST_ROOT_DIR"; then
        log_info "Using Boost $BOOST_VERSION headers in $BOOST_ROOT_DIR"
        EXTRA_INC="$BOOST_ROOT_DIR"
    elif [ "$OFFLINE" -eq 1 ]; then
        log_warn "Offline mode and Boost headers not found; SIMD-Spielman (C3) will be unavailable"
    else
        _archive="$DEPS_DIR/$BOOST_DIR_NAME.tar.bz2"
        if [ ! -f "$_archive" ] || ! printf '%s  %s\n' "$BOOST_SHA256" "$_archive" | sha256sum -c - >/dev/null 2>&1; then
            log_info "Downloading Boost $BOOST_VERSION headers"
            curl -fsSL -o "$_archive" "$BOOST_URL"
        fi
        printf '%s  %s\n' "$BOOST_SHA256" "$_archive" | sha256sum -c - >/dev/null || {
            echo "[ERROR] Boost archive SHA-256 mismatch: $_archive" >&2
            exit 1
        }
        tar -xjf "$_archive" -C "$DEPS_DIR" "$BOOST_DIR_NAME/boost" "$BOOST_DIR_NAME/LICENSE_1_0.txt"
        boost_usable "$BOOST_ROOT_DIR" || {
            echo "[ERROR] Unpacked Boost headers in $BOOST_ROOT_DIR are not usable by g++" >&2
            exit 1
        }
        EXTRA_INC="$BOOST_ROOT_DIR"
    fi
fi

# 4. Cargo dependency prefetch (--locked)
if [ "$OFFLINE" -eq 0 ] && command -v cargo >/dev/null 2>&1; then
    log_info "Running cargo fetch --locked for workspace"
    (cd "$CODE_DIR" && cargo fetch --locked)
    log_info "Running cargo fetch --locked for external/lcpc-orig"
    (cd / && cargo fetch --locked --manifest-path "$CODE_DIR/external/lcpc-orig/Cargo.toml")
    log_info "Running cargo fetch --locked for external/polycommit"
    (cd / && cargo fetch --locked --manifest-path "$CODE_DIR/external/polycommit/Cargo.toml")

    if [ "$SKIP_STEPS_FETCH" -eq 0 ] && [ -e "$CODE_DIR/.git" ]; then
        _step_tags=$(git -C "$CODE_DIR" tag --list 'step/*' | sort -V | awk '$0 != "step/00-import"')
        if [ -n "$_step_tags" ]; then
            _steps_stamp="$DEPS_DIR/.fetched-steps-$(printf '%s' "$_step_tags" | sha256sum | cut -c1-12)"
            if [ ! -f "$_steps_stamp" ]; then
                log_info "Fetching Cargo crates for step/* tags"
                _wt_base=$(mktemp -d "${TMPDIR:-/tmp}/rbd-fetch-steps.XXXXXX")
                _cur_wt=""
                _cleanup_wt() {
                    if [ -n "$_cur_wt" ] && [ -d "$_cur_wt" ]; then
                        git -C "$CODE_DIR" worktree remove --force "$_cur_wt" >/dev/null 2>&1 || rm -rf "$_cur_wt"
                    fi
                    rm -rf "$_wt_base"
                }
                trap _cleanup_wt EXIT INT TERM
                for _tag in $_step_tags; do
                    _cur_wt="$_wt_base/wt"
                    git -C "$CODE_DIR" worktree add --quiet --detach "$_cur_wt" "$_tag"
                    if [ -f "$_cur_wt/Cargo.lock" ]; then
                        (cd "$_cur_wt" && cargo fetch --locked) || log_warn "cargo fetch failed for $_tag"
                    fi
                    git -C "$CODE_DIR" worktree remove --force "$_cur_wt" >/dev/null 2>&1 || rm -rf "$_cur_wt"
                    _cur_wt=""
                done
                trap - EXIT INT TERM
                rm -rf "$_wt_base"
                : > "$_steps_stamp"
            else
                log_info "step/* Cargo crates already fetched"
            fi
        fi
    fi
fi

# 5. Write deps/env.sh
ENV_FILE="$DEPS_DIR/env.sh"
{
    printf '# Generated by reproduce/fetch_deps.sh; sourced by reproduce scripts.\n'
    printf 'RBD_DEPS_DIR=%s\n' "'$DEPS_DIR'"
    printf 'export RBD_DEPS_DIR\n'
    if [ -n "$TV_RESOLVED" ]; then
        printf 'SIMD_SPIELMAN_TV_DIR=%s\n' "'$TV_RESOLVED'"
        printf 'export SIMD_SPIELMAN_TV_DIR\n'
    fi
    if [ -n "$EXTRA_INC" ]; then
        printf 'SIMD_SPIELMAN_EXTRA_INCLUDE=%s\n' "'$EXTRA_INC'"
        printf 'export SIMD_SPIELMAN_EXTRA_INCLUDE\n'
    fi
} > "$ENV_FILE.tmp"
mv "$ENV_FILE.tmp" "$ENV_FILE"
log_info "Wrote $ENV_FILE"
