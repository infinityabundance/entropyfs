# EntropyFS ADR index

Architecture Decision Records. Statuses: `accepted`, `superseded`, `proposed`.

| ADR | Title | Status |
|-----|-------|--------|
| [0001](0001-single-crate.md) | Single-crate architecture, module-expressible boundaries | accepted |
| [0002](0002-fuse-frontend.md) | FUSE synchronous `fuser` frontend first; ublk later | accepted |
| [0003](0003-ryg-rans-rs.md) | Reuse `ryg-rans-rs` as the entropy backend, never fork it | accepted |
| [0004](0004-dsfb-observer.md) | DSFB is a zero-authority observer; exact cost decides | accepted |
| [0005](0005-representation-set.md) | Bounded, non-Turing-complete representation set v1 | accepted |
| [0006](0006-chunk-classes.md) | Multiple logical chunk classes, 64 KiB default | accepted |
| [0007](0007-object-model.md) | Immutable content-addressed nodes, COW mutation, atomic roots | accepted |
| [0008](0008-durability.md) | Dual superblocks, append-only segments, generation commit | accepted |
| [0009](0009-gc.md) | Reachability GC, emergency reserve, watermark behavior | accepted |
| [0010](0010-cost-function.md) | Explicit cost function and policy modes | accepted |
| [0011](0011-integrity.md) | Three distinct integrity concepts | accepted |
| [0012](0012-ondisk-codec.md) | Explicit little-endian byte codecs; serde only for JSON evidence | accepted |
| [0013](0013-concurrency.md) | Concurrent readers, per-inode mutation, narrow commit coordinator | accepted |
| [0014](0014-caching.md) | Caches are performance-only, never authoritative | accepted |
| [0015](0015-encryption.md) | Encryption layering defined now, implemented after storage is correct | accepted |
| [0016](0016-verification.md) | Property tests, crash courts, fuzzing, Kani where valuable | accepted |
| [0017](0017-dependencies.md) | Intentional dependency posture; no database, no tokio | accepted |
| [0018](0018-statfs.md) | Conservative `statfs`; opt-in logical overcommit | accepted |
