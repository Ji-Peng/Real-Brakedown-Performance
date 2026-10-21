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

set -eu

# If gcloud is not on PATH on the workstation, set GCLOUD to
# "$HOME/.local/share/google-cloud-sdk/bin/gcloud".
GCLOUD=${GCLOUD:-gcloud}
MACHINE=${MACHINE:-c3d}
SESSION_ID=${SESSION_ID:-}
PROFILE=${PROFILE:-}
ZONE=${ZONE:-}  # default per machine: c3d -> us-central1-c, c4a -> us-central1-a
INSTANCE=${INSTANCE:-}
REMOTE_CODE_DIR=${REMOTE_CODE_DIR:-}
SEED=${SEED:-}
SKIP_SYNC=0
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
CODE_DIR=$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)

log_info() {
    printf '[INFO] %s\n' "$*"
}

log_warn() {
    printf '[WARN] %s\n' "$*" >&2
}

log_error() {
    printf '[ERROR] %s\n' "$*" >&2
}

usage() {
    cat <<EOF
Usage: $0 [--machine c3d|c4a] [--session <id>] [--profile full|arm|smoke] [options]

Orchestrates a complete benchmark session on a Google Cloud VM per plan 06 §6 item 9:
  1. Starts the VM and waits for SSH connectivity.
  2. Syncs code/ and verifies first-boot checks via setup_vm.sh.
  3. Launches reproduce/all.sh with the chosen profile inside nohup remotely.
  4. Polls log output until completion (safe against dropped SSH connections).
  5. Copies results/ back to local code/results/.
  6. ALWAYS stops the VM on exit (via trap EXIT INT TERM) to minimize costs,
     and prints a reminder to delete the VM when all sessions finish.

Options:
  --machine c3d|c4a        Machine family (default: c3d)
  --session <id>           Session identifier (e.g. s1, s2; default: session-YYYYMMDD-HHMMSS)
  --profile full|arm|smoke Benchmark workload profile (default: full for c3d, arm for c4a)
  --zone ZONE              GCP zone (default: us-central1-c for c3d, us-central1-a for c4a)
  --instance NAME          Override VM instance name
  --remote-dir PATH        Remote checkout directory (default: ~/RealBrakedownPerf/code)
  --seed INT               Random seed for block interleaving order
  --skip-sync              Skip setup_vm.sh code sync
  -h, --help               Show this help message

If gcloud is not on PATH, set GCLOUD to
  \$HOME/.local/share/google-cloud-sdk/bin/gcloud
EOF
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --machine)
            [ "$#" -ge 2 ] || { log_error "--machine requires an argument (c3d|c4a)"; exit 2; }
            MACHINE=$2
            shift 2
            ;;
        --session)
            [ "$#" -ge 2 ] || { log_error "--session requires an argument"; exit 2; }
            SESSION_ID=$2
            shift 2
            ;;
        --profile)
            [ "$#" -ge 2 ] || { log_error "--profile requires an argument (full|arm|smoke)"; exit 2; }
            PROFILE=$2
            shift 2
            ;;
        --zone)
            [ "$#" -ge 2 ] || { log_error "--zone requires a zone"; exit 2; }
            ZONE=$2
            shift 2
            ;;
        --instance)
            [ "$#" -ge 2 ] || { log_error "--instance requires a name"; exit 2; }
            INSTANCE=$2
            shift 2
            ;;
        --remote-dir)
            [ "$#" -ge 2 ] || { log_error "--remote-dir requires an absolute path"; exit 2; }
            REMOTE_CODE_DIR=$2
            shift 2
            ;;
        --seed)
            [ "$#" -ge 2 ] || { log_error "--seed requires an integer"; exit 2; }
            SEED=$2
            shift 2
            ;;
        --skip-sync)
            SKIP_SYNC=1
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            log_error "Unknown option: $1"
            usage >&2
            exit 2
            ;;
    esac
done

[ -n "$ZONE" ] || { if [ "$MACHINE" = c3d ]; then ZONE=us-central1-c; else ZONE=us-central1-a; fi; }
case "$MACHINE" in
    c3d)
        DEFAULT_INSTANCE=rbd-bench-c3d
        DEFAULT_PROFILE=full
        ;;
    c4a)
        DEFAULT_INSTANCE=rbd-bench-c4a
        DEFAULT_PROFILE=arm
        ;;
    *)
        log_error "Invalid --machine '$MACHINE'; must be c3d or c4a"
        exit 2
        ;;
