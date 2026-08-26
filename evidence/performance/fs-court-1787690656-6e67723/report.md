# Filesystem court v2 (Phase 8H + 9A + 9H)

Archive: /mnt/1tb_kingston/entropyfs/evidence/performance/fs-court-1787690656-6e67723

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

- foreground (post-GC): apparent 76072207 B, allocated 76091392 B
- settled (+optimize +full compaction): apparent 68284411 B, allocated 68300800 B (density 1.994x)
- settle cost: 5.99 s elapsed (optimize 5.83 s + compact 0.16 s), 1.049x physical write amplification (71612542 B appended)

## fs

- btrfs: {"waived": "requires root + loop", "command": "mkfs.btrfs -f <image> && mount -o loop <image> <mnt>"}
- btrfs-zstd: {"waived": "requires root + loop", "command": "mkfs.btrfs -f <image> && mount -o loop,compress=zstd:1 <image> <mnt>"}
- erofs-lz4hc: {"waived": "mkfs.erofs not installed"}
- ext4/compressed.tgz: {"apparent": 409628, "allocated": 413696, "buffered_write_mbps": 435.1, "durable_write_mbps": 80.2, "warm_read_mbps": 422.0, "cold_read_mbps": 422.0, "cache": "warm-retained"}
- ext4/random.bin: {"apparent": 67108864, "allocated": 67112960, "buffered_write_mbps": 4471.8, "durable_write_mbps": 1558.7, "warm_read_mbps": 20790.9, "cold_read_mbps": 20790.9, "cache": "warm-retained"}
- ext4/src: {"apparent": 1559776, "allocated": 1912832, "buffered_write_mbps": 640.8, "durable_write_mbps": 49.9, "warm_read_mbps": 645.8, "cold_read_mbps": 645.8, "cache": "warm-retained"}
- ext4/zeros.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 6606.8, "durable_write_mbps": 2313.1, "warm_read_mbps": 17442.1, "cold_read_mbps": 17442.1, "cache": "warm-retained"}
- squashfs-zstd: {"waived": "mksquashfs not installed"}
- xfs: {"waived": "requires root + loop", "command": "mkfs.xfs -f <image> && mount -o loop <image> <mnt>"}

## zstd

- -1/compressed.tgz: {"apparent": 409628, "image": 409653, "ratio": 1.0, "write_mbps": 310.2}
- -1/random.bin: {"apparent": 67108864, "image": 67110414, "ratio": 1.0, "write_mbps": 4652.4}
- -1/src: {"apparent": 1559776, "image": 381623, "ratio": 4.087, "write_mbps": 253.0}
- -1/src-per-64k: {"apparent": 1559776, "image": 433373, "ratio": 3.599}
- -1/zeros.bin: {"apparent": 67108864, "image": 2288, "ratio": 29330.797, "write_mbps": 11799.0}
- -19/compressed.tgz: {"apparent": 409628, "image": 409653, "ratio": 1.0, "write_mbps": 33.3}
- -19/random.bin: {"apparent": 67108864, "image": 67110414, "ratio": 1.0, "write_mbps": 10.9}
- -19/src: {"apparent": 1559776, "image": 257156, "ratio": 6.065, "write_mbps": 5.4}
- -19/src-per-64k: {"apparent": 1559776, "image": 351188, "ratio": 4.441}
- -19/zeros.bin: {"apparent": 67108864, "image": 2077, "ratio": 32310.479, "write_mbps": 2214.7}

## entropyfs

- compressed.tgz/cold_read_mbps: "409.4"
- daemon_cpu_threads_4: {"cpu_secs": 1.33, "wall_secs": 0.0, "utilization": 1330.0}
- density: {"apparent": 136187132, "backing_apparent": 76072207, "backing_allocated": 76091392, "ratio": 1.79}
- entropyfs/compressed.tgz: {"apparent": 409628, "allocated": 410112, "buffered_write_mbps": 30.3, "durable_write_mbps": 23.6, "warm_read_mbps": 381.5, "cold_read_mbps": 381.5, "cache": "warm-retained"}
- entropyfs/random.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 67.9, "durable_write_mbps": 66.7, "warm_read_mbps": 3608.7, "cold_read_mbps": 3608.7, "cache": "warm-retained"}
- entropyfs/src: {"apparent": 1559776, "allocated": 1591296, "buffered_write_mbps": 1.9, "durable_write_mbps": 1.9, "warm_read_mbps": 68.4, "cold_read_mbps": 68.4, "cache": "warm-retained"}
- entropyfs/zeros.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 346.0, "durable_write_mbps": 333.3, "warm_read_mbps": 4491.0, "cold_read_mbps": 4491.0, "cache": "warm-retained"}
- random.bin/cold_read_mbps: "4056.5"
- settled: {"foreground_apparent": 76072207, "foreground_allocated": 76091392, "settled_apparent": 68284411, "settled_allocated": 68300800, "settle_elapsed_s": 5.99, "optimize_wall_s": 5.83, "compact_wall_s": 0.16, "settle_appended_bytes": 71612542, "settle_write_amp": 1.049, "settled_density": 1.994}
- src/cold_read_mbps: "66.3"
- zeros.bin/cold_read_mbps: "4981.0"

## Waivers

- fs/xfs: requires root + loop
- fs/btrfs: requires root + loop
- fs/btrfs-zstd: requires root + loop
- fs/squashfs-zstd: mksquashfs not installed
- fs/erofs-lz4hc: mkfs.erofs not installed

Run this court in a root-capable VM to clear the loop-mount
waivers and enable drop_caches cold reads.
