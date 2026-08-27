//! The shared adoption-wedge corpus (12E.13) — the SAME deterministic
//! workloads the adoption court measured, reused verbatim by the 12C-1
//! adaptive-budget oracle so its settled-bytes curves are directly
//! comparable to the sealed 12E.13 footprint numbers.
//!
//! # PURPOSE
//!
//! One corpus generator, two courts. The 12E.13 adoption court
//! (`src/tests/adoption_oracle.rs`) measured the settled footprint wedge
//! (build-artifacts 0.049×, four workloads clear 10×); the 12C-1
//! adaptive-budget oracle (`src/tests/adaptive_budget_probe.rs`) measures
//! how much of that wedge survives reduced foreground search effort.
//! Both must run the IDENTICAL bytes or the curves cannot be compared.
//!
//! # DETERMINISM
//!
//! Every byte derives from the workload/version/index parameters and a
//! fixed LCG seed — no wall-clock, no randomness. The generators are
//! pure: same call sequence → same bytes.

#![forbid(unsafe_code)]

/// Deterministic pseudo-random (LCG) helper.
pub fn lcg(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    *state >> 16
}

/// Fill `buf` with template lines carrying a (workload, version, index)
/// stamp so near-duplicates share structure but differ deterministically.
pub fn stamp_lines(buf: &mut Vec<u8>, tag: &str, ver: u64, idx: u64, line: &str) {
    let target = buf.capacity().min(1 << 20);
    while buf.len() < target {
        buf.extend_from_slice(
            format!(
                "{line} // {tag} v{ver} item{idx} seq{seq}\n",
                seq = buf.len() % 4096
            )
            .as_bytes(),
        );
    }
    buf.truncate(target);
}

/// One workload: a named set of immutable blobs (the adoption corpus).
pub struct Workload {
    pub name: &'static str,
    pub blobs: Vec<Vec<u8>>,
}

/// The six brief-mandated workload families. Deterministic.
pub fn workloads() -> Vec<Workload> {
    let mut out = Vec::new();
    let mut seed: u64 = 0xDEAD_BEEF_CAFE_F00D;

    // 1. Versioned build artifacts: 12 versions × 200 objects; 80% of
    //    each version is byte-identical to the previous (dedup), 20%
    //    carries a version stamp (near-duplicate).
    {
        let mut blobs = Vec::new();
        for ver in 0..12u64 {
            for i in 0..200u64 {
                let mut b = Vec::with_capacity(4096 + ((i * 37) % 60000) as usize);
                if i % 5 == 0 {
                    // the changed fifth: version-stamped object
                    stamp_lines(&mut b, "build", ver, i, "artifact(v){v} = {a} {b} {c}");
                } else {
                    // the stable 80%: identical across versions
                    stamp_lines(&mut b, "build", 0, i, "stable_object = {a} {b} {c}");
                }
                blobs.push(b);
            }
        }
        out.push(Workload {
            name: "build-artifacts",
            blobs,
        });
    }

    // 2. Incremental source trees: 10 versions × 150 files; each file
    //    changes only its version stamp + a few edited lines.
    {
        let mut blobs = Vec::new();
        for ver in 0..10u64 {
            for i in 0..150u64 {
                let mut b = Vec::with_capacity(2048 + ((i * 53) % 24000) as usize);
                stamp_lines(
                    &mut b,
                    "src",
                    ver,
                    i,
                    "fn handler_{i}(ctx: &mut Ctx) -> Result<(), E> { let v = ctx.get({i}); v.map(|_| ()) }",
                );
                // a few genuinely edited lines per version
                for e in 0..(ver % 5) {
                    b.extend_from_slice(
                        format!("// edit {ver}.{e} applied to file {i}\n").as_bytes(),
                    );
                }
                blobs.push(b);
            }
        }
        out.push(Workload {
            name: "source-trees",
            blobs,
        });
    }

    // 3. Container-like layers: 8 layers; each layer keeps 60% of the
    //    previous layer's files byte-identical and adds 40% new.
    {
        let mut blobs = Vec::new();
        let mut kept: Vec<Vec<u8>> = Vec::new();
        for layer in 0..8u64 {
            let mut next_kept: Vec<Vec<u8>> = Vec::new();
            // keep 60% of previous layer's files (or seed files)
            let prev_len = kept.len().max(100);
            for i in 0..prev_len {
                if i % 10 < 6 {
                    let b = if kept.is_empty() {
                        let mut x = Vec::with_capacity(8192);
                        stamp_lines(&mut x, "layer", 0, i as u64, "layer_base_file = {a} {b}");
                        x
                    } else {
                        kept[i].clone()
                    };
                    next_kept.push(b.clone());
                    blobs.push(b);
                }
            }
            // add 40% new
            for i in 0..(prev_len / 10 * 4) {
                let mut b = Vec::with_capacity(8192);
                stamp_lines(&mut b, "layer", layer, i as u64, "layer_new_file = {a} {b}");
                next_kept.push(b.clone());
                blobs.push(b);
            }
            kept = next_kept;
        }
        out.push(Workload {
            name: "container-layers",
            blobs,
        });
    }

    // 4. Near-duplicate generated assets: 50 assets from one template
    //    with per-asset parameters (strong shared structure).
    {
        let mut blobs = Vec::new();
        for i in 0..50u64 {
            let mut b = Vec::with_capacity(16 * 1024);
            stamp_lines(
                &mut b,
                "asset",
                0,
                i,
                "asset {{ id: {i}, palette: [r,g,b], scale: {s}, label: \"gen-{i}\" }}",
            );
            blobs.push(b);
        }
        out.push(Workload {
            name: "generated-assets",
            blobs,
        });
    }

    // 5. CI/cache-style object population: 300 objects; 60% unique, 40%
    //    exact duplicates of earlier ones (cache hits); mixed sizes.
    {
        let mut blobs = Vec::new();
        let mut pool: Vec<Vec<u8>> = Vec::new();
        for i in 0..300u64 {
            if i >= 120 && i % 10 < 4 {
                // duplicate a random earlier object (cache hit)
                let hit = pool[(lcg(&mut seed) as usize) % pool.len()].clone();
                blobs.push(hit);
                continue;
            }
            let mut b = Vec::with_capacity(1024 + ((i * 97) % 63000) as usize);
            stamp_lines(&mut b, "ci", 0, i, "cache_entry = {hash} {status} {bytes}");
            pool.push(b.clone());
            blobs.push(b);
        }
        out.push(Workload {
            name: "ci-cache",
            blobs,
        });
    }

    // 6. Versioned scientific outputs: 6 versions × 20 outputs of
    //    64–512 KiB; mostly stable across versions (high dedup).
    {
        let mut blobs = Vec::new();
        for ver in 0..6u64 {
            for i in 0..20u64 {
                let mut b = Vec::with_capacity(64 * 1024 + ((i * 131) % (448 * 1024)) as usize);
                stamp_lines(
                    &mut b,
                    "science",
                    ver,
                    i,
                    "run {i} metric={m} value={v} timestamp=1787840{ver}00",
                );
                blobs.push(b);
            }
        }
        out.push(Workload {
            name: "scientific-outputs",
            blobs,
        });
    }

    out
}
