# Filesystem court

Archive: /mnt/1tb_kingston/entropyfs/evidence/performance/fs-court-1787669946-b165d60

Per-corpus apparent bytes / allocated-or-image bytes / ratio.
Corpus artifact: 4 unique chunks per pattern — the structured corpus
artifact is a corpus property, not a claim (methodology §8).

## fs

- btrfs: {"waived": "requires root + loop", "command": "mkfs.btrfs -f <image> && mount -o loop <image> <mnt>"}
- btrfs-zstd: {"waived": "requires root + loop", "command": "mkfs.btrfs -f <image> && mount -o loop,compress=zstd:1 <image> <mnt>"}
- erofs-lz4hc: {"waived": "mkfs.erofs not installed"}
- ext4/compressed.tgz: {"apparent": 322276, "allocated": 323584, "write_mbps": 409.9}
- ext4/random.bin: {"apparent": 67108864, "allocated": 67108864, "write_mbps": 5033.2}
- ext4/src: {"apparent": 1146400, "allocated": 1478656, "write_mbps": 413.9}
- ext4/zeros.bin: {"apparent": 67108864, "allocated": 67108864, "write_mbps": 5688.2}
- squashfs-zstd: {"waived": "mksquashfs not installed"}
- xfs: {"waived": "requires root + loop", "command": "mkfs.xfs -f <image> && mount -o loop <image> <mnt>"}

## zstd

- -1/compressed.tgz: {"apparent": 322276, "image": 322298, "ratio": 1.0, "write_mbps": 236.0}
- -1/random.bin: {"apparent": 67108864, "image": 67110414, "ratio": 1.0, "write_mbps": 4666.3}
- -1/src: {"apparent": 1146400, "image": 292368, "ratio": 3.921, "write_mbps": 238.1}
- -1/zeros.bin: {"apparent": 67108864, "image": 2288, "ratio": 29330.797, "write_mbps": 13207.5}
- -19/compressed.tgz: {"apparent": 322276, "image": 322298, "ratio": 1.0, "write_mbps": 32.6}
- -19/random.bin: {"apparent": 67108864, "image": 67110414, "ratio": 1.0, "write_mbps": 11.5}
- -19/src: {"apparent": 1146400, "image": 200610, "ratio": 5.715, "write_mbps": 5.6}
- -19/zeros.bin: {"apparent": 67108864, "image": 2077, "ratio": 32310.479, "write_mbps": 2332.8}
- -3/compressed.tgz: {"apparent": 322276, "image": 322298, "ratio": 1.0, "write_mbps": 215.5}
- -3/random.bin: {"apparent": 67108864, "image": 67110414, "ratio": 1.0, "write_mbps": 3477.9}
- -3/src: {"apparent": 1146400, "image": 260731, "ratio": 4.397, "write_mbps": 205.8}
- -3/zeros.bin: {"apparent": 67108864, "image": 2120, "ratio": 31655.125, "write_mbps": 8706.8}

## entropyfs

- compressed.tgz: {"apparent": 322276, "write_mbps": 32.9, "read_mbps": 342.4}
- density: {"apparent": 135686404, "store_physical": 91156616, "ratio": 1.488}
- random.bin: {"apparent": 67108864, "write_mbps": 85.2, "read_mbps": 3532.3}
- src: {"apparent": 1146400, "write_mbps": 1.7, "read_mbps": 1288.5}
- zeros.bin: {"apparent": 67108864, "write_mbps": 453.4, "read_mbps": 4373.8}

## Waivers

- fs/xfs: requires root + loop
- fs/btrfs: requires root + loop
- fs/btrfs-zstd: requires root + loop
- fs/squashfs-zstd: mksquashfs not installed
- fs/erofs-lz4hc: mkfs.erofs not installed

Run this court in a root-capable VM to clear the loop-mount waivers.