esac

INSTANCE=${INSTANCE:-$DEFAULT_INSTANCE}
PROFILE=${PROFILE:-$DEFAULT_PROFILE}
if [ -z "$SESSION_ID" ]; then
    SESSION_ID="session-$(date -u +%Y%m%d-%H%M%S)"
fi

case "$PROFILE" in
    full)
        if [ "$MACHINE" = "c4a" ]; then
            log_error "Profile 'full' cannot be run on c4a (c4a lacks AVX-512 for SIMD-Spielman). Use --profile arm."
            exit 2
        fi
        ;;
    arm)
        ;;
    smoke)
        ;;
    *)
        log_error "Invalid --profile '$PROFILE'; must be full, arm, or smoke"
        exit 2
        ;;
esac

case "$INSTANCE" in
    ''|*[!A-Za-z0-9._-]*) log_error "Invalid instance name: $INSTANCE"; exit 2 ;;
esac
case "$ZONE" in
    ''|*[!A-Za-z0-9._-]*) log_error "Invalid zone: $ZONE"; exit 2 ;;
esac
case "$SESSION_ID" in
    ''|*[!A-Za-z0-9._-]*) log_error "Invalid session id: $SESSION_ID"; exit 2 ;;
esac

if ! command -v "$GCLOUD" >/dev/null 2>&1; then
    log_error "gcloud command not found: $GCLOUD"
    log_error 'Set GCLOUD="$HOME/.local/share/google-cloud-sdk/bin/gcloud" if needed.'
    exit 127
fi

# ALWAYS stop VM on exit (cost-minimization rule)
cleanup() {
    _rc=$?
    trap '' EXIT INT TERM  # Avoid recursive trap handling
    printf '\n'
    log_info "Cost-minimization rule: stopping VM '$INSTANCE' in zone '$ZONE'..."
    "$GCLOUD" compute instances stop "$INSTANCE" --zone="$ZONE" --quiet >/dev/null 2>&1 || true
    cat <<EOF

=======================================================================
REMINDER (Google Cloud VM Cost Policy):
VM '$INSTANCE' in '$ZONE' has been stopped.
Note: A stopped VM does not incur CPU/RAM charges, but its boot disk
continues to be billed until deleted.
Once all benchmark sessions are finished, delete the VM and its disk:
  reproduce/gcp/delete_vm.sh --machine $MACHINE --zone $ZONE
=======================================================================
EOF
    exit "$_rc"
}
trap cleanup EXIT INT TERM

log_info "=========================================================="
log_info "Benchmark Session Orchestrator (run_session.sh)"
log_info "Machine:     $MACHINE ($INSTANCE in $ZONE)"
log_info "Profile:     $PROFILE"
log_info "Session ID:  $SESSION_ID"
log_info "Seed:        ${SEED:-(auto-generated)}"
log_info "=========================================================="

# 1. Start VM if not running
log_info "Checking VM status for '$INSTANCE'..."
CURRENT_STATUS=$("$GCLOUD" compute instances describe "$INSTANCE" --zone="$ZONE" --format="value(status)" 2>/dev/null || echo "NOT_FOUND")
if [ "$CURRENT_STATUS" = "NOT_FOUND" ]; then
    log_error "VM instance '$INSTANCE' does not exist in zone '$ZONE'."
    log_error "Run reproduce/gcp/create_vm.sh --machine $MACHINE --yes first."
    exit 1
fi

if [ "$CURRENT_STATUS" != "RUNNING" ]; then
    log_info "VM status is $CURRENT_STATUS. Starting VM '$INSTANCE'..."
    "$GCLOUD" compute instances start "$INSTANCE" --zone="$ZONE"
else
    log_info "VM '$INSTANCE' is already running."
fi

# 2. Wait for SSH availability
log_info "Waiting for SSH to become ready on '$INSTANCE'..."
SSH_READY=0
for attempt in $(seq 1 60); do
    if "$GCLOUD" compute ssh "$INSTANCE" --zone="$ZONE" --command "true" >/dev/null 2>&1; then
        SSH_READY=1
        break
    fi
    sleep 5
done
if [ "$SSH_READY" -ne 1 ]; then
    log_error "Timed out waiting for SSH on '$INSTANCE' after 300 seconds."
    exit 1
