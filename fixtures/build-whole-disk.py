#!/usr/bin/env python3
"""Wrap ext4-basic.img in a synthetic GPT-partitioned whole-disk image.

The harness scenario `whole-disk-with-part` exercises `ext4 mount
<image> --part 1`, which needs a disk image with an ext4 partition
inside an MBR/GPT layout. We synthesise the smallest valid GPT we can
get away with: protective MBR at LBA 0, GPT header at LBA 1,
single-entry partition array at LBA 2, ext4 payload starting at the
standard 1-MiB-aligned LBA 2048.

Re-run whenever `ext4-basic.img` changes:

    python3 fixtures/build-whole-disk.py
"""
import struct
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
SRC = REPO.parent / "rust-fs-ext4" / "test-disks" / "ext4-basic.img"
DST = REPO.parent / "rust-fs-ext4" / "test-disks" / "ext4-whole-disk.img"

SECTOR = 512
DATA_LBA = 2048

# Linux filesystem data partition type GUID, on-disk byte order
# (mixed-endian: data1+data2+data3 little-endian, data4 big-endian).
GUID_LINUX_FS = bytes(
    [0xAF, 0xDA, 0xC6, 0x0F, 0x83, 0x84, 0x72, 0x47,
     0x8E, 0x79, 0x3D, 0x69, 0xD8, 0x47, 0x7D, 0xE4]
)


def main() -> int:
    if not SRC.exists():
        print(f"missing source: {SRC}", file=sys.stderr)
        return 1
    data = SRC.read_bytes()
    data_sectors = (len(data) + SECTOR - 1) // SECTOR
    total_sectors = DATA_LBA + data_sectors + 64  # backup-GPT pad
    img = bytearray(total_sectors * SECTOR)

    # Protective MBR.
    img[510] = 0x55
    img[511] = 0xAA
    mbr_part = 446
    img[mbr_part + 4] = 0xEE
    img[mbr_part + 8 : mbr_part + 12] = struct.pack("<I", 1)
    img[mbr_part + 12 : mbr_part + 16] = struct.pack(
        "<I", min(0xFFFFFFFF, total_sectors - 1)
    )

    # GPT header at LBA 1 (CRCs left zeroed — our parser doesn't verify).
    hdr = bytearray(92)
    hdr[0:8] = b"EFI PART"
    hdr[12:16] = struct.pack("<I", 92)        # header_size
    hdr[72:80] = struct.pack("<Q", 2)         # part array LBA
    hdr[80:84] = struct.pack("<I", 1)         # n entries
    hdr[84:88] = struct.pack("<I", 128)       # entry size
    img[SECTOR : SECTOR + 92] = hdr

    # Single partition entry at LBA 2.
    entry = bytearray(128)
    entry[0:16] = GUID_LINUX_FS
    entry[32:40] = struct.pack("<Q", DATA_LBA)
    entry[40:48] = struct.pack("<Q", DATA_LBA + data_sectors - 1)
    for i, ch in enumerate("rootfs"):
        entry[56 + i * 2 : 56 + i * 2 + 2] = struct.pack("<H", ord(ch))
    img[SECTOR * 2 : SECTOR * 2 + 128] = entry

    # Embed the ext4 image.
    img[DATA_LBA * SECTOR : DATA_LBA * SECTOR + len(data)] = data

    DST.write_bytes(img)
    print(f"wrote {DST}: {len(img):,} bytes ({total_sectors:,} sectors)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
