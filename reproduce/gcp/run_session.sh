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
STAGE_ARGS=""
KEEP_RUNNING=0
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
  5. Copies back to code/results/ only what this all.sh run wrote: its stage
     dir, the session-level schedule.json/failed_blocks.tsv and the output
     dirs of the blocks it ran (from the stage's copyback.tsv; never other run
     dirs or sessions on the VM), plus this VM's per-instance host-profile and
     first-boot records. A local copy of a re-run block is moved to the same
     <block>.prev-<UTC> name the VM used, so local and remote layouts agree.
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
  --blocks/--logn/--warmup/--lcpc-variants/--stage/--run-date ARG
                           Passed to all.sh to run one stage of a staged campaign
  --keep-running           Leave the VM running when this script exits, whatever the
                           outcome (the caller starts the next stage and stops the VM);
                           the exit status is still that of the stage
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
        --blocks|--logn|--warmup|--lcpc-variants|--stage|--run-date)
            [ "$#" -ge 2 ] || { log_error "$1 requires an argument"; exit 2; }
            STAGE_ARGS="$STAGE_ARGS $1 '$2'"
            shift 2
            ;;
        --keep-running)
            KEEP_RUNNING=1
            shift
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
    if [ "$KEEP_RUNNING" -eq 1 ]; then
        log_info "Stage finished (exit $_rc); VM '$INSTANCE' left running for the next stage (--keep-running)."
        exit "$_rc"
    fi
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
REMOTE_COPYBACK="/tmp/rbd-session-${SESSION_ID}.copyback"

log_info "Clearing remote status markers..."
"$GCLOUD" compute ssh "$INSTANCE" --zone="$ZONE" \
    --command "rm -f '$REMOTE_LOG' '$REMOTE_STATUS' '$REMOTE_PID' '$REMOTE_COPYBACK'"

if [ "$MACHINE" = c3d ]; then REMOTE_HOST_PROFILE=gcp-c3d-highmem-16; else REMOTE_HOST_PROFILE=gcp-c4a-highmem-16; fi
REMOTE_ALL_ARGS="--profile $PROFILE --session $SESSION_ID --host $REMOTE_HOST_PROFILE"
if [ -n "$SEED" ]; then
    REMOTE_ALL_ARGS="$REMOTE_ALL_ARGS --seed $SEED"
fi
REMOTE_ALL_ARGS="$REMOTE_ALL_ARGS$STAGE_ARGS"

RUN_CMD="cd '$REMOTE_DIR_ACTUAL' && . \"\$HOME/.cargo/env\" && RBD_COPYBACK_FILE='$REMOTE_COPYBACK' ./reproduce/all.sh $REMOTE_ALL_ARGS > '$REMOTE_LOG' 2>&1; echo \$? > '$REMOTE_STATUS'"

log_info "Launching remote benchmark campaign via nohup..."
# POSIX sh has no printf %q: ship the command as a remote script over stdin.
REMOTE_SCRIPT="/tmp/rbd-session-${SESSION_ID}.sh"
printf '%s\n' "$RUN_CMD" | "$GCLOUD" compute ssh "$INSTANCE" --zone="$ZONE" \
    --command "cat > '$REMOTE_SCRIPT'"
"$GCLOUD" compute ssh "$INSTANCE" --zone="$ZONE" \
    --command "nohup sh '$REMOTE_SCRIPT' >/dev/null 2>&1 & echo \$! > '$REMOTE_PID'"

REMOTE_PID_VAL=$("$GCLOUD" compute ssh "$INSTANCE" --zone="$ZONE" --command "cat '$REMOTE_PID' 2>/dev/null || echo unknown")
log_info "Remote job launched (PID: $REMOTE_PID_VAL). Streaming log output until completion..."

