#!/usr/bin/env bash
# scripts/run-matrix.sh — wrapper around the fs-test-harness matrix
# runner that cleans up disk images on exit (success, failure, Ctrl-C,
# or any signal).
#
# Why a wrapper instead of fixing the harness directly:
# * The harness lives in `vendor/fs-test-harness/` (git submodule);
#   we don't own its lifecycle. Cleanup belongs in consumer code.
# * `init-image` (and ship-to-host) create .img files under HOST_IMAGE_DIR
#   and have no opinion about who removes them. Without this trap the
#   directory accumulates stale images across runs.
#
# Multi-instance safety:
# * The harness generates a unique run_id (ms timestamp) per invocation
#   so parallel run-matrix.sh instances write to separate subdirectories
#   under HOST_IMAGE_DIR and cannot trample each other's images.
# * A mkdir-based lock keyed on the scenario filter prevents two
#   instances from running the same scenario set simultaneously.
#   Stale locks (from killed processes) can be removed with:
#     rm -rf /tmp/ext4-matrix-lock-*
#
# Usage: same as `vendor/fs-test-harness/scripts/run-tests.sh`. All
# arguments pass straight through. Examples:
#
#   bash scripts/run-matrix.sh                  # full matrix
#   bash scripts/run-matrix.sh basic-ro-list    # substring filter
#   bash scripts/run-matrix.sh --list           # don't run, just list
#   bash scripts/run-matrix.sh --keep-images    # skip the cleanup trap
#                                                 (handy for byte-diff
#                                                  inspection)
#
# Exit codes: pass-through from the harness runner. The trap fires
# regardless of exit code.

set -uo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root" || {
    echo "[run-matrix] fatal: cannot cd to repo_root=$repo_root" >&2
    exit 1
}

ssh_mux_socket="/tmp/ext4-ssh-mux-$$"
ssh_wrapper_dir=""
ssh_mux_pid=""

# Allow `--keep-images` to opt out of cleanup. Strip it before
# forwarding so the harness runner doesn't see an unknown flag.
keep_images=0
forwarded_args=()
for arg in "$@"; do
    case "$arg" in
        --keep-images) keep_images=1 ;;
        *) forwarded_args+=("$arg") ;;
    esac
done

