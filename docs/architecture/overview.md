# EntropyFS architecture overview

## 1. One sentence

EntropyFS is a mountable Linux filesystem whose persisted state is the
**minimum exact reversible representation state** necessary to reproduce
logical bytes, chosen per-extent by an exact cost function, committed
crash-consistently into an immutable content-addressed store, and served to
applications through FUSE.

## 2. The defining equation

```text
X = Materialize(D)
```

`X` = exact logical bytes; `D` = persisted representation descriptor. The
general form for structured families:

```text
X = T(E(U, S, P)) ⊕ R
```

`U` = versioned entropy universe · `S` = seed/state · `P` = rank/coordinate ·
`T` = bounded reversible transform · `R` = exact residual · `E` =
deterministic materialization. Not required for every extent — one family
among several, always selected by measured total cost.

## 3. Module map and dependency direction

```text
                    ┌───────────────────────────────┐
                    │  cli (commands, explain)      │
                    └──────────────┬────────────────┘
                                   │
                    ┌──────────────▼────────────────┐
                    │  fuse (POSIX adapter)         │
                    │  optimizer (search, rebase)   │
                    │  fsck (independent validation)│
                    └──────────────┬────────────────┘
                                   │
                    ┌──────────────▼────────────────┐
                    │  store (segments, transactions│
                    │  roots, inode/dir/extent trees│
                    │  snapshots, GC, recovery)     │
                    └──────────────┬────────────────┘
                                   │
        ┌──────────────┬───────────▼───────────┬──────────────┐
        │              │                       │              │
┌───────▼──────┐ ┌─────▼──────┐ ┌─────────────▼──┐ ┌─────────▼──────┐
│ format       │ │ integrity  │ │ cache (perf-  │ │ evidence       │
│ (byte codecs)│ │ (3 concepts)│ │ only)         │ │ (casefiles)    │
└──────────────┘ └────────────┘ └───────────────┘ └────────────────┘
                                   │
        ┌──────────────┬───────────▼───────────┬──────────────┐
        │              │                       │              │
┌───────▼──────┐ ┌─────▼──────┐ ┌─────────────▼──┐ ┌─────────▼──────┐
│ core         │ │ entropy    │ │ rans (adapts   │ │ dsfb (zero-    │
│ (representation│ (rank/unrank│ │ ryg-rans-rs)   │ │ authority obs.)│
│  algebra)    │ │  universes)│ │                │ │                │
└──────────────┘ └────────────┘ └────────────────┘ └────────────────┘
```

Invariants:

- `core` knows nothing about FUSE, disk, or DSFB.
- `fuse` contains no entropy algorithms — it converts FUSE ops into store
  transactions.
- `dsfb` never appears on any materialization path; the optimizer consults
  it only to order candidate search (ADR-0004).
- Everything is one crate (ADR-0001); arrows above are *visibility and
  import* direction, enforced by `pub(crate)` and code review, plus
  architecture tests.

## 4. Read path (summary)

`read(offset, len)` → extent tree lookup → per-extent `Materialize(D)` →
(optional cache) → reply. Materialization is a bounded interpreter over
representation descriptors; references resolve through the content index
with a depth cap of 4. See `docs/architecture/read-path.md`.

## 5. Write path (summary)

`write(offset, data)` → merge into affected 64 KiB extents → candidate
generation (dedup → cheap structural → rANS → RAW) → exact validation
(`materialize == data`) → commit transaction (append records → fsync →
superblock flip) → ack. See `docs/architecture/write-path.md`.

## 6. Commit (summary)

Dual-superblock generation commit (ADR-0008): append immutable records,
`fdatasync` segments, write inactive superblock slot with new root +
generation, `fsync` superblock. Recovery picks the highest valid generation.
See `docs/architecture/transaction-model.md` and
`docs/recovery/crash-consistency.md`.

## 7. The scientific loop

Every extent decision is auditable: `entropyfs explain <path>` shows
per-extent representation, alternatives, and exact byte accounting
(`docs/theory/information-accounting.md`). Every optimization campaign
produces evidence (hashes, revisions, commands, accounting) that makes
claims reproducible (`docs/performance/methodology.md`).

## 8. Phase status

See `docs/../README.md` (status table) for the phase map. The architecture
above is the target for all phases; each phase expands modules inside the
single crate (ADR-0001).