# 5. Poll for remote completion
DONE=0
SESSION_EXIT_CODE=1
while [ "$DONE" -eq 0 ]; do
    sleep 60
    # Print recent log lines from remote execution
    "$GCLOUD" compute ssh "$INSTANCE" --zone="$ZONE" \
        --command "tail -n 20 '$REMOTE_LOG' 2>/dev/null || true" || true

    # Check for completion status file
    # Tolerate transient SSH failures: under set -e a failed poll would exit and
    # the EXIT trap would stop the VM, killing the remote session.
    STATUS_OUTPUT=$("$GCLOUD" compute ssh "$INSTANCE" --zone="$ZONE" \
        --command "cat '$REMOTE_STATUS' 2>/dev/null || true" 2>/dev/null) || STATUS_OUTPUT=""
    if [ -n "$STATUS_OUTPUT" ]; then
        DONE=1
        SESSION_EXIT_CODE="$STATUS_OUTPUT"
    fi
done

log_info "Remote campaign process completed with exit code $SESSION_EXIT_CODE."

# 6. Copy results back to local code/results/: only what this all.sh run
# wrote (listed in its stage's copyback.tsv, see reproduce/all.sh) and this
# VM's per-instance evidence files, never other run dirs or sessions that may
# be on the VM's disk (stale or invalid runs, other sessions' copies).
log_info "Copying this stage's results back to the local directory..."
mkdir -p "$CODE_DIR/results"
COPY_FAILED=0
COPYBACK_LIST=$("$GCLOUD" compute ssh "$INSTANCE" --zone="$ZONE" --command \
    "f=\$(cat '$REMOTE_COPYBACK' 2>/dev/null) && cat \"\$f\"" 2>/dev/null | tr -d '\r') || COPYBACK_LIST=""
if [ -z "$COPYBACK_LIST" ]; then
    log_warn "all.sh wrote no copy-back list ($REMOTE_COPYBACK); nothing copied. The results stay on the VM."
    COPY_FAILED=1
fi

