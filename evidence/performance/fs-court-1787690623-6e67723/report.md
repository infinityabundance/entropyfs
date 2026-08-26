# Filesystem court v2 (Phase 8H + 9A + 9H)

Archive: /mnt/1tb_kingston/entropyfs/evidence/performance/fs-court-1787690623-6e67723

Corpus artifact: the structured corpus contains only 4 unique
64 KiB chunks — a corpus property, not a claim (methodology §8).

## Density (computed and sealed by the tooling)

Numerator: the same corpus apparent-byte sum (du -sb of src,
random.bin, zeros.bin, compressed.tgz) for every row. Denominators:
the COMPLETE filesystem state — the whole loop image's allocated
blocks for XFS/Btrfs (including their own metadata), the complete
EntropyFS store backing (segments + superblock). Both denominators
therefore include filesystem overhead beyond the corpus files.

- entropyfs-settled: 1.994x (allocated 68296704 B)

## EntropyFS storage states (Phase-9H)

- foreground (post-GC): apparent 74364836 B, allocated 74383360 B
- settled (+optimize +full compaction): apparent 68280172 B, allocated 68296704 B (density 1.994x)
- settle cost: 5.99 s elapsed (optimize 5.82 s + compact 0.17 s), 1.048x physical write amplification (71562795 B appended)

## fs

- btrfs: {"waived": "requires root + loop", "command": "mkfs.btrfs -f <image> && mount -o loop <image> <mnt>"}
- btrfs-zstd: {"waived": "requires root + loop", "command": "mkfs.btrfs -f <image> && mount -o loop,compress=zstd:1 <image> <mnt>"}
- erofs-lz4hc: {"waived": "mkfs.erofs not installed"}
- ext4/compressed.tgz: {"apparent": 409652, "allocated": 413696, "buffered_write_mbps": 483.0, "durable_write_mbps": 67.0, "warm_read_mbps": 510.9, "cold_read_mbps": 510.9, "cache": "warm-retained"}
- ext4/random.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 6858.6, "durable_write_mbps": 1678.9, "warm_read_mbps": 20187.6, "cold_read_mbps": 20187.6, "cache": "warm-retained"}
- ext4/src: {"apparent": 1559776, "allocated": 1912832, "buffered_write_mbps": 670.0, "durable_write_mbps": 53.7, "warm_read_mbps": 652.5, "cold_read_mbps": 652.5, "cache": "warm-retained"}
- ext4/zeros.bin: {"apparent": 67108864, "allocated": 67112960, "buffered_write_mbps": 6692.4, "durable_write_mbps": 2523.3, "warm_read_mbps": 20537.8, "cold_read_mbps": 20537.8, "cache": "warm-retained"}
- squashfs-zstd: {"waived": "mksquashfs not installed"}
- xfs: {"waived": "requires root + loop", "command": "mkfs.xfs -f <image> && mount -o loop <image> <mnt>"}

## zstd

- -1/compressed.tgz: {"apparent": 409652, "image": 409677, "ratio": 1.0, "write_mbps": 274.1}
- -1/random.bin: {"apparent": 67108864, "image": 67110414, "ratio": 1.0, "write_mbps": 4963.8}
- -1/src: {"apparent": 1559776, "image": 381626, "ratio": 4.087, "write_mbps": 271.0}
- -1/src-per-64k: {"apparent": 1559776, "image": 433381, "ratio": 3.599}
- -1/zeros.bin: {"apparent": 67108864, "image": 2288, "ratio": 29330.797, "write_mbps": 12303.3}
- -19/compressed.tgz: {"apparent": 409652, "image": 409677, "ratio": 1.0, "write_mbps": 34.3}
- -19/random.bin: {"apparent": 67108864, "image": 67110414, "ratio": 1.0, "write_mbps": 11.7}
- -19/src: {"apparent": 1559776, "image": 257128, "ratio": 6.066, "write_mbps": 5.7}
- -19/src-per-64k: {"apparent": 1559776, "image": 351209, "ratio": 4.441}
- -19/zeros.bin: {"apparent": 67108864, "image": 2077, "ratio": 32310.479, "write_mbps": 2152.1}

## entropyfs

- compressed.tgz/cold_read_mbps: "428.8"
- daemon_cpu_threads_1: {"cpu_secs": 1.23, "wall_secs": 0.0, "utilization": 1230.0}
- density: {"apparent": 136187156, "backing_apparent": 74364836, "backing_allocated": 74383360, "ratio": 1.831}
- entropyfs/compressed.tgz: {"apparent": 409652, "allocated": 410112, "buffered_write_mbps": 30.9, "durable_write_mbps": 24.1, "warm_read_mbps": 435.0, "cold_read_mbps": 435.0, "cache": "warm-retained"}
- entropyfs/random.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 71.4, "durable_write_mbps": 70.0, "warm_read_mbps": 3574.1, "cold_read_mbps": 3574.1, "cache": "warm-retained"}
- entropyfs/src: {"apparent": 1559776, "allocated": 1591296, "buffered_write_mbps": 1.9, "durable_write_mbps": 1.8, "warm_read_mbps": 72.0, "cold_read_mbps": 72.0, "cache": "warm-retained"}
- entropyfs/zeros.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 335.9, "durable_write_mbps": 323.8, "warm_read_mbps": 4489.6, "cold_read_mbps": 4489.6, "cache": "warm-retained"}
- random.bin/cold_read_mbps: "3652.8"
- settled: {"foreground_apparent": 74364836, "foreground_allocated": 74383360, "settled_apparent": 68280172, "settled_allocated": 68296704, "settle_elapsed_s": 5.99, "optimize_wall_s": 5.82, "compact_wall_s": 0.17, "settle_appended_bytes": 71562795, "settle_write_amp": 1.048, "settled_density": 1.994}
- src/cold_read_mbps: "73.5"
- zeros.bin/cold_read_mbps: "4441.7"

## Waivers

- fs/xfs: requires root + loop
- fs/btrfs: requires root + loop
- fs/btrfs-zstd: requires root + loop
- fs/squashfs-zstd: mksquashfs not installed
- fs/erofs-lz4hc: mkfs.erofs not installed

Run this court in a root-capable VM to clear the loop-mount
waivers and enable drop_caches cold reads.