fi
log_info "SSH connection confirmed."

# 3. Sync code and run first-boot environment checks
if [ "$SKIP_SYNC" -eq 0 ]; then
    log_info "Synchronizing code and running first-boot checks via setup_vm.sh..."
    SETUP_ARGS="--machine $MACHINE --instance $INSTANCE --zone $ZONE"
    if [ -n "$REMOTE_CODE_DIR" ]; then
        SETUP_ARGS="$SETUP_ARGS --remote-dir $REMOTE_CODE_DIR"
    fi
    # shellcheck disable=SC2086
    "$SCRIPT_DIR/setup_vm.sh" $SETUP_ARGS
fi

REMOTE_DIR_ACTUAL=${REMOTE_CODE_DIR:-~/RealBrakedownPerf/code}

# 4. Launch remote execution in background (nohup)
REMOTE_LOG="/tmp/rbd-session-${SESSION_ID}.log"
REMOTE_STATUS="/tmp/rbd-session-${SESSION_ID}.status"
REMOTE_PID="/tmp/rbd-session-${SESSION_ID}.pid"

log_info "Clearing remote status markers..."
"$GCLOUD" compute ssh "$INSTANCE" --zone="$ZONE" \
    --command "rm -f '$REMOTE_LOG' '$REMOTE_STATUS' '$REMOTE_PID'"

if [ "$MACHINE" = c3d ]; then REMOTE_HOST_PROFILE=gcp-c3d-highmem-16; else REMOTE_HOST_PROFILE=gcp-c4a-highmem-16; fi
REMOTE_ALL_ARGS="--profile $PROFILE --session $SESSION_ID --host $REMOTE_HOST_PROFILE"
if [ -n "$SEED" ]; then
    REMOTE_ALL_ARGS="$REMOTE_ALL_ARGS --seed $SEED"
fi

RUN_CMD="cd '$REMOTE_DIR_ACTUAL' && . \"\$HOME/.cargo/env\" && ./reproduce/all.sh $REMOTE_ALL_ARGS > '$REMOTE_LOG' 2>&1; echo \$? > '$REMOTE_STATUS'"

log_info "Launching remote benchmark campaign via nohup..."
"$GCLOUD" compute ssh "$INSTANCE" --zone="$ZONE" \
    --command "nohup sh -c $(printf '%q' "$RUN_CMD") >/dev/null 2>&1 & echo \$! > '$REMOTE_PID'"

REMOTE_PID_VAL=$("$GCLOUD" compute ssh "$INSTANCE" --zone="$ZONE" --command "cat '$REMOTE_PID' 2>/dev/null || echo unknown")
log_info "Remote job launched (PID: $REMOTE_PID_VAL). Streaming log output until completion..."

# 5. Poll for remote completion
DONE=0
SESSION_EXIT_CODE=1
while [ "$DONE" -eq 0 ]; do
    sleep 15
    # Print recent log lines from remote execution
    "$GCLOUD" compute ssh "$INSTANCE" --zone="$ZONE" \
        --command "tail -n 20 '$REMOTE_LOG' 2>/dev/null || true" || true

    # Check for completion status file
    STATUS_OUTPUT=$("$GCLOUD" compute ssh "$INSTANCE" --zone="$ZONE" \
        --command "cat '$REMOTE_STATUS' 2>/dev/null || true")
    if [ -n "$STATUS_OUTPUT" ]; then
        DONE=1
        SESSION_EXIT_CODE="$STATUS_OUTPUT"
    fi
done

log_info "Remote campaign process completed with exit code $SESSION_EXIT_CODE."

# 6. Copy results back to local code/results/
log_info "Copying results back to local directory..."
mkdir -p "$CODE_DIR/results"
"$GCLOUD" compute scp --recurse --zone="$ZONE" \
    "$INSTANCE:$REMOTE_DIR_ACTUAL/results/*" "$CODE_DIR/results/" || {
    log_warn "Some results failed to copy back from $INSTANCE."
}

if [ "$SESSION_EXIT_CODE" -ne 0 ]; then
    log_error "Remote session $SESSION_ID failed with exit code $SESSION_EXIT_CODE."
    exit "$SESSION_EXIT_CODE"
fi

log_info "=========================================================="
log_info "Session $SESSION_ID finished successfully!"
log_info "Results copied to: $CODE_DIR/results/"
log_info "=========================================================="
exit 0
