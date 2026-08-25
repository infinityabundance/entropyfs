# Filesystem court v2 (Phase 8H + 9A + 9H)

Archive: /mnt/1tb_kingston/entropyfs/evidence/performance/fs-court-1787690806-6e67723

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

- foreground (post-GC): apparent 76520021 B, allocated 76537856 B
- settled (+optimize +full compaction): apparent 68282543 B, allocated 68300800 B (density 1.994x)
- settle cost: 5.97 s elapsed (optimize 5.81 s + compact 0.16 s), 1.048x physical write amplification (71594086 B appended)

## fs

- btrfs: {"waived": "requires root + loop", "command": "mkfs.btrfs -f <image> && mount -o loop <image> <mnt>"}
- btrfs-zstd: {"waived": "requires root + loop", "command": "mkfs.btrfs -f <image> && mount -o loop,compress=zstd:1 <image> <mnt>"}
- erofs-lz4hc: {"waived": "mkfs.erofs not installed"}
- ext4/compressed.tgz: {"apparent": 409642, "allocated": 413696, "buffered_write_mbps": 428.6, "durable_write_mbps": 79.6, "warm_read_mbps": 450.4, "cold_read_mbps": 450.4, "cache": "warm-retained"}
- ext4/random.bin: {"apparent": 67108864, "allocated": 67112960, "buffered_write_mbps": 4417.6, "durable_write_mbps": 1649.5, "warm_read_mbps": 18877.2, "cold_read_mbps": 18877.2, "cache": "warm-retained"}
- ext4/src: {"apparent": 1559776, "allocated": 1912832, "buffered_write_mbps": 708.9, "durable_write_mbps": 46.5, "warm_read_mbps": 656.3, "cold_read_mbps": 656.3, "cache": "warm-retained"}
- ext4/zeros.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 6622.5, "durable_write_mbps": 2315.0, "warm_read_mbps": 21365.5, "cold_read_mbps": 21365.5, "cache": "warm-retained"}
- squashfs-zstd: {"waived": "mksquashfs not installed"}
- xfs: {"waived": "requires root + loop", "command": "mkfs.xfs -f <image> && mount -o loop <image> <mnt>"}

## zstd

- -1/compressed.tgz: {"apparent": 409642, "image": 409667, "ratio": 1.0, "write_mbps": 254.3}
- -1/random.bin: {"apparent": 67108864, "image": 67110414, "ratio": 1.0, "write_mbps": 4761.3}
- -1/src: {"apparent": 1559776, "image": 381631, "ratio": 4.087, "write_mbps": 247.9}
- -1/src-per-64k: {"apparent": 1559776, "image": 433366, "ratio": 3.599}
- -1/zeros.bin: {"apparent": 67108864, "image": 2288, "ratio": 29330.797, "write_mbps": 11091.0}
- -19/compressed.tgz: {"apparent": 409642, "image": 409667, "ratio": 1.0, "write_mbps": 30.5}
- -19/random.bin: {"apparent": 67108864, "image": 67110414, "ratio": 1.0, "write_mbps": 11.3}
- -19/src: {"apparent": 1559776, "image": 257118, "ratio": 6.066, "write_mbps": 5.7}
- -19/src-per-64k: {"apparent": 1559776, "image": 351205, "ratio": 4.441}
- -19/zeros.bin: {"apparent": 67108864, "image": 2077, "ratio": 32310.479, "write_mbps": 2191.1}

## entropyfs

- compressed.tgz/cold_read_mbps: "304.1"
- daemon_cpu_threads_8: {"cpu_secs": 1.35, "wall_secs": 2.3, "utilization": 0.59}
- density: {"apparent": 136187146, "backing_apparent": 76520021, "backing_allocated": 76537856, "ratio": 1.779}
- entropyfs/compressed.tgz: {"apparent": 409642, "allocated": 410112, "buffered_write_mbps": 30.7, "durable_write_mbps": 23.7, "warm_read_mbps": 378.5, "cold_read_mbps": 378.5, "cache": "warm-retained"}
- entropyfs/random.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 65.4, "durable_write_mbps": 64.3, "warm_read_mbps": 3410.2, "cold_read_mbps": 3410.2, "cache": "warm-retained"}
- entropyfs/src: {"apparent": 1559776, "allocated": 1591296, "buffered_write_mbps": 1.9, "durable_write_mbps": 1.9, "warm_read_mbps": 68.5, "cold_read_mbps": 68.5, "cache": "warm-retained"}
- entropyfs/zeros.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 335.6, "durable_write_mbps": 324.1, "warm_read_mbps": 4507.7, "cold_read_mbps": 4507.7, "cache": "warm-retained"}
- random.bin/cold_read_mbps: "4335.9"
- settled: {"foreground_apparent": 76520021, "foreground_allocated": 76537856, "settled_apparent": 68282543, "settled_allocated": 68300800, "settle_elapsed_s": 5.97, "optimize_wall_s": 5.81, "compact_wall_s": 0.16, "settle_appended_bytes": 71594086, "settle_write_amp": 1.048, "settled_density": 1.994}
- src/cold_read_mbps: "69.5"
- zeros.bin/cold_read_mbps: "5118.8"

## Waivers

- fs/xfs: requires root + loop
- fs/btrfs: requires root + loop
- fs/btrfs-zstd: requires root + loop
- fs/squashfs-zstd: mksquashfs not installed
- fs/erofs-lz4hc: mkfs.erofs not installed

Run this court in a root-capable VM to clear the loop-mount
waivers and enable drop_caches cold reads.
