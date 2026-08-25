# Phase-10D: metadata writeback epochs (sealed court comparison, revision d2fe894)

Archives: `fs-court-1787697315-d2fe894/` (foreground=full),
`fs-court-1787697364-d2fe894/` (foreground=cheap). Zero waivers,
privileged docker VM, symmetric rules, 1 FUSE thread, background
optimizer disabled for the foreground section. Same machine and tooling
as the sealed 10B/10C pairs.

## What changed

Namespace/writeback ops accumulate in an ACTIVE EPOCH: each op appends
its staged objects + a `MUTATION_LOG` envelope (the recoverable dirty
state) and acks after the page-cache flush — no tree COW, no root build,
no superblock write per op. The overlay is the read view before the
checkpoint; checkpoints (fsync/unmount/GC/optimizer/size cap) merge the
frozen overlay into the trees ONCE (`bulk_load` for per-directory trees,
`apply_sorted_batch` bulk COW for the global indexes) with one root
publication. Recovery replays envelopes with `seq > root.log_seq`.
On-disk: format v1 retained; incompat bit 15 + record tag 0x07 + a
trailing `root.log_seq` field (additive, old binaries refuse).

## The result (mounted FUSE, same corpus set, same machine)

| corpus | 10C full | 10D full | 10B cheap | 10D cheap |
| ------ | -------- | -------- | --------- | --------- |
| src tiny-file writes (buffered) | 10.4 | **36.5** (3.5×) | 10.0 | **35.4** MiB/s |
| src tiny-file writes (durable) | 7.9 | **22.5** (2.8×) | 5.3 | 22.9 MiB/s |
| src warm reads | 67.1 | **128.2** (1.9×) | 64.6 | 109.8 MiB/s |
| random 64 MiB writes (buffered) | 148.8 | 176.6 | 229.3 | **579.3** MiB/s |
| zeros 64 MiB writes (buffered) | 271.8 | **867.0** (3.2×) | 235.4 | **904.8** MiB/s |
| compressed.tgz writes (buffered) | 63.8 | 81.1 | 66.3 | 151.8 MiB/s |
| warm random reads | 2743 | 1853 | 2493 | 2182 MiB/s |
| foreground density (post-GC) | 1.825× | **1.974×** | 1.821× | 1.974× |
| settled density (allocated) | 1.994× | **1.995×** | 1.994× | **1.995×** |
| settle cost / write amp | 5.39 s | 5.47 s / 1.051× | 5.39 s | 5.39 s / 1.051× |
| settled fsck / reconciliation | clean | clean (live canonical 100.0%) | clean | clean |

The epoch eliminates the per-op immutable-transaction machinery for the
namespace path: src tiny-file writes 3.5×, zeros 3.2× (the trivial path
no longer pays a transaction per op), and the foreground post-GC density
improves 1.825× → 1.974× because the write path no longer stages
per-op tree intermediates (the deferred checkpoint builds each final tree
once). The settled density floor holds (1.995×, within the corpus-growth
noise of the sealed 1.994×); the write amplification is essentially
unchanged (1.051× vs 1.047-1.049×) because the consumed log envelopes
are reclaimed by the compaction. All four settled runs: live canonical
100.0%, fsck clean.

Host A/B of the same src-workload (135 files, nested dirs): create p50
2.5 ms → 8.5 µs, setattr 2.4 ms → 4.1 µs (~300× per namespace op); the
source-tree copy drops from ~0.7 s to 0.045 s.

## On-disk note

Format version v1 is retained. The new incompat feature bit 15
(MUTATION_LOG), the record tag 0x07, and the trailing `root.log_seq`
field (absent in pre-epoch roots, decoded as 0) are additive extensions;
an implementation that cannot replay the log refuses the store.
