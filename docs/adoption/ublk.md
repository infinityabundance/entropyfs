# ublk adoption path (Phase 12E.17)

The ublk frontend (`entropyfs ublk`, `src/ublk/`, ADR-0020) exposes the
SAME entropy storage engine as a Linux block device, so filesystems
(ext4/XFS) can be layered above it experimentally. Nothing is
duplicated: the device is a hidden file in the store and every block
operation runs through the normal representation engine.

## User-facing state (normative)

- **Status: EXPERIMENTAL.** The block frontend is an alternate frontend
  over the same engine, not a production device target.
- **Kernel requirements:** the control plane needs the `ublk_drv` kernel
  module (`CONFIG_BLK_DEV_UBLK`). Without it the kernel path cannot run;
  the `BlockStore` adapter itself is fully testable without the kernel.
- **Root/capability requirements:** attaching a ublk device requires
  root and the module loaded. In environments without root (or where
  the module is absent), `entropyfs ublk bench` remains available as the
  kernel-free BlockStore exercise.
- **Supported operations:** read, write, flush, discard (the adapter
  surface). The kernel-facing mapping (through `libublk`) is exercised
  where the Docker-VM/root court can provide `ublk_drv`.
- **Durability semantics:** `BlockStore::flush` IS the engine's
  durability barrier — the Phase-12B durability-generation machinery
  (group commit; concurrent flushes coalesce onto one physical barrier
  per generation). There is no independent block durability model; the
  ublk `FLUSH`/`FUA` mapping is exactly the barrier.
- **Discard semantics:** `discard(offset, len)` punches the range — the
  bytes read as zero and their storage is freed (the block-device
  equivalent of hole punching).

## Kernel-free exercise (CI)

```sh
entropyfs ublk bench STORE dev0 --size-mib 32
```

writes and verifies byte-exact through the `BlockStore` adapter with no
kernel dependency (sealed run: 33 554 432 B written and verified
byte-exact). This is the CI lane for the block adapter.

## Kernel path (root court)

Where the runtime provides `ublk_drv` + root, the court exercises read,
write, flush, discard, reopen, fsck through the device. On the current
development host the module requires root (the agent lane has none) —
recorded as a capability waiver under the normal discipline, never a
failure.
