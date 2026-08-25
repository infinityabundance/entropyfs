# Crash consistency

Phase 0 deliverable #6. The protocol, the invariant, and the courts that
test it.

## 1. The invariant

> Recovery may observe the complete previous transaction or the complete new
> transaction, never an impossible hybrid root.

Formally: after any crash, the state reachable from the chosen root is
exactly the state committed by some generation `g` that was fully durable
before the crash, or the pre-crash state at generation `g−1` — never a
mixture of records from two different committed roots.

## 2. Why the protocol guarantees it

1. Records are immutable and content-addressed; a root references a set of
   records by hash.
2. Commit order (ADR-0008): records appended + `fdatasync`ed before the
   superblock slot is written; slot `fsync`ed before ack.
3. Therefore: if slot `N+1` is durable, every record reachable from
   root(N+1) is durable. If not, recovery reads slot `N` (or a torn `N+1`
   that fails CRC and is ignored). Either way, the reachable set is
   internally consistent.
4. Records not reachable from any root are garbage — unreachable garbage can
   never become authoritative because authority flows only from the
   superblock's root pointer.

## 3. Injection points (crash courts)

| Point | Meaning |
|-------|---------|
| `AFTER_RECORD_APPEND` | records appended, not yet fdatasync'd |
| `AFTER_SEGMENT_FDATASYNC` | segment data durable |
| `AFTER_SEGMENT_DIR_FSYNC` | new segment dir entry durable |
| `AFTER_ROOT_WRITE` | root object appended (it is, with the other records) |
| `AFTER_SUPERBLOCK_WRITE` | new slot written, not fsynced |
| `BEFORE_SUPERBLOCK_FSYNC` | same as above (alias) |
| `AFTER_SUPERBLOCK_FSYNC` | commit durable; before ack |
| `BEFORE_OLD_SEGMENT_DELETE` | GC: new root durable, old segments not yet unlinked |

## 4. Court procedure (per fixture)

```text
1. construct known pre-state (mkfs, populate, commit; record logical hashes)
2. begin operation (write / truncate / mkdir / gc / optimizer rewrite)
3. kill the daemon at the injection point (SIGKILL; no cleanup)
4. restart and mount (or run fsck offline)
5. assert:
   a. superblock selection picked a valid highest generation;
   b. every reachable record validates (CRC + structure);
   c. every file's logical bytes match the pre- or post-state expectation
      (hash-compare against recorded hashes);
   d. no unreachable authoritative metadata: fsck leak report contains
      only records that are garbage by construction;
   e. the store mounts read-write again and further commits succeed.
6. write a machine-readable receipt (evidence/)
```

## 5. Court families

- **Daemon-crash courts**: `tools/crash-court.sh` kills the daemon process
  at each injection point across a fixture matrix (metadata ops, data
  writes, truncates, renames, GC, optimizer rewrites, segment rollover,
  ENOSPC). Fast, deterministic, run in CI.
- **Host-crash courts**: `tools/vm-court.sh` performs the same matrix inside
  a VM with power cuts (QEMU `-action reboot=shutdown` or ACPI poweroff at
  the injection point, simulated by a crash-during-fsync helper that
  truncates at specific offsets, then full power-off). Never run on the
  development host (ADR-0016).
- **Latency-journal courts** (fsync-order torture): random commit-order
  perturbations within the protocol to catch ordering regressions.

## 6. Torn-write handling

- Superblock slots: 512 bytes with CRC; a torn write fails CRC. Recovery
  picks the other slot if it has a valid higher-or-equal generation.
- Segment tail: recovery scans forward; a torn envelope (bad magic/version,
  length overflow past EOF, bad CRC) ends the scan at the previous valid
  boundary. The torn tail is never referenced by a committed root unless it
  is a full valid record.

## 7. Related documents

- `docs/architecture/transaction-model.md` — protocol detail
- `docs/recovery/fsck.md` — independent verification
- `docs/security/threat-model.md` — adversarial framing
