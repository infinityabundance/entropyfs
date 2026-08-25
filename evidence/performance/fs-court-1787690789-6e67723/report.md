# Filesystem court v2 (Phase 8H + 9A + 9H)

Archive: /mnt/1tb_kingston/entropyfs/evidence/performance/fs-court-1787690789-6e67723

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

- foreground (post-GC): apparent 76339730 B, allocated 76357632 B
- settled (+optimize +full compaction): apparent 68281503 B, allocated 68300800 B (density 1.994x)
- settle cost: 6.06 s elapsed (optimize 5.9 s + compact 0.16 s), 1.049x physical write amplification (71615408 B appended)

## fs

- btrfs: {"waived": "requires root + loop", "command": "mkfs.btrfs -f <image> && mount -o loop <image> <mnt>"}
- btrfs-zstd: {"waived": "requires root + loop", "command": "mkfs.btrfs -f <image> && mount -o loop,compress=zstd:1 <image> <mnt>"}
- erofs-lz4hc: {"waived": "mkfs.erofs not installed"}
- ext4/compressed.tgz: {"apparent": 409641, "allocated": 413696, "buffered_write_mbps": 506.3, "durable_write_mbps": 63.6, "warm_read_mbps": 438.2, "cold_read_mbps": 438.2, "cache": "warm-retained"}
- ext4/random.bin: {"apparent": 67108864, "allocated": 67112960, "buffered_write_mbps": 4541.0, "durable_write_mbps": 1621.7, "warm_read_mbps": 17834.8, "cold_read_mbps": 17834.8, "cache": "warm-retained"}
- ext4/src: {"apparent": 1559776, "allocated": 1912832, "buffered_write_mbps": 742.1, "durable_write_mbps": 54.2, "warm_read_mbps": 644.0, "cold_read_mbps": 644.0, "cache": "warm-retained"}
- ext4/zeros.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 6713.4, "durable_write_mbps": 2355.3, "warm_read_mbps": 21516.2, "cold_read_mbps": 21516.2, "cache": "warm-retained"}
- squashfs-zstd: {"waived": "mksquashfs not installed"}
- xfs: {"waived": "requires root + loop", "command": "mkfs.xfs -f <image> && mount -o loop <image> <mnt>"}

## zstd

- -1/compressed.tgz: {"apparent": 409641, "image": 409666, "ratio": 1.0, "write_mbps": 253.2}
- -1/random.bin: {"apparent": 67108864, "image": 67110414, "ratio": 1.0, "write_mbps": 4725.2}
- -1/src: {"apparent": 1559776, "image": 381666, "ratio": 4.087, "write_mbps": 272.6}
- -1/src-per-64k: {"apparent": 1559776, "image": 433386, "ratio": 3.599}
- -1/zeros.bin: {"apparent": 67108864, "image": 2288, "ratio": 29330.797, "write_mbps": 11884.2}
- -19/compressed.tgz: {"apparent": 409641, "image": 409666, "ratio": 1.0, "write_mbps": 35.5}
- -19/random.bin: {"apparent": 67108864, "image": 67110414, "ratio": 1.0, "write_mbps": 11.0}
- -19/src: {"apparent": 1559776, "image": 257095, "ratio": 6.067, "write_mbps": 5.6}
- -19/src-per-64k: {"apparent": 1559776, "image": 351190, "ratio": 4.441}
- -19/zeros.bin: {"apparent": 67108864, "image": 2077, "ratio": 32310.479, "write_mbps": 2173.5}

## entropyfs

- compressed.tgz/cold_read_mbps: "319.4"
- daemon_cpu_threads_4: {"cpu_secs": 1.35, "wall_secs": 2.31, "utilization": 0.58}
- density: {"apparent": 136187145, "backing_apparent": 76339730, "backing_allocated": 76357632, "ratio": 1.784}
- entropyfs/compressed.tgz: {"apparent": 409641, "allocated": 410112, "buffered_write_mbps": 27.6, "durable_write_mbps": 21.6, "warm_read_mbps": 404.9, "cold_read_mbps": 404.9, "cache": "warm-retained"}
- entropyfs/random.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 64.0, "durable_write_mbps": 63.6, "warm_read_mbps": 3681.4, "cold_read_mbps": 3681.4, "cache": "warm-retained"}
- entropyfs/src: {"apparent": 1559776, "allocated": 1591296, "buffered_write_mbps": 1.9, "durable_write_mbps": 1.9, "warm_read_mbps": 67.7, "cold_read_mbps": 67.7, "cache": "warm-retained"}
- entropyfs/zeros.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 334.9, "durable_write_mbps": 322.9, "warm_read_mbps": 4481.5, "cold_read_mbps": 4481.5, "cache": "warm-retained"}
- random.bin/cold_read_mbps: "3901.9"
- settled: {"foreground_apparent": 76339730, "foreground_allocated": 76357632, "settled_apparent": 68281503, "settled_allocated": 68300800, "settle_elapsed_s": 6.06, "optimize_wall_s": 5.9, "compact_wall_s": 0.16, "settle_appended_bytes": 71615408, "settle_write_amp": 1.049, "settled_density": 1.994}
- src/cold_read_mbps: "68.8"
- zeros.bin/cold_read_mbps: "4761.4"

## Waivers

- fs/xfs: requires root + loop
- fs/btrfs: requires root + loop
- fs/btrfs-zstd: requires root + loop
- fs/squashfs-zstd: mksquashfs not installed
- fs/erofs-lz4hc: mkfs.erofs not installed

Run this court in a root-capable VM to clear the loop-mount
waivers and enable drop_caches cold reads.
