# Filesystem court v2 (Phase 8H + 9A + 9H)

Archive: /repo/evidence/performance/fs-court-1787697867-b345640

Corpus artifact: the structured corpus contains only 4 unique
64 KiB chunks — a corpus property, not a claim (methodology §8).

## Density (computed and sealed by the tooling)

Numerator: the same corpus apparent-byte sum (du -sb of src,
random.bin, zeros.bin, compressed.tgz) for every row. Denominators:
the COMPLETE filesystem state — the whole loop image's allocated
blocks for XFS/Btrfs (including their own metadata), the complete
EntropyFS store backing (segments + superblock). Both denominators
therefore include filesystem overhead beyond the corpus files.

- btrfs: 0.94x (allocated 145072128 B)
- btrfs-zstd: 1.708x (allocated 79847424 B)
- entropyfs-settled: 1.995x (allocated 68370432 B)
- xfs: 0.668x (allocated 204218368 B)

## EntropyFS storage states (Phase-9H)

- foreground (post-GC): apparent 69434098 B, allocated 69439488 B
- settled (+optimize +full compaction): apparent 68364751 B, allocated 68370432 B (density 1.995x)
- settle cost: 5.45 s elapsed (optimize 5.27 s + compact 0.18 s), 1.052x physical write amplification (71900589 B appended)

## fs

- btrfs: {"image_logical_bytes": 1073741824, "image_allocated_bytes": 145072128, "corpus_apparent_bytes": 136401995, "density": 0.94}
- btrfs-zstd: {"image_logical_bytes": 1073741824, "image_allocated_bytes": 79847424, "corpus_apparent_bytes": 136401995, "density": 1.708}
- btrfs-zstd/compressed.tgz: {"apparent": 449049, "allocated": 450560, "buffered_write_mbps": 213.6, "durable_write_mbps": 35.1, "warm_read_mbps": 216.7, "cold_read_mbps": 180.1, "cache": "warm-retained"}
- btrfs-zstd/random.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 6134.6, "durable_write_mbps": 2061.4, "warm_read_mbps": 14678.8, "cold_read_mbps": 5483.7, "cache": "warm-retained"}
- btrfs-zstd/src: {"apparent": 1735218, "allocated": 2019328, "buffered_write_mbps": 126.9, "durable_write_mbps": 77.2, "warm_read_mbps": 586.1, "cold_read_mbps": 276.2, "cache": "warm-retained"}
- btrfs-zstd/zeros.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 6218.2, "durable_write_mbps": 2898.9, "warm_read_mbps": 15666.4, "cold_read_mbps": 8769.9, "cache": "warm-retained"}
- btrfs/compressed.tgz: {"apparent": 449049, "allocated": 450560, "buffered_write_mbps": 210.6, "durable_write_mbps": 37.4, "warm_read_mbps": 209.9, "cold_read_mbps": 185.6, "cache": "warm-retained"}
- btrfs/random.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 5916.4, "durable_write_mbps": 2155.1, "warm_read_mbps": 16075.0, "cold_read_mbps": 5252.5, "cache": "warm-retained"}
- btrfs/src: {"apparent": 1735218, "allocated": 2019328, "buffered_write_mbps": 192.4, "durable_write_mbps": 96.7, "warm_read_mbps": 538.0, "cold_read_mbps": 371.1, "cache": "warm-retained"}
- btrfs/zeros.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 6674.0, "durable_write_mbps": 2127.3, "warm_read_mbps": 14446.5, "cold_read_mbps": 5675.0, "cache": "warm-retained"}
- erofs-lz4hc/compressed.tgz: {"apparent": 449049, "image": 450560, "allocated": 450560, "ratio": 0.997}
- erofs-lz4hc/random.bin: {"apparent": 67108864, "image": 67112960, "allocated": 67112960, "ratio": 1.0}
- erofs-lz4hc/src: {"apparent": 1735218, "image": 913408, "allocated": 913408, "ratio": 1.9}
- erofs-lz4hc/zeros.bin: {"apparent": 67108864, "image": 319488, "allocated": 319488, "ratio": 210.051}
- ext4/compressed.tgz: {"apparent": 449049, "allocated": 450560, "buffered_write_mbps": 226.4, "durable_write_mbps": 37.5, "warm_read_mbps": 232.8, "cold_read_mbps": 196.9, "cache": "warm-retained"}
- ext4/random.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 3577.5, "durable_write_mbps": 2245.0, "warm_read_mbps": 9700.9, "cold_read_mbps": 8718.8, "cache": "warm-retained"}
- ext4/src: {"apparent": 1735218, "allocated": 2019328, "buffered_write_mbps": 503.6, "durable_write_mbps": 39.4, "warm_read_mbps": 606.3, "cold_read_mbps": 494.1, "cache": "warm-retained"}
- ext4/zeros.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 3570.5, "durable_write_mbps": 2381.9, "warm_read_mbps": 9720.8, "cold_read_mbps": 9172.5, "cache": "warm-retained"}
- squashfs-zstd/compressed.tgz: {"apparent": 449049, "image": 450560, "allocated": 450560, "ratio": 0.997}
- squashfs-zstd/random.bin: {"apparent": 67108864, "image": 67112960, "allocated": 67112960, "ratio": 1.0}
- squashfs-zstd/src: {"apparent": 1735218, "image": 360448, "allocated": 360448, "ratio": 4.814}
- squashfs-zstd/zeros.bin: {"apparent": 67108864, "image": 4096, "allocated": 4096, "ratio": 16384.0}
- xfs: {"image_logical_bytes": 1073741824, "image_allocated_bytes": 204218368, "corpus_apparent_bytes": 136401995, "density": 0.668}
- xfs/compressed.tgz: {"apparent": 449049, "allocated": 450560, "buffered_write_mbps": 213.5, "durable_write_mbps": 37.7, "warm_read_mbps": 233.5, "cold_read_mbps": 185.2, "cache": "warm-retained"}
- xfs/random.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 6139.6, "durable_write_mbps": 2169.8, "warm_read_mbps": 16482.7, "cold_read_mbps": 5231.9, "cache": "warm-retained"}
- xfs/src: {"apparent": 1735218, "allocated": 2039808, "buffered_write_mbps": 348.7, "durable_write_mbps": 105.4, "warm_read_mbps": 602.0, "cold_read_mbps": 391.1, "cache": "warm-retained"}
- xfs/zeros.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 5782.0, "durable_write_mbps": 2240.8, "warm_read_mbps": 16107.6, "cold_read_mbps": 5821.0, "cache": "warm-retained"}

