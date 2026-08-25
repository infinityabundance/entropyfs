# Filesystem court v2 (Phase 8H + 9A + 9H)

Archive: /repo/evidence/performance/fs-court-1787691637-d38f73f

Corpus artifact: the structured corpus contains only 4 unique
64 KiB chunks — a corpus property, not a claim (methodology §8).

## Density (computed and sealed by the tooling)

Numerator: the same corpus apparent-byte sum (du -sb of src,
random.bin, zeros.bin, compressed.tgz) for every row. Denominators:
the COMPLETE filesystem state — the whole loop image's allocated
blocks for XFS/Btrfs (including their own metadata), the complete
EntropyFS store backing (segments + superblock). Both denominators
therefore include filesystem overhead beyond the corpus files.

- btrfs: 0.94x (allocated 144973824 B)
- btrfs-zstd: 1.708x (allocated 79753216 B)
- entropyfs-settled: 1.994x (allocated 68296704 B)
- xfs: 0.668x (allocated 204021760 B)

## EntropyFS storage states (Phase-9H)

- foreground (post-GC): apparent 74517611 B, allocated 74522624 B
- settled (+optimize +full compaction): apparent 68291768 B, allocated 68296704 B (density 1.994x)
- settle cost: 5.36 s elapsed (optimize 5.2 s + compact 0.16 s), 1.048x physical write amplification (71553873 B appended)

## fs

- btrfs: {"image_logical_bytes": 1073741824, "image_allocated_bytes": 144973824, "corpus_apparent_bytes": 136213607, "density": 0.94}
- btrfs-zstd: {"image_logical_bytes": 1073741824, "image_allocated_bytes": 79753216, "corpus_apparent_bytes": 136213607, "density": 1.708}
- btrfs-zstd/compressed.tgz: {"apparent": 414954, "allocated": 417792, "buffered_write_mbps": 190.1, "durable_write_mbps": 5.4, "warm_read_mbps": 196.9, "cold_read_mbps": 134.5, "cache": "warm-retained"}
- btrfs-zstd/random.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 5700.3, "durable_write_mbps": 804.6, "warm_read_mbps": 16442.5, "cold_read_mbps": 5170.6, "cache": "warm-retained"}
- btrfs-zstd/src: {"apparent": 1580925, "allocated": 1855488, "buffered_write_mbps": 109.5, "durable_write_mbps": 11.1, "warm_read_mbps": 499.3, "cold_read_mbps": 276.2, "cache": "warm-retained"}
- btrfs-zstd/zeros.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 6211.7, "durable_write_mbps": 672.7, "warm_read_mbps": 16749.9, "cold_read_mbps": 7832.2, "cache": "warm-retained"}
- btrfs/compressed.tgz: {"apparent": 414954, "allocated": 417792, "buffered_write_mbps": 187.1, "durable_write_mbps": 6.0, "warm_read_mbps": 148.0, "cold_read_mbps": 162.4, "cache": "warm-retained"}
- btrfs/random.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 5735.5, "durable_write_mbps": 1204.3, "warm_read_mbps": 16857.3, "cold_read_mbps": 5277.6, "cache": "warm-retained"}
- btrfs/src: {"apparent": 1580925, "allocated": 1855488, "buffered_write_mbps": 140.6, "durable_write_mbps": 11.8, "warm_read_mbps": 493.4, "cold_read_mbps": 359.0, "cache": "warm-retained"}
- btrfs/zeros.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 6335.5, "durable_write_mbps": 786.3, "warm_read_mbps": 16584.0, "cold_read_mbps": 5547.3, "cache": "warm-retained"}
- erofs-lz4hc/compressed.tgz: {"apparent": 414954, "image": 417792, "allocated": 417792, "ratio": 0.993}
- erofs-lz4hc/random.bin: {"apparent": 67108864, "image": 67112960, "allocated": 67112960, "ratio": 1.0}
- erofs-lz4hc/src: {"apparent": 1580925, "image": 847872, "allocated": 847872, "ratio": 1.865}
- erofs-lz4hc/zeros.bin: {"apparent": 67108864, "image": 319488, "allocated": 319488, "ratio": 210.051}
- ext4/compressed.tgz: {"apparent": 414954, "allocated": 417792, "buffered_write_mbps": 213.2, "durable_write_mbps": 4.0, "warm_read_mbps": 202.9, "cold_read_mbps": 175.0, "cache": "warm-retained"}
- ext4/random.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 3866.8, "durable_write_mbps": 512.1, "warm_read_mbps": 8601.5, "cold_read_mbps": 8892.9, "cache": "warm-retained"}
- ext4/src: {"apparent": 1580925, "allocated": 1855488, "buffered_write_mbps": 482.7, "durable_write_mbps": 5.0, "warm_read_mbps": 543.4, "cold_read_mbps": 451.6, "cache": "warm-retained"}
- ext4/zeros.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 3452.5, "durable_write_mbps": 748.3, "warm_read_mbps": 9070.0, "cold_read_mbps": 8560.9, "cache": "warm-retained"}
- squashfs-zstd/compressed.tgz: {"apparent": 414954, "image": 417792, "allocated": 417792, "ratio": 0.993}
- squashfs-zstd/random.bin: {"apparent": 67108864, "image": 67112960, "allocated": 67112960, "ratio": 1.0}
- squashfs-zstd/src: {"apparent": 1580925, "image": 327680, "allocated": 327680, "ratio": 4.825}
- squashfs-zstd/zeros.bin: {"apparent": 67108864, "image": 4096, "allocated": 4096, "ratio": 16384.0}
- xfs: {"image_logical_bytes": 1073741824, "image_allocated_bytes": 204021760, "corpus_apparent_bytes": 136213607, "density": 0.668}
- xfs/compressed.tgz: {"apparent": 414954, "allocated": 417792, "buffered_write_mbps": 200.6, "durable_write_mbps": 5.3, "warm_read_mbps": 212.2, "cold_read_mbps": 164.7, "cache": "warm-retained"}
- xfs/random.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 6020.6, "durable_write_mbps": 783.5, "warm_read_mbps": 16941.0, "cold_read_mbps": 5816.9, "cache": "warm-retained"}
- xfs/src: {"apparent": 1580925, "allocated": 1875968, "buffered_write_mbps": 337.5, "durable_write_mbps": 13.3, "warm_read_mbps": 515.9, "cold_read_mbps": 365.7, "cache": "warm-retained"}
- xfs/zeros.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 5910.2, "durable_write_mbps": 695.5, "warm_read_mbps": 16608.9, "cold_read_mbps": 6303.0, "cache": "warm-retained"}

