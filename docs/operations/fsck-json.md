# fsck JSON reference (Phase 12E.6)

`entropyfs fsck --json STORE` emits a versioned, machine-readable report
— CI tooling never parses prose. Schema version is the top-level
`schema_version` field.

## Shape

```json
{
  "schema_version": 1,
  "status": "clean" | "corrupt",
  "findings": [
    {
      "code": "BTREE_FANOUT_LIMIT",
      "severity": "error" | "warning" | "info",
      "object": "…",
      "observed": 4097,
      "limit": 4096
    }
  ],
  "summary": { "segments_scanned": 1, "records_scanned": 10, "live_objects": 3 }
}
```

## Finding fields

| field | meaning |
| --- | --- |
| `code` | the stable finding class (the machine contract — switch on this) |
| `severity` | error (corruption) / warning (recoverable anomaly) / info |
| `object` | the affected object (inode/segment/record identity) |
| `observed` | the measured value, when the finding is a bound breach |
| `limit` | the violated bound, when applicable |

## Status semantics

- `clean` — no error-severity findings; `findings` may still carry
  warnings/info.
- `corrupt` — at least one error-severity finding; the store must not be
  trusted for writes until repaired or recreated.

## Finding classes (representative)

`BTREE_FANOUT_LIMIT`, `INODE_INDEX_MISSING`, `EXTENT_OVERLAP`,
`CHUNK_DESCRIPTOR_MISSING`, `REFERENCED_OBJECT_MISSING`,
`RECORD_CRC`, `TORN_RECORD`, `SUPERBLOCK_SLOT_INVALID`,
`UNEXPLAINED_BYTES` — the full set is the `FsckIssue` codes in
`src/integrity/` and `src/cli/json.rs` (the DTOs). `scrub --json` uses
the same schema with materialization-verification findings added.

## Contract

- `schema_version` increments only on a breaking schema change; fields
  are additive between versions.
- The library errors are the structured `FsckIssue`s; the CLI renders
  the same data as JSON. Nothing forces CI to parse prose.
