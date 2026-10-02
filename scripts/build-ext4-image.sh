#!/usr/bin/env bash
# build-ext4-image.sh RUN_ID IMAGE_TYPE -- produce one fresh ext4 image for one
# scenario, as {image_dir}/RUN_ID/ext4-<type>.img. It is the command behind
# the `build-ext4-image` op in fs-windows-test-harness.toml.
#
# The image always comes from rust-fs-ext4's test-disks/_vm-builder.sh, which
# formats it with mkfs.ext4 and populates it through a real kernel mount
# (setfattr, setfacl, symlinks). Where that script runs is the only choice:
#
#   * By default, in the Alpine builder VM run-matrix.sh starts (a Mac has no
#     mkfs.ext4 and no ext4 mount). The VM sees {image_dir} as /host over 9p.
#
#   * With EXT4_PREBUILT_IMAGES set, the same script already ran on a Linux
#     host before the matrix -- CI's `ext4 images` job, as root on
#     ubuntu-latest -- and wrote <dir>/<type>/ext4-*.img for each type. A
#     Windows runner has no Linux kernel to build on. Each scenario still gets
#     its own copy, because the read-write scenarios change the image.
#
# A type with no prebuilt image is an error naming the type and the
# directory, not a fallback to the VM: a CI run that silently tried to start
# QEMU would fail somewhere much less readable.
set -euo pipefail

[ $# -eq 2 ] || { echo "build-ext4-image.sh: usage: build-ext4-image.sh RUN_ID IMAGE_TYPE" >&2; exit 2; }
run_id="$1"
image_type="$2"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if [ -n "${EXT4_PREBUILT_IMAGES:-}" ]; then
    src="$EXT4_PREBUILT_IMAGES/$image_type"
    if ! ls "$src"/ext4-*.img >/dev/null 2>&1; then
        echo "build-ext4-image.sh: EXT4_PREBUILT_IMAGES has no ext4-*.img for type '$image_type' in $src" >&2
        exit 1
    fi
    # HOST_IMAGE_DIR is exported by run-matrix.sh; the runner resolves
    # {image_dir} from the same setting, so both name one directory.
    dest="${HOST_IMAGE_DIR:?build-ext4-image.sh: HOST_IMAGE_DIR is not set; run through scripts/run-matrix.sh}/$run_id"
    mkdir -p "$dest"
    cp "$src"/ext4-*.img "$dest"/
    echo "[build-ext4-image] $image_type: copied prebuilt $(cd "$src" && ls ext4-*.img | tr '\n' ' ')-> $dest"
    exit 0
fi

exec bash "$repo_root/scripts/builder-ssh.sh" \
    "mkdir -p /host/$run_id && sh /host/_vm-builder.sh /host/$run_id $image_type"