# ── Resolve image directory ─────────────────────────────────────────────────
# Read HOST_IMAGE_DIR from .test-env (same source the harness uses).
# Default: diskimages/ relative to the repo root.
host_image_dir=""
if [ -f "$repo_root/.test-env" ]; then
    host_image_dir=$(grep '^HOST_IMAGE_DIR=' "$repo_root/.test-env" 2>/dev/null | cut -d= -f2- | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')
fi
host_image_dir="${host_image_dir:-diskimages}"
# Resolve relative paths against repo_root.
if [[ "$host_image_dir" != /* ]]; then
    host_image_dir="$repo_root/$host_image_dir"
fi
mkdir -p "$host_image_dir"

# ── Per-scenario-filter lock ────────────────────────────────────────────────
# Prevent two invocations with the same scenario filter from racing.
# Uses mkdir atomicity. The lock key is the normalised filter string
# (or "full" for no filter). Stale locks: rm -rf /tmp/ext4-matrix-lock-*
lock_key="${forwarded_args[*]:-full}"
lock_key="${lock_key//[^a-zA-Z0-9_-]/_}"
scenario_lock="/tmp/ext4-matrix-lock-${lock_key}"
if ! mkdir "$scenario_lock" 2>/dev/null; then
    existing_pid=$(cat "$scenario_lock/pid" 2>/dev/null || echo "?")
    echo "[run-matrix] scenario filter '${lock_key}' is already running (pid ${existing_pid})" >&2
    echo "[run-matrix] if stale: rm -rf ${scenario_lock}" >&2
    exit 1
fi
echo "$$" > "$scenario_lock/pid"

start_ssh_mux() {
    # Read VM_HOST and optional SSH_KEY from .test-env (same source as harness).
    [[ ! -f "$repo_root/.test-env" ]] && return 0
    local vm_host ssh_key
    vm_host=$(grep '^VM_HOST=' "$repo_root/.test-env" 2>/dev/null | head -1 | cut -d= -f2- | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')
    ssh_key=$(grep '^SSH_KEY=' "$repo_root/.test-env" 2>/dev/null | head -1 | cut -d= -f2- | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')
    [[ -z "$vm_host" ]] && return 0

    local key_opts=()
    [[ -n "$ssh_key" && -f "$ssh_key" ]] && key_opts=(-i "$ssh_key" -o IdentitiesOnly=yes)

    # Start a background master that holds the TCP connection open.
    # ServerAliveInterval sends keepalives every 15 s so a silently-dropped
    # TCP connection is detected within ~75 s rather than hanging forever.
    ssh "${key_opts[@]+"${key_opts[@]}"}" \
        -o ControlMaster=yes \
        -o "ControlPath=$ssh_mux_socket" \
        -o ControlPersist=600 \
        -o BatchMode=yes \
        -o ConnectTimeout=10 \
        -o ServerAliveInterval=15 \
        -o ServerAliveCountMax=5 \
        -N "$vm_host" &>/dev/null &
    ssh_mux_pid=$!

    # Wait up to 5 s for the socket to appear.
    local i
    for i in 1 2 3 4 5; do
        [[ -S "$ssh_mux_socket" ]] && break
        sleep 1
    done

    if [[ ! -S "$ssh_mux_socket" ]]; then
        echo "[run-matrix] SSH mux: master did not start; proceeding without mux" >&2
        return 0
    fi
    echo "[run-matrix] SSH mux: ready ($ssh_mux_socket)" >&2

    # Create a transparent ssh wrapper so the harness reuses the master
    # without any harness-side changes.
    local real_ssh
    real_ssh=$(command -v ssh)
    ssh_wrapper_dir=$(mktemp -d /tmp/ext4-ssh-wrap-XXXXXX)
    cat > "$ssh_wrapper_dir/ssh" <<EOF
#!/bin/bash
exec "$real_ssh" -o ControlMaster=auto -o ControlPath="$ssh_mux_socket" "\$@"
EOF
    chmod +x "$ssh_wrapper_dir/ssh"
    export PATH="$ssh_wrapper_dir:$PATH"
}

stop_ssh_mux() {
    [[ -S "$ssh_mux_socket" ]] && \
        ssh -o ControlPath="$ssh_mux_socket" -O exit dummy 2>/dev/null || true
    [[ -n "${ssh_mux_pid:-}" ]] && kill "$ssh_mux_pid" 2>/dev/null || true
    rm -f "$ssh_mux_socket"
    [[ -n "${ssh_wrapper_dir:-}" ]] && rm -rf "$ssh_wrapper_dir"
}

cleanup() {
    if [ "$keep_images" -eq 1 ]; then
        echo "[run-matrix] --keep-images set; leaving images in $host_image_dir" >&2
    else
        # Only remove run_id subdirectories created by THIS invocation.
        # Pre-existing dirs (from a concurrent run with a different filter key)
        # are left untouched.
        local count=0 dir
        while IFS= read -r dir; do
            [ -z "$dir" ] && continue
            echo "$pre_run_subdirs" | grep -qxF "$dir" && continue
            local n
            n=$(find "$dir" -name 'ext4-*.img' -type f 2>/dev/null | wc -l | tr -d ' ')
            if [ "$n" -gt 0 ]; then
                find "$dir" -name 'ext4-*.img' -type f -delete
                count=$((count + n))
            fi
            rmdir "$dir" 2>/dev/null || true
        done < <(find "$host_image_dir" -mindepth 1 -maxdepth 1 -type d 2>/dev/null)
        if [ "$count" -gt 0 ]; then
            echo "[run-matrix] cleanup: removed $count image(s) from this run's dir(s)" >&2
        fi
    fi
    stop_ssh_mux
    rm -rf "$scenario_lock"
}

# Single EXIT trap does all the work. The signal traps re-raise as
# `exit <128 + signum>` (the conventional code for that signal), which
# flows into the EXIT trap once. Trapping cleanup directly on signals
# AND on EXIT would double-fire it.
trap cleanup EXIT
trap 'exit 130' INT   # 128 + SIGINT  (2)
trap 'exit 143' TERM  # 128 + SIGTERM (15)
trap 'exit 129' HUP   # 128 + SIGHUP  (1)
trap 'exit 131' QUIT  # 128 + SIGQUIT (3)

# Snapshot existing run_id subdirs so cleanup only removes dirs created by
# this invocation, not any belonging to a concurrently-running instance.
pre_run_subdirs=$(find "$host_image_dir" -mindepth 1 -maxdepth 1 -type d 2>/dev/null | sort)

# Start SSH connection mux before handing off to the harness.
# The harness opens many separate SSH sessions per scenario; multiplexing
# them through one TCP connection prevents Windows sshd MaxStartups exhaustion.
start_ssh_mux

# Forward to the real runner.
bash "$repo_root/vendor/fs-test-harness/scripts/run-tests.sh" "${forwarded_args[@]+"${forwarded_args[@]}"}"
