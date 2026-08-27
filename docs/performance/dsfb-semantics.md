# Phase-12C-0: the DSFB structural-semiotics oracle

Sealed: `evidence/performance/dsfb-semantics-probe-*/`.
Probe: `src/tests/dsfb_semantics_probe.rs`. Machinery:
`src/dsfb/semantics.rs`.

## The idea

The 12C brief: use filesystem context to decide **which candidate-search
hypotheses are worth spending CPU on**, while retaining exact byte
validation as the only authority. The DSFB observer key becomes
`P(channel | chunk history, semantic context)` — a search-ordering /
trust score from cheap semantic classes.

## The machinery

`SemanticContext`: quantized classes from the name (extension / parent /
basename shape) and from a bounded 4 KiB byte sketch (magic signature /
printable ratio / entropy proxy) plus the lifecycle. `SemanticPrior`: a
per-class table of channel win counts (learned at every observe). The
DSFB plan scores each channel

```text
plan_trust(channel) = historical_trust(channel)
                    + 0.3 × prior(class, channel)
```

Strictly advisory: ordering and budget only, never bytes. The winner is
still the minimum over byte-validated candidates by exact cost. The mode
gate (`None` / `Extension` / `ByteSketch` / `History` / `Combined`)
selects which class groups feed the prior key, so the oracle can
attribute each evidence source.

## The oracle

A heterogeneous corpus — source `.rs`, config `.toml`, incompressible
`.bin`, zeros, extensionless — PLUS the brief's semantic-deception
exhibits (incompressible noise named `.rs`, zeros named `.bin`), written
twice per mode: pass 1 learns the prior, pass 2 measures the guided
search. Every mode must persist byte-identical state (asserted).

| mode | search CPU | cand/chunk | win rank | RAW% | density |
| --- | ---: | ---: | ---: | ---: | ---: |
| S0 none | 36.7 ms | 2.89 | 4.41 | 37.5 | 1.81 |
| S1 extension | 36.3 ms | 2.89 | **1.02** | 37.5 | 1.81 |
| S2 byte sketch | 36.0 ms | 2.89 | 2.88 | 37.5 | 1.81 |
| S3 history | 35.9 ms | 2.89 | 1.52 | 37.5 | 1.81 |
| S4 combined | 35.7 ms | 2.89 | 2.88 | 37.5 | 1.81 |

## The verdict: RECORD — the ordering is real, the CPU lever is not yet

The class evidence genuinely reorders the search: the winner's average
plan rank drops 4.41 → 1.02 with the extension classes (the name
predicts the winning family on this corpus) and 1.52 with history. The
deception exhibits prove the prior never overrides the byte gate: noise
named `.rs` and zeros named `.bin` behave byte-identically and
density-identically to their honest counterparts.

But the search CPU moves only ~3% (36.7 → 35.7 ms), and the reason is
structural: the DSFB plan's budget is a channel COUNT — the reordering
changes WHICH base channels sit inside the budget, not how many
candidates are evaluated. For chunks where the base channels never win
(noise → RAW), their order barely touches the evaluated set.

The brief's adoption gate — *search CPU falls substantially while settled
density stays approximately unchanged* — is therefore NOT met by the
prior alone. The honest conclusion: keep the prior wired and mode-gated
(zero risk, real ordering value, density-identical) and make it the
confidence input for the **adaptive foreground budget** — search effort =
f(system pressure, queue depth, class confidence) — the brief's
identified follow-up, which converts the ordering advantage into skipped
expensive-family work. That is 12C-1.
