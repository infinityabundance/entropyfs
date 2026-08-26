# Filesystem court v2 (Phase 8H + 9A + 9H)

Archive: /repo/evidence/performance/fs-court-1787688786-b4abc71

Corpus artifact: the structured corpus contains only 4 unique
64 KiB chunks — a corpus property, not a claim (methodology §8).

## Density (computed and sealed by the tooling)

Numerator: the same corpus apparent-byte sum (du -sb of src,
random.bin, zeros.bin, compressed.tgz) for every row. Denominators:
the COMPLETE filesystem state — the whole loop image's allocated
blocks for XFS/Btrfs (including their own metadata), the complete
EntropyFS store backing (segments + superblock). Both denominators
therefore include filesystem overhead beyond the corpus files.

- btrfs: 0.943x (allocated 144683008 B)
- btrfs-zstd: 1.728x (allocated 78905344 B)
- entropyfs-settled: 1.994x (allocated 68272128 B)
- xfs: 0.669x (allocated 203964416 B)

## EntropyFS storage states (Phase-9H)

- foreground (post-GC): apparent 74613076 B, allocated 74616832 B
- settled (+optimize +full compaction): apparent 68266790 B, allocated 68272128 B (density 1.994x)
- settle cost: 5.38 s elapsed (optimize 5.22 s + compact 0.16 s), 1.048x physical write amplification (71516045 B appended)

## fs

- btrfs: {"image_logical_bytes": 1073741824, "image_allocated_bytes": 144683008, "corpus_apparent_bytes": 136382358, "density": 0.943}
- btrfs-zstd: {"image_logical_bytes": 1073741824, "image_allocated_bytes": 78905344, "corpus_apparent_bytes": 136382358, "density": 1.728}
- btrfs-zstd/compressed.tgz: {"apparent": 404619, "allocated": 405504, "buffered_write_mbps": 184.7, "durable_write_mbps": 36.8, "warm_read_mbps": 207.4, "cold_read_mbps": 152.9, "cache": "warm-retained"}
- btrfs-zstd/random.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 5808.7, "durable_write_mbps": 2128.2, "warm_read_mbps": 17763.2, "cold_read_mbps": 5288.1, "cache": "warm-retained"}
- btrfs-zstd/src: {"apparent": 1540563, "allocated": 1810432, "buffered_write_mbps": 113.8, "durable_write_mbps": 69.6, "warm_read_mbps": 489.7, "cold_read_mbps": 265.3, "cache": "warm-retained"}
- btrfs-zstd/zeros.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 6073.2, "durable_write_mbps": 2896.8, "warm_read_mbps": 15879.8, "cold_read_mbps": 7938.7, "cache": "warm-retained"}
- btrfs/compressed.tgz: {"apparent": 404619, "allocated": 405504, "buffered_write_mbps": 195.8, "durable_write_mbps": 36.2, "warm_read_mbps": 202.4, "cold_read_mbps": 161.1, "cache": "warm-retained"}
- btrfs/random.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 5720.0, "durable_write_mbps": 2041.6, "warm_read_mbps": 17481.7, "cold_read_mbps": 5006.4, "cache": "warm-retained"}
- btrfs/src: {"apparent": 1540563, "allocated": 1810432, "buffered_write_mbps": 137.4, "durable_write_mbps": 76.0, "warm_read_mbps": 551.1, "cold_read_mbps": 328.6, "cache": "warm-retained"}
- btrfs/zeros.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 5735.6, "durable_write_mbps": 2072.0, "warm_read_mbps": 16510.8, "cold_read_mbps": 5428.3, "cache": "warm-retained"}
- erofs-lz4hc/compressed.tgz: {"apparent": 404619, "image": 409600, "allocated": 409600, "ratio": 0.988}
- erofs-lz4hc/random.bin: {"apparent": 67108864, "image": 67112960, "allocated": 67112960, "ratio": 1.0}
- erofs-lz4hc/src: {"apparent": 1540563, "image": 819200, "allocated": 819200, "ratio": 1.881}
- erofs-lz4hc/zeros.bin: {"apparent": 67108864, "image": 319488, "allocated": 319488, "ratio": 210.051}
- ext4/compressed.tgz: {"apparent": 404619, "allocated": 405504, "buffered_write_mbps": 199.2, "durable_write_mbps": 38.5, "warm_read_mbps": 186.7, "cold_read_mbps": 182.9, "cache": "warm-retained"}
- ext4/random.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 3651.8, "durable_write_mbps": 2390.8, "warm_read_mbps": 10046.8, "cold_read_mbps": 8841.2, "cache": "warm-retained"}
- ext4/src: {"apparent": 1540563, "allocated": 1810432, "buffered_write_mbps": 460.2, "durable_write_mbps": 77.3, "warm_read_mbps": 549.2, "cold_read_mbps": 214.7, "cache": "warm-retained"}
- ext4/zeros.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 3764.6, "durable_write_mbps": 2549.0, "warm_read_mbps": 9441.0, "cold_read_mbps": 8619.5, "cache": "warm-retained"}
- squashfs-zstd/compressed.tgz: {"apparent": 404619, "image": 405504, "allocated": 405504, "ratio": 0.998}
- squashfs-zstd/random.bin: {"apparent": 67108864, "image": 67112960, "allocated": 67112960, "ratio": 1.0}
- squashfs-zstd/src: {"apparent": 1540563, "image": 319488, "allocated": 319488, "ratio": 4.822}
- squashfs-zstd/zeros.bin: {"apparent": 67108864, "image": 4096, "allocated": 4096, "ratio": 16384.0}
- xfs: {"image_logical_bytes": 1073741824, "image_allocated_bytes": 203964416, "corpus_apparent_bytes": 136382358, "density": 0.669}
- xfs/compressed.tgz: {"apparent": 404619, "allocated": 405504, "buffered_write_mbps": 190.2, "durable_write_mbps": 38.3, "warm_read_mbps": 198.6, "cold_read_mbps": 170.1, "cache": "warm-retained"}
- xfs/random.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 5766.9, "durable_write_mbps": 2105.8, "warm_read_mbps": 15704.0, "cold_read_mbps": 6085.2, "cache": "warm-retained"}
- xfs/src: {"apparent": 1540563, "allocated": 1830912, "buffered_write_mbps": 352.5, "durable_write_mbps": 84.4, "warm_read_mbps": 533.4, "cold_read_mbps": 367.0, "cache": "warm-retained"}
- xfs/zeros.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 6215.7, "durable_write_mbps": 2169.7, "warm_read_mbps": 16471.8, "cold_read_mbps": 6295.7, "cache": "warm-retained"}