safe_rel_path() {
    case "$1" in
        ''|-|*[!A-Za-z0-9._/-]*|/*|*..*) return 1 ;;
    esac
    return 0
}
copy_remote() {
    # $1: path relative to results/; copied into the same place locally.
    _local_parent="$CODE_DIR/results/$(dirname -- "$1")"
    mkdir -p "$_local_parent"
    "$GCLOUD" compute scp --recurse --zone="$ZONE" \
        "$INSTANCE:$REMOTE_DIR_ACTUAL/results/$1" "$_local_parent/" || COPY_FAILED=1
}

# The session-level schedule.json is append-only. A session may have run
# stages on more than one VM, so merge the VM's stage entries into the local
# file (same stage tag: the VM's entry wins) instead of overwriting it.
merge_session_schedule() {
    _incoming=$(mktemp -d "${TMPDIR:-/tmp}/rbd-schedule.XXXXXX") || { COPY_FAILED=1; return; }
    if "$GCLOUD" compute scp --zone="$ZONE" \
        "$INSTANCE:$REMOTE_DIR_ACTUAL/results/$1" "$_incoming/schedule.json"; then
        mkdir -p "$CODE_DIR/results/$(dirname -- "$1")"
        python3 - "$_incoming/schedule.json" "$CODE_DIR/results/$1" <<'PY' || COPY_FAILED=1
import json, os, sys
incoming_path, local_path = sys.argv[1], sys.argv[2]
with open(incoming_path, encoding="utf-8") as f:
    incoming = json.load(f)
try:
    with open(local_path, encoding="utf-8") as f:
        local = json.load(f)
except (OSError, ValueError):
    local = None
if isinstance(local, dict) and "stages" in local and "stages" in incoming:
    tags = {e.get("stage") for e in incoming["stages"]}
    kept = [e for e in local["stages"] if e.get("stage") not in tags]
    incoming["stages"] = sorted(kept + incoming["stages"], key=lambda e: (e.get("started_utc") or "", e.get("stage") or ""))
    for p in local.get("profiles", []):
        if p not in incoming.setdefault("profiles", []):
            incoming["profiles"].append(p)
    if "legacy_schedule" in local:
        incoming.setdefault("legacy_schedule", local["legacy_schedule"])
elif isinstance(local, dict) and "stages" not in local:
    incoming.setdefault("legacy_schedule", local)
tmp = local_path + ".tmp"
with open(tmp, "w", encoding="utf-8") as f:
    json.dump(incoming, f, indent=2)
    f.write("\n")
os.replace(tmp, local_path)
PY
    else
        COPY_FAILED=1
    fi
    rm -rf "$_incoming"
}

TAB=$(printf '\t')
COPYBACK_FILE_LOCAL=$(mktemp "${TMPDIR:-/tmp}/rbd-copyback.XXXXXX") || exit 1
printf '%s\n' "$COPYBACK_LIST" > "$COPYBACK_FILE_LOCAL"
while IFS="$TAB" read -r kind rel prev exists; do
    [ -n "$kind" ] || continue
    if ! safe_rel_path "$rel"; then
        log_warn "Skipping unexpected copy-back path: $rel"
        COPY_FAILED=1
        continue
    fi
    case "$kind" in
        session)
            mkdir -p "$CODE_DIR/results/$rel"
            ;;
        file)
            case "$rel" in
                */schedule.json) merge_session_schedule "$rel" ;;
                *) copy_remote "$rel" ;;
            esac
            ;;
        stage)
            copy_remote "$rel"
            ;;
        block)
            local_block="$CODE_DIR/results/$rel"
            if [ -e "$local_block" ]; then
                # The VM moved its earlier copy of this block to $prev; do
                # the same here (or use a local .prev- name) before copying.
                if safe_rel_path "$prev" && [ ! -e "$CODE_DIR/results/$prev" ]; then
                    local_prev="$CODE_DIR/results/$prev"
                else
                    local_prev="$local_block.prev-local-$(date -u +%Y%m%dT%H%M%SZ)"
                fi
                mv "$local_block" "$local_prev" || { COPY_FAILED=1; continue; }
                log_info "Moved local $(basename "$local_block") to $(basename "$local_prev")"
            elif safe_rel_path "$prev" && [ ! -e "$CODE_DIR/results/$prev" ]; then
                copy_remote "$prev"
            fi
            if [ "$exists" = 1 ]; then
                copy_remote "$rel"
            fi
            ;;
        *)
            log_warn "Unknown copy-back entry: $kind"
            ;;
    esac
done < "$COPYBACK_FILE_LOCAL"
rm -f "$COPYBACK_FILE_LOCAL"

# This VM's per-instance evidence files.
REMOTE_EVIDENCE=$("$GCLOUD" compute ssh "$INSTANCE" --zone="$ZONE" --command \
    "cd '$REMOTE_DIR_ACTUAL/results' 2>/dev/null && ls -d '$REMOTE_HOST_PROFILE'/host-profile-'$INSTANCE'.json '$REMOTE_HOST_PROFILE'/first-boot-manifest-'$INSTANCE'.json 2>/dev/null || true" | tr -d '\r') || REMOTE_EVIDENCE=""
for remote_path in $REMOTE_EVIDENCE; do
    if safe_rel_path "$remote_path"; then
        copy_remote "$remote_path"
    else
        log_warn "Skipping unexpected remote path: $remote_path"
    fi
done
if [ "$COPY_FAILED" -ne 0 ]; then
    log_warn "Some results failed to copy back from $INSTANCE."
    [ "$SESSION_EXIT_CODE" -ne 0 ] || SESSION_EXIT_CODE=1
fi

if [ "$SESSION_EXIT_CODE" -ne 0 ]; then
    log_error "Remote session $SESSION_ID failed with exit code $SESSION_EXIT_CODE."
    exit "$SESSION_EXIT_CODE"
fi

log_info "=========================================================="
log_info "Session $SESSION_ID finished successfully!"
log_info "Results copied to: $CODE_DIR/results/$(printf '%s\n' "$COPYBACK_LIST" | awk -F '\t' '$1 == "session" { print $2; exit }')"
log_info "=========================================================="
exit 0