## zstd

- -1/compressed.tgz: {"apparent": 414954, "image": 414979, "ratio": 1.0, "write_mbps": 205.5}
- -1/random.bin: {"apparent": 67108864, "image": 67110414, "ratio": 1.0, "write_mbps": 4281.7}
- -1/src: {"apparent": 1580925, "image": 384336, "ratio": 4.113, "write_mbps": 239.8}
- -1/src-per-64k: {"apparent": 1580925, "image": 437526, "ratio": 3.613}
- -1/zeros.bin: {"apparent": 67108864, "image": 2288, "ratio": 29330.797, "write_mbps": 7011.2}
- -19/compressed.tgz: {"apparent": 414954, "image": 414979, "ratio": 1.0, "write_mbps": 32.4}
- -19/random.bin: {"apparent": 67108864, "image": 67110414, "ratio": 1.0, "write_mbps": 11.3}
- -19/src: {"apparent": 1580925, "image": 261509, "ratio": 6.045, "write_mbps": 5.7}
- -19/src-per-64k: {"apparent": 1580925, "image": 354097, "ratio": 4.465}
- -19/zeros.bin: {"apparent": 67108864, "image": 2077, "ratio": 32310.479, "write_mbps": 1717.5}

## entropyfs

- compressed.tgz/cold_read_mbps: "167.3"
- daemon_cpu_threads_1: {"cpu_secs": 0.77, "wall_secs": 2.91, "utilization": 0.26}
- density: {"apparent": 136213607, "backing_apparent": 74517611, "backing_allocated": 74522624, "ratio": 1.828}
- entropyfs/compressed.tgz: {"apparent": 414954, "allocated": 415232, "buffered_write_mbps": 66.3, "durable_write_mbps": 4.9, "warm_read_mbps": 168.0, "cold_read_mbps": 164.3, "cache": "warm-retained"}
- entropyfs/random.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 229.3, "durable_write_mbps": 176.5, "warm_read_mbps": 2492.6, "cold_read_mbps": 2654.3, "cache": "warm-retained"}
- entropyfs/src: {"apparent": 1580925, "allocated": 1611776, "buffered_write_mbps": 10.0, "durable_write_mbps": 5.3, "warm_read_mbps": 64.6, "cold_read_mbps": 55.3, "cache": "warm-retained"}
- entropyfs/zeros.bin: {"apparent": 67108864, "allocated": 67108864, "buffered_write_mbps": 235.4, "durable_write_mbps": 183.7, "warm_read_mbps": 3761.9, "cold_read_mbps": 4355.5, "cache": "warm-retained"}
- random.bin/cold_read_mbps: "2907.0"
- settled: {"foreground_apparent": 74517611, "foreground_allocated": 74522624, "settled_apparent": 68291768, "settled_allocated": 68296704, "settle_elapsed_s": 5.36, "optimize_wall_s": 5.2, "compact_wall_s": 0.16, "settle_appended_bytes": 71553873, "settle_write_amp": 1.048, "settled_density": 1.994}
- src/cold_read_mbps: "65.2"
- zeros.bin/cold_read_mbps: "4547.5"

## Waivers


Run this court in a root-capable VM to clear the loop-mount
waivers and enable drop_caches cold reads.
