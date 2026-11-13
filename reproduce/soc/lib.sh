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

# Shared helpers of reproduce/soc/*.sh (sourced, not run).
#
# Cluster layout below one root directory in the cluster home (NFS, shared by
# the x86-64 and aarch64 nodes):
#   <root>/code                      clone of this repository (sync.sh)
#   <root>/SIMD-Spielman-for-ZKPs    upstream checkout at the pinned commit
#                                    (test vectors for the fork's tests)
#   <root>/toolchain/<arch>/         rustup and Cargo homes per architecture
#   <root>/deps/                     Boost headers and their environment file
#   <root>/target/<class>-<session>  Cargo target directories (one per session,
#                                    so concurrent sessions never share one)
#   <root>/jobs/                     Slurm job stage lists and logs
#
# Workstation side, the login node is reached with "$RBD_SSH $RBD_LOGIN":
#   RBD_LOGIN    user@host of the login node (default xlogin.comp.nus.edu.sg)
#   RBD_SSH      ssh command, split into words (default "ssh"); for password
#                login e.g. "sshpass -f $HOME/.soc.pass ssh -o PubkeyAuthentication=no"
#   RBD_REMOTE_ROOT  root directory, relative to the cluster home (default rbd)

RBD_LOGIN=${RBD_LOGIN:-xlogin.comp.nus.edu.sg}
RBD_SSH=${RBD_SSH:-ssh}
RBD_REMOTE_ROOT=${RBD_REMOTE_ROOT:-rbd}
RBD_UPSTREAM_URL=https://github.com/flokrieger/SIMD-Spielman-for-ZKPs.git
RBD_UPSTREAM_COMMIT=7b399dbfecc406c79349015f1e21accc4def1be7
RBD_LCPC_VARIANTS_MAIN=as-published-as-is,fork-flags-portable

soc_log() {
    printf '[soc] %s\n' "$*" >&2
}

soc_die() {
    printf '[soc] ERROR: %s\n' "$*" >&2
    exit 1
}

# Run a shell command on the login node (workstation side).
soc_remote() {
    # shellcheck disable=SC2086 # RBD_SSH is a command with options.
    $RBD_SSH "$RBD_LOGIN" "$@"
}

# Run a POSIX sh script on the login node, independent of the login shell:
# soc_remote_sh SCRIPT [ARG...] (arguments must not contain single quotes).
soc_remote_sh() {
    _script=$(printf '%s' "$1" | sed "s/'/'\\\\''/g")
    shift
    _args=
    for _a in "$@"; do
        _args="$_args '$_a'"
    done
    soc_remote "sh -c '$_script' rbd$_args"
}

# Node class -> machine architecture (uname -m).
soc_class_arch() {
    case "$1" in
        xcnh) echo x86_64 ;;
        xcng) echo aarch64 ;;
        *) return 1 ;;
    esac
}

# Cluster side: per-architecture rustup/Cargo homes and Boost overrides.
# Argument: the cluster root directory.
soc_toolchain_env() {
    _arch=$(uname -m)
    RUSTUP_HOME="$1/toolchain/$_arch/rustup"
    CARGO_HOME="$1/toolchain/$_arch/cargo"
    PATH="$CARGO_HOME/bin:$PATH"
    export RUSTUP_HOME CARGO_HOME PATH
    if [ -f "$1/deps/simd-spielman.env" ]; then
        # shellcheck disable=SC1091
        . "$1/deps/simd-spielman.env"
    fi
}

# Split a stage spec 'NAME|REGEX|LOGN[|lcpc=LIST]' into SPEC_NAME, SPEC_REGEX,
# SPEC_LOGN and SPEC_LCPC (default: the second argument). REGEX
# may contain '|'; an empty REGEX or LOGN means no all.sh filter.
soc_parse_spec() {
    SPEC_LCPC=$2
    _spec=$1
    case "$_spec" in
        *'|lcpc='*)
            SPEC_LCPC=${_spec##*|lcpc=}
            _spec=${_spec%|lcpc=*}
            ;;
    esac
    SPEC_NAME=${_spec%%|*}
    _rest=${_spec#*|}
    SPEC_REGEX=${_rest%|*}
    SPEC_LOGN=${_rest##*|}
}

# Validate a stage spec; prints the problem and returns 1 if it is invalid.
soc_check_spec() {
    case "$1" in
        *'|'*'|'*) ;;
        *) printf "stage spec must be 'NAME|REGEX|LOGN[|lcpc=LIST]': %s\n" "$1" >&2; return 1 ;;
    esac
    soc_parse_spec "$1" "$RBD_LCPC_VARIANTS_MAIN"
    case "$SPEC_NAME" in
        ''|*[!A-Za-z0-9._-]*) printf 'invalid stage name in: %s\n' "$1" >&2; return 1 ;;
    esac
    case "$SPEC_LOGN" in
        *[!0-9,]*) printf 'invalid logN list in: %s\n' "$1" >&2; return 1 ;;
    esac
    case "$SPEC_LCPC" in
        ''|*[!a-z,-]*) printf 'invalid LCPC variant list in: %s\n' "$1" >&2; return 1 ;;
    esac
}
