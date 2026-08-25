# Filesystem court v2 (Phase 8H + 9A + 9H)

Archive: /repo/evidence/performance/fs-court-1787688251-69e69d4

Corpus artifact: the structured corpus contains only 4 unique
64 KiB chunks — a corpus property, not a claim (methodology §8).

## EntropyFS storage states (Phase-9H)

- foreground (post-GC): apparent 74583387 B, allocated 74588160 B
- settled (+optimize +full compaction): apparent 68266785 B, allocated 68272128 B (density 1.994x)
- settle cost: 5.45 s elapsed (optimize 5.29 s + compact 0.16 s), 1.047x physical write amplification (71448369 B appended)

## fs

- btrfs: {"image_logical_bytes": 1073741824, "image_allocated_bytes": 144781312}
- btrfs-zstd: {"image_logical_bytes": 1073741824, "image_allocated_bytes": 79134720}
- btrfs-zstd/compressed.tgz: {"apparent": 404614, "allocated": 405504, "buffered_write_mbps": 174.4, "durable_write_mbps": 33.9, "warm_read_mbps": 189.0, "cold_read_mbps": 148.2, "cache": "warm-retained"}
- btrfs-zstd/random.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 6145.8, "durable_write_mbps": 2117.7, "warm_read_mbps": 14570.9, "cold_read_mbps": 5304.9, "cache": "warm-retained"}
- btrfs-zstd/src: {"apparent": 1540563, "allocated": 1810432, "buffered_write_mbps": 100.7, "durable_write_mbps": 63.5, "warm_read_mbps": 537.2, "cold_read_mbps": 268.5, "cache": "warm-retained"}
- btrfs-zstd/zeros.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 5758.7, "durable_write_mbps": 2860.9, "warm_read_mbps": 16054.5, "cold_read_mbps": 8649.1, "cache": "warm-retained"}
- btrfs/compressed.tgz: {"apparent": 404614, "allocated": 405504, "buffered_write_mbps": 193.2, "durable_write_mbps": 34.0, "warm_read_mbps": 195.7, "cold_read_mbps": 153.3, "cache": "warm-retained"}
- btrfs/random.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 5772.1, "durable_write_mbps": 2061.5, "warm_read_mbps": 17333.0, "cold_read_mbps": 4975.3, "cache": "warm-retained"}
- btrfs/src: {"apparent": 1540563, "allocated": 1810432, "buffered_write_mbps": 121.9, "durable_write_mbps": 75.1, "warm_read_mbps": 540.2, "cold_read_mbps": 326.3, "cache": "warm-retained"}
- btrfs/zeros.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 5391.3, "durable_write_mbps": 2045.5, "warm_read_mbps": 16958.7, "cold_read_mbps": 5348.8, "cache": "warm-retained"}
- erofs-lz4hc/compressed.tgz: {"apparent": 404614, "image": 409600, "allocated": 409600, "ratio": 0.988}
- erofs-lz4hc/random.bin: {"apparent": 67108864, "image": 67112960, "allocated": 67112960, "ratio": 1.0}
- erofs-lz4hc/src: {"apparent": 1540563, "image": 819200, "allocated": 819200, "ratio": 1.881}
- erofs-lz4hc/zeros.bin: {"apparent": 67108864, "image": 319488, "allocated": 319488, "ratio": 210.051}
- ext4/compressed.tgz: {"apparent": 404614, "allocated": 405504, "buffered_write_mbps": 201.1, "durable_write_mbps": 34.9, "warm_read_mbps": 204.0, "cold_read_mbps": 175.6, "cache": "warm-retained"}
- ext4/random.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 3852.9, "durable_write_mbps": 2393.2, "warm_read_mbps": 10159.7, "cold_read_mbps": 9123.1, "cache": "warm-retained"}
- ext4/src: {"apparent": 1540563, "allocated": 1810432, "buffered_write_mbps": 456.9, "durable_write_mbps": 34.0, "warm_read_mbps": 543.2, "cold_read_mbps": 460.6, "cache": "warm-retained"}
- ext4/zeros.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 3591.3, "durable_write_mbps": 2303.0, "warm_read_mbps": 9012.6, "cold_read_mbps": 8158.9, "cache": "warm-retained"}
- squashfs-zstd/compressed.tgz: {"apparent": 404614, "image": 405504, "allocated": 405504, "ratio": 0.998}
- squashfs-zstd/random.bin: {"apparent": 67108864, "image": 67112960, "allocated": 67112960, "ratio": 1.0}
- squashfs-zstd/src: {"apparent": 1540563, "image": 319488, "allocated": 319488, "ratio": 4.822}
- squashfs-zstd/zeros.bin: {"apparent": 67108864, "image": 4096, "allocated": 4096, "ratio": 16384.0}
- xfs: {"image_logical_bytes": 1073741824, "image_allocated_bytes": 203964416}
- xfs/compressed.tgz: {"apparent": 404614, "allocated": 405504, "buffered_write_mbps": 194.6, "durable_write_mbps": 33.4, "warm_read_mbps": 206.7, "cold_read_mbps": 172.5, "cache": "warm-retained"}
- xfs/random.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 5866.0, "durable_write_mbps": 2069.7, "warm_read_mbps": 15887.3, "cold_read_mbps": 6145.0, "cache": "warm-retained"}
- xfs/src: {"apparent": 1540563, "allocated": 1830912, "buffered_write_mbps": 321.5, "durable_write_mbps": 92.4, "warm_read_mbps": 521.8, "cold_read_mbps": 370.7, "cache": "warm-retained"}
- xfs/zeros.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 5648.2, "durable_write_mbps": 1978.8, "warm_read_mbps": 16083.2, "cold_read_mbps": 6045.8, "cache": "warm-retained"}