## zstd

- -1/compressed.tgz: {"apparent": 404619, "image": 404644, "ratio": 1.0, "write_mbps": 200.3}
- -1/random.bin: {"apparent": 67108864, "image": 67110414, "ratio": 1.0, "write_mbps": 4698.5}
- -1/src: {"apparent": 1540563, "image": 372677, "ratio": 4.134, "write_mbps": 237.3}
- -1/src-per-64k: {"apparent": 1540563, "image": 423171, "ratio": 3.641}
- -1/zeros.bin: {"apparent": 67108864, "image": 2288, "ratio": 29330.797, "write_mbps": 7461.2}
- -19/compressed.tgz: {"apparent": 404619, "image": 404644, "ratio": 1.0, "write_mbps": 31.2}
- -19/random.bin: {"apparent": 67108864, "image": 67110414, "ratio": 1.0, "write_mbps": 11.9}
- -19/src: {"apparent": 1540563, "image": 253938, "ratio": 6.067, "write_mbps": 5.9}
- -19/src-per-64k: {"apparent": 1540563, "image": 342201, "ratio": 4.502}
- -19/zeros.bin: {"apparent": 67108864, "image": 2077, "ratio": 32310.479, "write_mbps": 1657.9}

## entropyfs

- compressed.tgz/cold_read_mbps: "174.1"
- density: {"apparent": 136162910, "backing_apparent": 74613076, "backing_allocated": 74616832, "ratio": 1.825}
- entropyfs/compressed.tgz: {"apparent": 404619, "allocated": 404992, "buffered_write_mbps": 42.2, "durable_write_mbps": 21.9, "warm_read_mbps": 174.4, "cold_read_mbps": 163.8, "cache": "warm-retained"}
- entropyfs/random.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 67.1, "durable_write_mbps": 66.5, "warm_read_mbps": 2586.0, "cold_read_mbps": 2677.3, "cache": "warm-retained"}
- entropyfs/src: {"apparent": 1540563, "allocated": 1571840, "buffered_write_mbps": 9.9, "durable_write_mbps": 9.2, "warm_read_mbps": 64.7, "cold_read_mbps": 60.4, "cache": "warm-retained"}
- entropyfs/zeros.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 244.4, "durable_write_mbps": 235.8, "warm_read_mbps": 3748.9, "cold_read_mbps": 4522.3, "cache": "warm-retained"}
- random.bin/cold_read_mbps: "2872.7"
- settled: {"foreground_apparent": 74613076, "foreground_allocated": 74616832, "settled_apparent": 68266790, "settled_allocated": 68272128, "settle_elapsed_s": 5.38, "optimize_wall_s": 5.22, "compact_wall_s": 0.16, "settle_appended_bytes": 71516045, "settle_write_amp": 1.048, "settled_density": 1.994}
- src/cold_read_mbps: "61.5"
- zeros.bin/cold_read_mbps: "4424.5"

## Waivers


Run this court in a root-capable VM to clear the loop-mount
waivers and enable drop_caches cold reads.
