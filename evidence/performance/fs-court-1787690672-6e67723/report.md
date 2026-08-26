# Filesystem court v2 (Phase 8H + 9A + 9H)

Archive: /mnt/1tb_kingston/entropyfs/evidence/performance/fs-court-1787690672-6e67723

Corpus artifact: the structured corpus contains only 4 unique
64 KiB chunks — a corpus property, not a claim (methodology §8).

## Density (computed and sealed by the tooling)

Numerator: the same corpus apparent-byte sum (du -sb of src,
random.bin, zeros.bin, compressed.tgz) for every row. Denominators:
the COMPLETE filesystem state — the whole loop image's allocated
blocks for XFS/Btrfs (including their own metadata), the complete
EntropyFS store backing (segments + superblock). Both denominators
therefore include filesystem overhead beyond the corpus files.

- entropyfs-settled: 1.994x (allocated 68300800 B)

## EntropyFS storage states (Phase-9H)

- foreground (post-GC): apparent 76144747 B, allocated 76161024 B
- settled (+optimize +full compaction): apparent 68281623 B, allocated 68300800 B (density 1.994x)
- settle cost: 6.08 s elapsed (optimize 5.92 s + compact 0.16 s), 1.048x physical write amplification (71554342 B appended)

## fs

- btrfs: {"waived": "requires root + loop", "command": "mkfs.btrfs -f <image> && mount -o loop <image> <mnt>"}
- btrfs-zstd: {"waived": "requires root + loop", "command": "mkfs.btrfs -f <image> && mount -o loop,compress=zstd:1 <image> <mnt>"}
- erofs-lz4hc: {"waived": "mkfs.erofs not installed"}
- ext4/compressed.tgz: {"apparent": 409634, "allocated": 413696, "buffered_write_mbps": 472.6, "durable_write_mbps": 85.5, "warm_read_mbps": 474.5, "cold_read_mbps": 474.5, "cache": "warm-retained"}
- ext4/random.bin: {"apparent": 67108864, "allocated": 67112960, "buffered_write_mbps": 4465.6, "durable_write_mbps": 1611.9, "warm_read_mbps": 15460.7, "cold_read_mbps": 15460.7, "cache": "warm-retained"}
- ext4/src: {"apparent": 1559776, "allocated": 1912832, "buffered_write_mbps": 769.6, "durable_write_mbps": 42.7, "warm_read_mbps": 600.4, "cold_read_mbps": 600.4, "cache": "warm-retained"}
- ext4/zeros.bin: {"apparent": 67108864, "allocated": 67112960, "buffered_write_mbps": 6555.1, "durable_write_mbps": 2365.5, "warm_read_mbps": 18710.7, "cold_read_mbps": 18710.7, "cache": "warm-retained"}
- squashfs-zstd: {"waived": "mksquashfs not installed"}
- xfs: {"waived": "requires root + loop", "command": "mkfs.xfs -f <image> && mount -o loop <image> <mnt>"}

## zstd

- -1/compressed.tgz: {"apparent": 409634, "image": 409659, "ratio": 1.0, "write_mbps": 288.8}
- -1/random.bin: {"apparent": 67108864, "image": 67110414, "ratio": 1.0, "write_mbps": 4461.0}
- -1/src: {"apparent": 1559776, "image": 381607, "ratio": 4.087, "write_mbps": 258.4}
- -1/src-per-64k: {"apparent": 1559776, "image": 433379, "ratio": 3.599}
- -1/zeros.bin: {"apparent": 67108864, "image": 2288, "ratio": 29330.797, "write_mbps": 11812.6}
- -19/compressed.tgz: {"apparent": 409634, "image": 409659, "ratio": 1.0, "write_mbps": 34.8}
- -19/random.bin: {"apparent": 67108864, "image": 67110414, "ratio": 1.0, "write_mbps": 11.5}
- -19/src: {"apparent": 1559776, "image": 257119, "ratio": 6.066, "write_mbps": 5.6}
- -19/src-per-64k: {"apparent": 1559776, "image": 351193, "ratio": 4.441}
- -19/zeros.bin: {"apparent": 67108864, "image": 2077, "ratio": 32310.479, "write_mbps": 2287.8}

## entropyfs

- compressed.tgz/cold_read_mbps: "305.3"
- daemon_cpu_threads_8: {"cpu_secs": 1.34, "wall_secs": 0.0, "utilization": 1340.0}
- density: {"apparent": 136187138, "backing_apparent": 76144747, "backing_allocated": 76161024, "ratio": 1.788}
- entropyfs/compressed.tgz: {"apparent": 409634, "allocated": 410112, "buffered_write_mbps": 30.9, "durable_write_mbps": 24.2, "warm_read_mbps": 366.0, "cold_read_mbps": 366.0, "cache": "warm-retained"}
- entropyfs/random.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 67.1, "durable_write_mbps": 65.8, "warm_read_mbps": 3823.9, "cold_read_mbps": 3823.9, "cache": "warm-retained"}
- entropyfs/src: {"apparent": 1559776, "allocated": 1591296, "buffered_write_mbps": 1.9, "durable_write_mbps": 1.8, "warm_read_mbps": 70.4, "cold_read_mbps": 70.4, "cache": "warm-retained"}
- entropyfs/zeros.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 342.4, "durable_write_mbps": 329.6, "warm_read_mbps": 4610.1, "cold_read_mbps": 4610.1, "cache": "warm-retained"}
- random.bin/cold_read_mbps: "4079.2"
- settled: {"foreground_apparent": 76144747, "foreground_allocated": 76161024, "settled_apparent": 68281623, "settled_allocated": 68300800, "settle_elapsed_s": 6.08, "optimize_wall_s": 5.92, "compact_wall_s": 0.16, "settle_appended_bytes": 71554342, "settle_write_amp": 1.048, "settled_density": 1.994}
- src/cold_read_mbps: "68.6"
- zeros.bin/cold_read_mbps: "4742.7"

## Waivers

- fs/xfs: requires root + loop
- fs/btrfs: requires root + loop
- fs/btrfs-zstd: requires root + loop
- fs/squashfs-zstd: mksquashfs not installed
- fs/erofs-lz4hc: mkfs.erofs not installed

Run this court in a root-capable VM to clear the loop-mount
waivers and enable drop_caches cold reads.