## zstd

- -1/compressed.tgz: {"apparent": 449049, "image": 449074, "ratio": 1.0, "write_mbps": 220.1}
- -1/random.bin: {"apparent": 67108864, "image": 67110414, "ratio": 1.0, "write_mbps": 4405.4}
- -1/src: {"apparent": 1735218, "image": 420859, "ratio": 4.123, "write_mbps": 245.2}
- -1/src-per-64k: {"apparent": 1735218, "image": 477728, "ratio": 3.632}
- -1/zeros.bin: {"apparent": 67108864, "image": 2288, "ratio": 29330.797, "write_mbps": 7439.2}
- -19/compressed.tgz: {"apparent": 449049, "image": 449074, "ratio": 1.0, "write_mbps": 32.8}
- -19/random.bin: {"apparent": 67108864, "image": 67110414, "ratio": 1.0, "write_mbps": 11.5}
- -19/src: {"apparent": 1735218, "image": 284403, "ratio": 6.101, "write_mbps": 5.8}
- -19/src-per-64k: {"apparent": 1735218, "image": 386143, "ratio": 4.494}
- -19/zeros.bin: {"apparent": 67108864, "image": 2077, "ratio": 32310.479, "write_mbps": 1776.5}

## entropyfs

- compressed.tgz/cold_read_mbps: "202.4"
- daemon_cpu_threads_1: {"cpu_secs": 2.79, "wall_secs": 2.37, "utilization": 1.18}
- density: {"apparent": 136401995, "backing_apparent": 69434098, "backing_allocated": 69439488, "ratio": 1.964}
- entropyfs/compressed.tgz: {"apparent": 449049, "allocated": 449536, "buffered_write_mbps": 85.6, "durable_write_mbps": 30.9, "warm_read_mbps": 200.2, "cold_read_mbps": 170.6, "cache": "warm-retained"}
- entropyfs/random.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 180.7, "durable_write_mbps": 174.8, "warm_read_mbps": 2138.6, "cold_read_mbps": 2212.9, "cache": "warm-retained"}
- entropyfs/src: {"apparent": 1735218, "allocated": 1767424, "buffered_write_mbps": 36.9, "durable_write_mbps": 24.4, "warm_read_mbps": 126.4, "cold_read_mbps": 122.7, "cache": "warm-retained"}
- entropyfs/zeros.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 932.0, "durable_write_mbps": 829.1, "warm_read_mbps": 2990.3, "cold_read_mbps": 3305.8, "cache": "warm-retained"}
- random.bin/cold_read_mbps: "1422.0"
- settled: {"foreground_apparent": 69434098, "foreground_allocated": 69439488, "settled_apparent": 68364751, "settled_allocated": 68370432, "settle_elapsed_s": 5.45, "optimize_wall_s": 5.27, "compact_wall_s": 0.18, "settle_appended_bytes": 71900589, "settle_write_amp": 1.052, "settled_density": 1.995}
- src/cold_read_mbps: "87.3"
- zeros.bin/cold_read_mbps: "1719.4"

## Waivers


Run this court in a root-capable VM to clear the loop-mount
waivers and enable drop_caches cold reads.
