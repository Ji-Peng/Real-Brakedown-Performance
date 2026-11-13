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

# Host profile: local-i7-11700k
# Workstation with Intel Core i7-11700K (Cypress Cove, 8 cores).
#
# CPU 2 is strictly reserved on this machine and MUST NEVER BE USED.
# Local runs are for testing, validation, and development ONLY;
# publishing benchmark numbers from this machine is forbidden.

HOST_NAME="local-i7-11700k"
HOST_IS_TEST_ONLY=1
HOST_FORBIDDEN_CORES="2"
HOST_DEFAULT_CORES="0,1,3-7"
HOST_DEFAULT_SINGLE_CORE="0"
HOST_ALLOWED_CORES="0,1,3-7"

# Local-only Boost headers used by reproduce/simd_spielman.sh when the system
# does not provide libboost-all-dev. The wrapper supplies the standard header
# <atomic>, which the upstream source assumes Boost includes transitively.
# Hosts with libboost-all-dev installed need neither override.
SIMD_SPIELMAN_EXTRA_INCLUDE=${SIMD_SPIELMAN_EXTRA_INCLUDE:-"$CODE_DIR/target-verify/deps/multiprecision/include:$CODE_DIR/target-verify/deps/config/include"}
SIMD_SPIELMAN_CXX_WRAPPER=${SIMD_SPIELMAN_CXX_WRAPPER:-"$CODE_DIR/target-verify/deps/gxx-include-atomic"}
export SIMD_SPIELMAN_EXTRA_INCLUDE SIMD_SPIELMAN_CXX_WRAPPER

host_validate_cores() {
    _cores_to_check="${1:-}"
    if [ -z "$_cores_to_check" ]; then
        return 0
    fi

    # Reject core 2 if present in any representation (e.g. "2", "0,2", "0-3", "2-5")
    if command -v python3 >/dev/null 2>&1; then
        python3 -c "
import sys

def parse_cores(core_str):
    cores = set()
    for part in core_str.split(','):
        part = part.strip()
        if not part:
            continue
        if '-' in part:
            lo, hi = part.split('-', 1)
            cores.update(range(int(lo), int(hi) + 1))
        else:
            cores.add(int(part))
    return cores

try:
    req = parse_cores('$1')
    if 2 in req:
        sys.exit(1)
except Exception:
    pass
" || {
            echo "[ERROR] Host profile '$HOST_NAME': CPU 2 is reserved for another task and MUST NOT be used!" >&2
            echo "[ERROR] Requested core list: '$_cores_to_check'" >&2
            return 1
        }
    else
        case ",${_cores_to_check}," in
            *,2,*|*,0-2,*|*,0-3,*|*,0-4,*|*,0-5,*|*,0-6,*|*,0-7,*|*,1-2,*|*,1-3,*|*,1-4,*|*,1-5,*|*,1-6,*|*,1-7,*|*,2-*)
                echo "[ERROR] Host profile '$HOST_NAME': CPU 2 is reserved for another task and MUST NOT be used!" >&2
                echo "[ERROR] Requested core list: '$_cores_to_check'" >&2
                return 1
                ;;
        esac
    fi
    return 0
}

host_check_mode() {
    echo "[INFO] Host profile: $HOST_NAME (TEST-ONLY: development and testing only; never publish numbers from this machine)"
}
