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

# Host profile: SoC cluster node class xcnh (2x AMD EPYC 9755, Zen 5, SMT 2,
# L3 shared by 8 physical cores). The benchmark set is the 8 physical cores of
# one L3 domain that the Slurm job owns entirely (request -c 16
# --threads-per-core=1); loading fails if the job owns no such domain.
# Cores are derived from the job's CPU affinity and cgroup cpuset, never from
# all online CPUs of the shared node (reproduce/soc/host_profile.py). The node
# record (Slurm job, CPU model, cache topology, governor, boost, load, the
# node's allocated CPUs, perf availability) is written to
# results/soc-xcnh/host-profile-<hostname>.json.

HOST_NAME=soc-xcnh
HOST_IS_TEST_ONLY=0
HOST_FORBIDDEN_CORES=
HOST_DEFAULT_CORES=
HOST_DEFAULT_SINGLE_CORE=
HOST_ALLOWED_CORES=
HOST_PERF_STATUS=

_soc_load_host_profile() {
    _soc_manifest="$CODE_DIR/results/$HOST_NAME/host-profile-$(host_instance_name).json"
    _soc_info=$(python3 "$CODE_DIR/reproduce/soc/host_profile.py" --node-class xcnh \
        --host-profile "$HOST_NAME" --manifest "$_soc_manifest") || {
        printf '%s\n' "[ERROR] Host profile $HOST_NAME: no benchmark core set for this job (see above)." >&2
        return 1
    }
    IFS='|' read -r HOST_DEFAULT_SINGLE_CORE HOST_DEFAULT_CORES HOST_ALLOWED_CORES _soc_domain <<SOC_EOF
$_soc_info
SOC_EOF
    if [ -z "$HOST_DEFAULT_SINGLE_CORE" ] || [ -z "$HOST_DEFAULT_CORES" ] || [ -z "$HOST_ALLOWED_CORES" ]; then
        printf '%s\n' "[ERROR] Host profile $HOST_NAME returned incomplete core data." >&2
        return 1
    fi
    HOST_PERF_STATUS=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["perf"]["status"])' "$_soc_manifest" 2>/dev/null || echo unknown)
    printf '[INFO] %s cores: single=%s, eight-core=%s, allowed=%s, L3 domain=%s, perf: %s\n' \
        "$HOST_NAME" "$HOST_DEFAULT_SINGLE_CORE" "$HOST_DEFAULT_CORES" "$HOST_ALLOWED_CORES" \
        "$_soc_domain" "$HOST_PERF_STATUS" >&2
    printf '[INFO] Node record: %s\n' "$_soc_manifest" >&2
}

_soc_load_host_profile || exit 1
export HOST_PERF_STATUS

# shellcheck disable=SC2317 # Called by reproduce/common.sh after sourcing this profile.
host_check_mode() {
    printf '[INFO] Host profile: %s (cores derived from the Slurm job allocation)\n' "$HOST_NAME"
}

# shellcheck disable=SC2317 # Called by reproduce/all.sh before and after each block.
host_noise_snapshot() {
    python3 "$CODE_DIR/reproduce/soc/host_profile.py" --noise 2>/dev/null || printf '{}\n'
}
