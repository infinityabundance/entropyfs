# Release gates (Phase 12E.23/12E.24)

The adoption-release gate: `tools/check-release-gates.sh` verifies the
20-point 12E.23 checklist against the current tree (plus the
three-lane aggregate) and exits nonzero on any failure. The sealed
result of the gate run at the 12E line release:

```text
== release gates: 21 passed, 0 failed ==
```

## The gates

1. one Cargo package, no workspace (ADR-0001);
2. stable embeddable Engine facade (`src/engine`);
3. the engine builds without FUSE (`--no-default-features`);
4. format-v1 compatibility normative and implementation-tested
   (`compat_seal`);
5. golden stores continuously readable / fsck-clean (12E.4);
6. unknown COMPAT/RO_COMPAT/INCOMPAT behavior tested exactly;
7. `status --json`, `metrics --json`, `fsck --json` with versioned
   schemas (12E.6);
8–10. the three mandatory distro lanes (AlmaLinux 10.2, Ubuntu Server
   26.04, openSUSE Leap 16) pass their Docker-VM courts with immutable
   digests recorded;
11. every portability artifact records its immutable base-image digest;
12. SyncIo vs UringIo rerun on real storage before any default change
    (12E.11 — Sync retained);
13. the mounted worker-pool court informed the production scheduler
    default (11E1 — the pool is the mount default);
14. the small-object packing oracle ran before any persistent format was
    added (12E.12 — REJECTED);
15. the full normal/crash/hostile/fsck/parity suites stay green
    (ci-matrix lane, flake protocol documented);
16. no safety/resource-bound regression (the unsafe ledger enforces the
    two designated files);
17. material claims point to sealed evidence (the evidence index and
    per-archive manifests);
18. README remains the stable front door; CHANGELOG remains the full
    temporal ledger;
19. the public API and persistent format have explicit compatibility
    policies (`docs/api/engine.md`, `docs/format/compatibility-policy.md`);
20. a new engineer can install EntropyFS and exercise the engine without
    understanding the research architecture (`tools/trial-path.sh`,
    the Go content-store example).

## The success criterion (12E.24)

> A competent engineer on AlmaLinux 10.2 minimal, Ubuntu Server 26.04
> LTS minimal, or openSUSE Leap 16 minimal can reproducibly build
> EntropyFS, open or create a store through a stable library surface,
> persist and retrieve exact content, inspect it with machine-readable
> operational tooling, upgrade without ambiguity about format
> compatibility, and verify the store without adopting the FUSE
> frontend or trusting undocumented behavior.

Every clause maps to a sealed gate above: the distro courts (8–10)
build + run the engine smoke + fsck; the Engine facade (2–3) is the
stable library surface; byte-exact round trips are the engine's hash
gate (adoption oracle, 12E.13); the JSON tooling (7) is the
machine-readable inspection; the compatibility policy (4, 6, 19) is the
upgrade contract; and the trial path (20) is the engineer's entry
point. The larger adoption test — "can another project obtain a
measurable storage benefit by embedding EntropyFS while treating its
internal representation machinery as a boring implementation detail?" —
is answered in the affirmative by the sealed adoption court
(`docs/adoption/object-store.md`).
