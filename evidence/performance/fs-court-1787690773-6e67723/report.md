# Filesystem court v2 (Phase 8H + 9A + 9H)

Archive: /mnt/1tb_kingston/entropyfs/evidence/performance/fs-court-1787690773-6e67723

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

- foreground (post-GC): apparent 74789303 B, allocated 74805248 B
- settled (+optimize +full compaction): apparent 68280156 B, allocated 68296704 B (density 1.994x)
- settle cost: 6.05 s elapsed (optimize 5.89 s + compact 0.16 s), 1.048x physical write amplification (71535973 B appended)

## fs

- btrfs: {"waived": "requires root + loop", "command": "mkfs.btrfs -f <image> && mount -o loop <image> <mnt>"}
- btrfs-zstd: {"waived": "requires root + loop", "command": "mkfs.btrfs -f <image> && mount -o loop,compress=zstd:1 <image> <mnt>"}
- erofs-lz4hc: {"waived": "mkfs.erofs not installed"}
- ext4/compressed.tgz: {"apparent": 409636, "allocated": 413696, "buffered_write_mbps": 480.0, "durable_write_mbps": 81.9, "warm_read_mbps": 494.0, "cold_read_mbps": 494.0, "cache": "warm-retained"}
- ext4/random.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 6613.1, "durable_write_mbps": 1816.3, "warm_read_mbps": 16704.1, "cold_read_mbps": 16704.1, "cache": "warm-retained"}
- ext4/src: {"apparent": 1559776, "allocated": 1912832, "buffered_write_mbps": 740.2, "durable_write_mbps": 58.3, "warm_read_mbps": 486.4, "cold_read_mbps": 486.4, "cache": "warm-retained"}
- ext4/zeros.bin: {"apparent": 67108864, "allocated": 67112960, "buffered_write_mbps": 6599.0, "durable_write_mbps": 2359.7, "warm_read_mbps": 19153.8, "cold_read_mbps": 19153.8, "cache": "warm-retained"}
- squashfs-zstd: {"waived": "mksquashfs not installed"}
- xfs: {"waived": "requires root + loop", "command": "mkfs.xfs -f <image> && mount -o loop <image> <mnt>"}

## zstd

- -1/compressed.tgz: {"apparent": 409636, "image": 409661, "ratio": 1.0, "write_mbps": 270.6}
- -1/random.bin: {"apparent": 67108864, "image": 67110414, "ratio": 1.0, "write_mbps": 5054.4}
- -1/src: {"apparent": 1559776, "image": 381638, "ratio": 4.087, "write_mbps": 272.8}
- -1/src-per-64k: {"apparent": 1559776, "image": 433364, "ratio": 3.599}
- -1/zeros.bin: {"apparent": 67108864, "image": 2288, "ratio": 29330.797, "write_mbps": 10792.1}
- -19/compressed.tgz: {"apparent": 409636, "image": 409661, "ratio": 1.0, "write_mbps": 36.0}
- -19/random.bin: {"apparent": 67108864, "image": 67110414, "ratio": 1.0, "write_mbps": 11.6}
- -19/src: {"apparent": 1559776, "image": 257178, "ratio": 6.065, "write_mbps": 5.7}
- -19/src-per-64k: {"apparent": 1559776, "image": 351195, "ratio": 4.441}
- -19/zeros.bin: {"apparent": 67108864, "image": 2077, "ratio": 32310.479, "write_mbps": 2188.6}

## entropyfs

- compressed.tgz/cold_read_mbps: "401.0"
- daemon_cpu_threads_2: {"cpu_secs": 1.26, "wall_secs": 2.2, "utilization": 0.57}
- density: {"apparent": 136187140, "backing_apparent": 74789303, "backing_allocated": 74805248, "ratio": 1.821}
- entropyfs/compressed.tgz: {"apparent": 409636, "allocated": 410112, "buffered_write_mbps": 30.8, "durable_write_mbps": 24.1, "warm_read_mbps": 362.5, "cold_read_mbps": 362.5, "cache": "warm-retained"}
- entropyfs/random.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 70.8, "durable_write_mbps": 70.4, "warm_read_mbps": 3727.3, "cold_read_mbps": 3727.3, "cache": "warm-retained"}
- entropyfs/src: {"apparent": 1559776, "allocated": 1591296, "buffered_write_mbps": 1.9, "durable_write_mbps": 1.8, "warm_read_mbps": 72.7, "cold_read_mbps": 72.7, "cache": "warm-retained"}
- entropyfs/zeros.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 374.5, "durable_write_mbps": 360.1, "warm_read_mbps": 4638.1, "cold_read_mbps": 4638.1, "cache": "warm-retained"}
- random.bin/cold_read_mbps: "4256.0"
- settled: {"foreground_apparent": 74789303, "foreground_allocated": 74805248, "settled_apparent": 68280156, "settled_allocated": 68296704, "settle_elapsed_s": 6.05, "optimize_wall_s": 5.89, "compact_wall_s": 0.16, "settle_appended_bytes": 71535973, "settle_write_amp": 1.048, "settled_density": 1.994}
- src/cold_read_mbps: "70.9"
- zeros.bin/cold_read_mbps: "5407.2"

## Waivers

- fs/xfs: requires root + loop
- fs/btrfs: requires root + loop
- fs/btrfs-zstd: requires root + loop
- fs/squashfs-zstd: mksquashfs not installed
- fs/erofs-lz4hc: mkfs.erofs not installed

Run this court in a root-capable VM to clear the loop-mount
waivers and enable drop_caches cold reads.