## zstd

- -1/compressed.tgz: {"apparent": 404614, "image": 404639, "ratio": 1.0, "write_mbps": 198.6}
- -1/random.bin: {"apparent": 67108864, "image": 67110414, "ratio": 1.0, "write_mbps": 4262.0}
- -1/src: {"apparent": 1540563, "image": 372663, "ratio": 4.134, "write_mbps": 231.0}
- -1/src-per-64k: {"apparent": 1540563, "image": 423161, "ratio": 3.641}
- -1/zeros.bin: {"apparent": 67108864, "image": 2288, "ratio": 29330.797, "write_mbps": 7133.2}
- -19/compressed.tgz: {"apparent": 404614, "image": 404639, "ratio": 1.0, "write_mbps": 31.4}
- -19/random.bin: {"apparent": 67108864, "image": 67110414, "ratio": 1.0, "write_mbps": 11.3}
- -19/src: {"apparent": 1540563, "image": 253965, "ratio": 6.066, "write_mbps": 5.8}
- -19/src-per-64k: {"apparent": 1540563, "image": 342196, "ratio": 4.502}
- -19/zeros.bin: {"apparent": 67108864, "image": 2077, "ratio": 32310.479, "write_mbps": 1761.8}

## entropyfs

- compressed.tgz/cold_read_mbps: "181.5"
- density: {"apparent": 136162905, "backing_apparent": 74583387, "backing_allocated": 74588160, "ratio": 1.826}
- entropyfs/compressed.tgz: {"apparent": 404614, "allocated": 404992, "buffered_write_mbps": 39.9, "durable_write_mbps": 22.2, "warm_read_mbps": 183.9, "cold_read_mbps": 153.5, "cache": "warm-retained"}
- entropyfs/random.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 68.1, "durable_write_mbps": 67.4, "warm_read_mbps": 2604.7, "cold_read_mbps": 2612.1, "cache": "warm-retained"}
- entropyfs/src: {"apparent": 1540563, "allocated": 1571840, "buffered_write_mbps": 10.2, "durable_write_mbps": 9.3, "warm_read_mbps": 64.0, "cold_read_mbps": 58.8, "cache": "warm-retained"}
- entropyfs/zeros.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 246.0, "durable_write_mbps": 238.7, "warm_read_mbps": 3963.2, "cold_read_mbps": 4245.2, "cache": "warm-retained"}
- random.bin/cold_read_mbps: "2786.4"
- settled: {"foreground_apparent": 74583387, "foreground_allocated": 74588160, "settled_apparent": 68266785, "settled_allocated": 68272128, "settle_elapsed_s": 5.45, "optimize_wall_s": 5.29, "compact_wall_s": 0.16, "settle_appended_bytes": 71448369, "settle_write_amp": 1.047, "settled_density": 1.994}
- src/cold_read_mbps: "64.6"
- zeros.bin/cold_read_mbps: "4688.2"

## Waivers


Run this court in a root-capable VM to clear the loop-mount
waivers and enable drop_caches cold reads.
