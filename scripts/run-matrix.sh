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
builder_vm_pid=""
builder_vm_owned=0   # 1 only when this invocation started the VM

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
export HOST_IMAGE_DIR="$host_image_dir"

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

ship_vm_scripts() {
    # SCP harness vm-side scripts to the Windows VM so {vm.harness_root}/scripts/vm/
    # exists. Idempotent — runs every invocation since the files are small and
    # the copy keeps the VM in sync with any harness submodule bumps.
    [[ ! -f "$repo_root/.test-env" ]] && return 0
    local vm_host ssh_key vm_workdir
    vm_host=$(grep '^VM_HOST=' "$repo_root/.test-env" 2>/dev/null | head -1 | cut -d= -f2- | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')
    ssh_key=$(grep '^SSH_KEY=' "$repo_root/.test-env" 2>/dev/null | head -1 | cut -d= -f2- | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')
    vm_workdir=$(grep '^VM_WORKDIR=' "$repo_root/.test-env" 2>/dev/null | head -1 | cut -d= -f2- | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')
    [[ -z "$vm_host" || -z "$vm_workdir" ]] && return 0

    local key_opts=()
    [[ -n "$ssh_key" && -f "$ssh_key" ]] && key_opts=(-i "$ssh_key" -o IdentitiesOnly=yes)

    local harness_dir="vendor/fs-test-harness"
    local src="$repo_root/$harness_dir/scripts/vm"
    local dest="$vm_workdir/$harness_dir/scripts/vm"
    local ps_dest="${dest//\//\\}"

    ssh "${key_opts[@]+"${key_opts[@]}"}" \
        -o BatchMode=yes \
        -o ConnectTimeout=10 \
        "$vm_host" \
        "powershell -NoProfile -NonInteractive -Command \"New-Item -ItemType Directory -Path '$ps_dest' -Force | Out-Null\"" >&2 \
        || { echo "[run-matrix] WARNING: could not create VM script dir; vm-side ops may fail" >&2; return 0; }

    scp "${key_opts[@]+"${key_opts[@]}"}" \
        -o BatchMode=yes \
        -o ConnectTimeout=10 \
        -r "$src/." "$vm_host:$dest/" >&2 \
        || { echo "[run-matrix] WARNING: could not ship vm scripts; vm-side ops may fail" >&2; return 0; }

    echo "[run-matrix] harness vm scripts → $vm_host:$dest" >&2
}

ensure_vm_workdir() {
    # Read VM_HOST, SSH_KEY, VM_WORKDIR from .test-env (same source as harness).
    [[ ! -f "$repo_root/.test-env" ]] && return 0
    local vm_host ssh_key vm_workdir
    vm_host=$(grep '^VM_HOST=' "$repo_root/.test-env" 2>/dev/null | head -1 | cut -d= -f2- | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')
    ssh_key=$(grep '^SSH_KEY=' "$repo_root/.test-env" 2>/dev/null | head -1 | cut -d= -f2- | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')
    vm_workdir=$(grep '^VM_WORKDIR=' "$repo_root/.test-env" 2>/dev/null | head -1 | cut -d= -f2- | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')
    [[ -z "$vm_host" || -z "$vm_workdir" ]] && return 0

    local key_opts=()
    [[ -n "$ssh_key" && -f "$ssh_key" ]] && key_opts=(-i "$ssh_key" -o IdentitiesOnly=yes)

    # PowerShell: create the workdir if it doesn't already exist.
    local ps_workdir="${vm_workdir//\//\\}"
    ssh "${key_opts[@]+"${key_opts[@]}"}" \
        -o BatchMode=yes \
        -o ConnectTimeout=10 \
        "$vm_host" \
        "powershell -NoProfile -NonInteractive -Command \"if (-not (Test-Path '$ps_workdir')) { New-Item -ItemType Directory -Path '$ps_workdir' -Force | Out-Null }; Write-Host '[vm] workdir: $ps_workdir'\"" >&2 \
        || {
            echo "[run-matrix] WARNING: could not ensure VM workdir ($vm_workdir); ship-to-vm ops may fail" >&2
            return 0
        }
    echo "[run-matrix] VM workdir ready: $vm_workdir" >&2
}

# ── ext4 builder VM ─────────────────────────────────────────────────────────
# Mirrors the ntfs pattern: ntfs formats images on-demand using the Mac's
# own binary; ext4 needs Linux so we keep an Alpine VM running for the
# duration of the matrix run. Each recipe's `build-ext4-image` op SSHes
# into this VM to create + format + populate one image.
start_builder_vm() {
    local builder_script="$repo_root/vendor/rust-fs-ext4/test-disks/build-ext4-feature-images.sh"
    local env_file="$repo_root/vendor/rust-fs-ext4/test-disks/.vm-cache/server.env"

    # If a previous invocation left server.env and the qemu is still alive,
    # reuse it — don't start a second VM on the same port.
    if [[ -f "$env_file" ]]; then
        # shellcheck disable=SC1090
        source "$env_file"
        local existing_pid="${EXT4_BUILDER_PID:-}"
        if [[ -n "$existing_pid" ]] && kill -0 "$existing_pid" 2>/dev/null; then
            echo "[run-matrix] builder VM: already running (pid=${existing_pid}), reusing" >&2
            builder_vm_pid="$existing_pid"
            builder_vm_owned=0
            return 0
        fi
        # Stale server.env — remove it so the start script writes a fresh one.
        rm -f "$env_file"
    fi

    [[ ! -f "$builder_script" ]] && {
        echo "[run-matrix] builder VM: script not found at $builder_script" >&2
        return 1
    }
    echo "[run-matrix] builder VM: starting..." >&2
    bash "$builder_script" --server || {
        echo "[run-matrix] builder VM: startup script failed" >&2
        exit 1
    }
    if [[ ! -f "$env_file" ]]; then
        echo "[run-matrix] builder VM: server.env not written — startup failed" >&2
        exit 1
    fi
    # shellcheck disable=SC1090
    source "$env_file"
    builder_vm_pid="${EXT4_BUILDER_PID:-}"
    builder_vm_owned=1
    echo "[run-matrix] builder VM: ready (pid=${builder_vm_pid} port=${EXT4_BUILDER_PORT})" >&2
}

stop_builder_vm() {
    [[ -z "${builder_vm_pid:-}" ]] && return 0
    # Don't stop a VM we didn't start — it might still be in use by another
    # concurrent invocation that is reusing it.
    [[ "${builder_vm_owned:-0}" -eq 0 ]] && return 0
    local builder_key="$repo_root/vendor/rust-fs-ext4/test-disks/.vm-cache/builder-key"
    echo "[run-matrix] builder VM: shutting down (pid=${builder_vm_pid})..." >&2
    ssh \
        -p 2222 \
        -i "$builder_key" \
        -o StrictHostKeyChecking=no \
        -o BatchMode=yes \
        -o ControlPath=none \
        -o ConnectTimeout=5 \
        root@localhost "poweroff" 2>/dev/null || true
    # Give it 5 s then hard-kill qemu.
    local i
    for i in 1 2 3 4 5; do
        kill -0 "$builder_vm_pid" 2>/dev/null || return 0
        sleep 1
    done
    kill "$builder_vm_pid" 2>/dev/null || true
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
    stop_builder_vm
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

# Capture real ssh before the mux wrapper prepends to PATH. The builder SSH
# script uses this to reach the Alpine VM without going through the mux.
export EXT4_REAL_SSH
EXT4_REAL_SSH=$(command -v ssh)

# Start the ext4 builder VM so recipes can create images on demand.
start_builder_vm

# Start SSH connection mux before handing off to the harness.
# The harness opens many separate SSH sessions per scenario; multiplexing
# them through one TCP connection prevents Windows sshd MaxStartups exhaustion.
start_ssh_mux

# Ensure the Windows VM workdir exists. Idempotent — cheap SSH round-trip,
# skipped if VM_HOST or VM_WORKDIR is not configured in .test-env.
ensure_vm_workdir

# Ship the harness vm-side scripts (win-write.ps1, _lib.ps1, etc.) to the
# Windows VM so {vm.harness_root}/scripts/vm/ resolves correctly.
ship_vm_scripts

# ── smoke gate (only when running the full matrix) ────────────────────────────
# Fast host-only sanity check before committing to a full run. Catches
# wiring breakage (Alpine VM, SSH, image dir) in seconds. The smoke
# group is defined in fs-test-harness.toml [groups].smoke. Gate is
# skipped when the user passes a scenario filter — they know what they
# are targeting.
if [[ "${#forwarded_args[@]}" -eq 0 ]]; then
    echo "[run-matrix] === smoke gate (group: smoke) ===" >&2
    if ! bash "$repo_root/vendor/fs-test-harness/scripts/run-tests.sh" smoke; then
        echo "[run-matrix] smoke gate failed; aborting before full matrix run" >&2
        exit 1
    fi
    echo "[run-matrix] smoke gate passed; proceeding to full matrix" >&2
fi

# Forward to the real runner.
bash "$repo_root/vendor/fs-test-harness/scripts/run-tests.sh" "${forwarded_args[@]+"${forwarded_args[@]}"}"
