# Filesystem court v2 (Phase 8H + 9A + 9H)

Archive: /mnt/1tb_kingston/entropyfs/evidence/performance/fs-court-1787690823-6e67723

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

- foreground (post-GC): apparent 76395082 B, allocated 76414976 B
- settled (+optimize +full compaction): apparent 68280153 B, allocated 68296704 B (density 1.994x)
- settle cost: 6.0 s elapsed (optimize 5.83 s + compact 0.17 s), 1.048x physical write amplification (71552892 B appended)

## fs

- btrfs: {"waived": "requires root + loop", "command": "mkfs.btrfs -f <image> && mount -o loop <image> <mnt>"}
- btrfs-zstd: {"waived": "requires root + loop", "command": "mkfs.btrfs -f <image> && mount -o loop,compress=zstd:1 <image> <mnt>"}
- erofs-lz4hc: {"waived": "mkfs.erofs not installed"}
- ext4/compressed.tgz: {"apparent": 409633, "allocated": 413696, "buffered_write_mbps": 533.7, "durable_write_mbps": 69.8, "warm_read_mbps": 475.5, "cold_read_mbps": 475.5, "cache": "warm-retained"}
- ext4/random.bin: {"apparent": 67108864, "allocated": 67112960, "buffered_write_mbps": 4520.9, "durable_write_mbps": 1586.6, "warm_read_mbps": 20565.7, "cold_read_mbps": 20565.7, "cache": "warm-retained"}
- ext4/src: {"apparent": 1559776, "allocated": 1912832, "buffered_write_mbps": 672.4, "durable_write_mbps": 40.3, "warm_read_mbps": 673.5, "cold_read_mbps": 673.5, "cache": "warm-retained"}
- ext4/zeros.bin: {"apparent": 67108864, "allocated": 67112960, "buffered_write_mbps": 6225.0, "durable_write_mbps": 2212.6, "warm_read_mbps": 19942.0, "cold_read_mbps": 19942.0, "cache": "warm-retained"}
- squashfs-zstd: {"waived": "mksquashfs not installed"}
- xfs: {"waived": "requires root + loop", "command": "mkfs.xfs -f <image> && mount -o loop <image> <mnt>"}

## zstd

- -1/compressed.tgz: {"apparent": 409633, "image": 409658, "ratio": 1.0, "write_mbps": 265.0}
- -1/random.bin: {"apparent": 67108864, "image": 67110414, "ratio": 1.0, "write_mbps": 4827.9}
- -1/src: {"apparent": 1559776, "image": 381599, "ratio": 4.087, "write_mbps": 265.3}
- -1/src-per-64k: {"apparent": 1559776, "image": 433385, "ratio": 3.599}
- -1/zeros.bin: {"apparent": 67108864, "image": 2288, "ratio": 29330.797, "write_mbps": 10905.1}
- -19/compressed.tgz: {"apparent": 409633, "image": 409658, "ratio": 1.0, "write_mbps": 35.0}
- -19/random.bin: {"apparent": 67108864, "image": 67110414, "ratio": 1.0, "write_mbps": 11.3}
- -19/src: {"apparent": 1559776, "image": 257113, "ratio": 6.066, "write_mbps": 5.4}
- -19/src-per-64k: {"apparent": 1559776, "image": 351193, "ratio": 4.441}
- -19/zeros.bin: {"apparent": 67108864, "image": 2077, "ratio": 32310.479, "write_mbps": 2256.8}

## entropyfs

- compressed.tgz/cold_read_mbps: "385.8"
- daemon_cpu_threads_16: {"cpu_secs": 1.37, "wall_secs": 2.28, "utilization": 0.6}
- density: {"apparent": 136187137, "backing_apparent": 76395082, "backing_allocated": 76414976, "ratio": 1.782}
- entropyfs/compressed.tgz: {"apparent": 409633, "allocated": 410112, "buffered_write_mbps": 30.0, "durable_write_mbps": 23.4, "warm_read_mbps": 327.5, "cold_read_mbps": 327.5, "cache": "warm-retained"}
- entropyfs/random.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 65.5, "durable_write_mbps": 65.1, "warm_read_mbps": 3507.7, "cold_read_mbps": 3507.7, "cache": "warm-retained"}
- entropyfs/src: {"apparent": 1559776, "allocated": 1591296, "buffered_write_mbps": 1.9, "durable_write_mbps": 1.9, "warm_read_mbps": 65.7, "cold_read_mbps": 65.7, "cache": "warm-retained"}
- entropyfs/zeros.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 328.3, "durable_write_mbps": 316.4, "warm_read_mbps": 4434.9, "cold_read_mbps": 4434.9, "cache": "warm-retained"}
- random.bin/cold_read_mbps: "4128.5"
- settled: {"foreground_apparent": 76395082, "foreground_allocated": 76414976, "settled_apparent": 68280153, "settled_allocated": 68296704, "settle_elapsed_s": 6.0, "optimize_wall_s": 5.83, "compact_wall_s": 0.17, "settle_appended_bytes": 71552892, "settle_write_amp": 1.048, "settled_density": 1.994}
- src/cold_read_mbps: "66.5"
- zeros.bin/cold_read_mbps: "5333.4"

## Waivers

- fs/xfs: requires root + loop
- fs/btrfs: requires root + loop
- fs/btrfs-zstd: requires root + loop
- fs/squashfs-zstd: mksquashfs not installed
- fs/erofs-lz4hc: mkfs.erofs not installed

Run this court in a root-capable VM to clear the loop-mount
waivers and enable drop_caches cold reads.
