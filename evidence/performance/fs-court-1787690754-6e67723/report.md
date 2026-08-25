# Filesystem court v2 (Phase 8H + 9A + 9H)

Archive: /mnt/1tb_kingston/entropyfs/evidence/performance/fs-court-1787690754-6e67723

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

- foreground (post-GC): apparent 74387090 B, allocated 74403840 B
- settled (+optimize +full compaction): apparent 68280170 B, allocated 68296704 B (density 1.994x)
- settle cost: 6.01 s elapsed (optimize 5.84 s + compact 0.17 s), 1.048x physical write amplification (71527923 B appended)

## fs

- btrfs: {"waived": "requires root + loop", "command": "mkfs.btrfs -f <image> && mount -o loop <image> <mnt>"}
- btrfs-zstd: {"waived": "requires root + loop", "command": "mkfs.btrfs -f <image> && mount -o loop,compress=zstd:1 <image> <mnt>"}
- erofs-lz4hc: {"waived": "mkfs.erofs not installed"}
- ext4/compressed.tgz: {"apparent": 409650, "allocated": 413696, "buffered_write_mbps": 508.8, "durable_write_mbps": 16.5, "warm_read_mbps": 490.9, "cold_read_mbps": 490.9, "cache": "warm-retained"}
- ext4/random.bin: {"apparent": 67108864, "allocated": 67112960, "buffered_write_mbps": 6908.3, "durable_write_mbps": 298.1, "warm_read_mbps": 20121.1, "cold_read_mbps": 20121.1, "cache": "warm-retained"}
- ext4/src: {"apparent": 1559776, "allocated": 1912832, "buffered_write_mbps": 767.6, "durable_write_mbps": 1.0, "warm_read_mbps": 628.8, "cold_read_mbps": 628.8, "cache": "warm-retained"}
- ext4/zeros.bin: {"apparent": 67108864, "allocated": 67112960, "buffered_write_mbps": 6811.1, "durable_write_mbps": 461.7, "warm_read_mbps": 21997.0, "cold_read_mbps": 21997.0, "cache": "warm-retained"}
- squashfs-zstd: {"waived": "mksquashfs not installed"}
- xfs: {"waived": "requires root + loop", "command": "mkfs.xfs -f <image> && mount -o loop <image> <mnt>"}

## zstd

- -1/compressed.tgz: {"apparent": 409650, "image": 409675, "ratio": 1.0, "write_mbps": 248.9}
- -1/random.bin: {"apparent": 67108864, "image": 67110414, "ratio": 1.0, "write_mbps": 5269.5}
- -1/src: {"apparent": 1559776, "image": 381611, "ratio": 4.087, "write_mbps": 287.8}
- -1/src-per-64k: {"apparent": 1559776, "image": 433370, "ratio": 3.599}
- -1/zeros.bin: {"apparent": 67108864, "image": 2288, "ratio": 29330.797, "write_mbps": 13133.4}
- -19/compressed.tgz: {"apparent": 409650, "image": 409675, "ratio": 1.0, "write_mbps": 33.6}
- -19/random.bin: {"apparent": 67108864, "image": 67110414, "ratio": 1.0, "write_mbps": 11.5}
- -19/src: {"apparent": 1559776, "image": 257134, "ratio": 6.066, "write_mbps": 5.7}
- -19/src-per-64k: {"apparent": 1559776, "image": 351190, "ratio": 4.441}
- -19/zeros.bin: {"apparent": 67108864, "image": 2077, "ratio": 32310.479, "write_mbps": 2297.6}

## entropyfs

- compressed.tgz/cold_read_mbps: "387.1"
- daemon_cpu_threads_1: {"cpu_secs": 1.23, "wall_secs": 2.22, "utilization": 0.55}
- density: {"apparent": 136187154, "backing_apparent": 74387090, "backing_allocated": 74403840, "ratio": 1.83}
- entropyfs/compressed.tgz: {"apparent": 409650, "allocated": 410112, "buffered_write_mbps": 30.6, "durable_write_mbps": 23.7, "warm_read_mbps": 353.9, "cold_read_mbps": 353.9, "cache": "warm-retained"}
- entropyfs/random.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 70.0, "durable_write_mbps": 69.5, "warm_read_mbps": 3346.0, "cold_read_mbps": 3346.0, "cache": "warm-retained"}
- entropyfs/src: {"apparent": 1559776, "allocated": 1591296, "buffered_write_mbps": 1.9, "durable_write_mbps": 1.9, "warm_read_mbps": 71.5, "cold_read_mbps": 71.5, "cache": "warm-retained"}
- entropyfs/zeros.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 332.2, "durable_write_mbps": 319.8, "warm_read_mbps": 4526.8, "cold_read_mbps": 4526.8, "cache": "warm-retained"}
- random.bin/cold_read_mbps: "3617.9"
- settled: {"foreground_apparent": 74387090, "foreground_allocated": 74403840, "settled_apparent": 68280170, "settled_allocated": 68296704, "settle_elapsed_s": 6.01, "optimize_wall_s": 5.84, "compact_wall_s": 0.17, "settle_appended_bytes": 71527923, "settle_write_amp": 1.048, "settled_density": 1.994}
- src/cold_read_mbps: "69.9"
- zeros.bin/cold_read_mbps: "5105.5"

## Waivers

- fs/xfs: requires root + loop
- fs/btrfs: requires root + loop
- fs/btrfs-zstd: requires root + loop
- fs/squashfs-zstd: mksquashfs not installed
- fs/erofs-lz4hc: mkfs.erofs not installed

Run this court in a root-capable VM to clear the loop-mount
waivers and enable drop_caches cold reads.
