# Filesystem court v2 (Phase 8H + 9A + 9H)

Archive: /mnt/1tb_kingston/entropyfs/evidence/performance/fs-court-1787690689-6e67723

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

- foreground (post-GC): apparent 76165865 B, allocated 76185600 B
- settled (+optimize +full compaction): apparent 68283172 B, allocated 68300800 B (density 1.994x)
- settle cost: 6.02 s elapsed (optimize 5.85 s + compact 0.17 s), 1.048x physical write amplification (71584713 B appended)

## fs

- btrfs: {"waived": "requires root + loop", "command": "mkfs.btrfs -f <image> && mount -o loop <image> <mnt>"}
- btrfs-zstd: {"waived": "requires root + loop", "command": "mkfs.btrfs -f <image> && mount -o loop,compress=zstd:1 <image> <mnt>"}
- erofs-lz4hc: {"waived": "mkfs.erofs not installed"}
- ext4/compressed.tgz: {"apparent": 409651, "allocated": 413696, "buffered_write_mbps": 457.3, "durable_write_mbps": 80.6, "warm_read_mbps": 432.8, "cold_read_mbps": 432.8, "cache": "warm-retained"}
- ext4/random.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 4551.4, "durable_write_mbps": 1609.9, "warm_read_mbps": 19178.8, "cold_read_mbps": 19178.8, "cache": "warm-retained"}
- ext4/src: {"apparent": 1559776, "allocated": 1912832, "buffered_write_mbps": 722.9, "durable_write_mbps": 54.3, "warm_read_mbps": 662.1, "cold_read_mbps": 662.1, "cache": "warm-retained"}
- ext4/zeros.bin: {"apparent": 67108864, "allocated": 67112960, "buffered_write_mbps": 6437.0, "durable_write_mbps": 2314.8, "warm_read_mbps": 21474.5, "cold_read_mbps": 21474.5, "cache": "warm-retained"}
- squashfs-zstd: {"waived": "mksquashfs not installed"}
- xfs: {"waived": "requires root + loop", "command": "mkfs.xfs -f <image> && mount -o loop <image> <mnt>"}

## zstd

- -1/compressed.tgz: {"apparent": 409651, "image": 409676, "ratio": 1.0, "write_mbps": 280.2}
- -1/random.bin: {"apparent": 67108864, "image": 67110414, "ratio": 1.0, "write_mbps": 5207.1}
- -1/src: {"apparent": 1559776, "image": 381652, "ratio": 4.087, "write_mbps": 277.0}
- -1/src-per-64k: {"apparent": 1559776, "image": 433394, "ratio": 3.599}
- -1/zeros.bin: {"apparent": 67108864, "image": 2288, "ratio": 29330.797, "write_mbps": 11225.7}
- -19/compressed.tgz: {"apparent": 409651, "image": 409676, "ratio": 1.0, "write_mbps": 35.1}
- -19/random.bin: {"apparent": 67108864, "image": 67110414, "ratio": 1.0, "write_mbps": 11.4}
- -19/src: {"apparent": 1559776, "image": 257163, "ratio": 6.065, "write_mbps": 5.7}
- -19/src-per-64k: {"apparent": 1559776, "image": 351195, "ratio": 4.441}
- -19/zeros.bin: {"apparent": 67108864, "image": 2077, "ratio": 32310.479, "write_mbps": 2209.0}

## entropyfs

- compressed.tgz/cold_read_mbps: "396.7"
- daemon_cpu_threads_16: {"cpu_secs": 1.37, "wall_secs": 0.0, "utilization": 1370.0}
- density: {"apparent": 136187155, "backing_apparent": 76165865, "backing_allocated": 76185600, "ratio": 1.788}
- entropyfs/compressed.tgz: {"apparent": 409651, "allocated": 410112, "buffered_write_mbps": 30.0, "durable_write_mbps": 22.2, "warm_read_mbps": 315.0, "cold_read_mbps": 315.0, "cache": "warm-retained"}
- entropyfs/random.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 66.9, "durable_write_mbps": 64.9, "warm_read_mbps": 3358.3, "cold_read_mbps": 3358.3, "cache": "warm-retained"}
- entropyfs/src: {"apparent": 1559776, "allocated": 1591296, "buffered_write_mbps": 1.9, "durable_write_mbps": 1.9, "warm_read_mbps": 68.1, "cold_read_mbps": 68.1, "cache": "warm-retained"}
- entropyfs/zeros.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 337.0, "durable_write_mbps": 323.9, "warm_read_mbps": 4759.1, "cold_read_mbps": 4759.1, "cache": "warm-retained"}
- random.bin/cold_read_mbps: "4128.2"
- settled: {"foreground_apparent": 76165865, "foreground_allocated": 76185600, "settled_apparent": 68283172, "settled_allocated": 68300800, "settle_elapsed_s": 6.02, "optimize_wall_s": 5.85, "compact_wall_s": 0.17, "settle_appended_bytes": 71584713, "settle_write_amp": 1.048, "settled_density": 1.994}
- src/cold_read_mbps: "66.2"
- zeros.bin/cold_read_mbps: "4872.1"

## Waivers

- fs/xfs: requires root + loop
- fs/btrfs: requires root + loop
- fs/btrfs-zstd: requires root + loop
- fs/squashfs-zstd: mksquashfs not installed
- fs/erofs-lz4hc: mkfs.erofs not installed

Run this court in a root-capable VM to clear the loop-mount
waivers and enable drop_caches cold reads.
