//! Benchmark corpora (§41, methodology §1): deterministic, hashable,
//! reproducible logical data sets. Every corpus records its name, source,
//! description, content hash (BLAKE3) and per-version hashes so a result
//! can be re-verified byte-for-byte.

#![forbid(unsafe_code)]

use std::path::Path;
use std::process::Command;

/// One benchmark corpus.
///
/// `versions` is the write stream: each element is one full-file version
/// written over the previous one (single-element for one-shot corpora). The
/// corpus *logical content* is the final version; `content_hash` covers it.
#[derive(Debug, Clone)]
pub struct Corpus {
    /// Corpus name (matches `results.json` groups).
    pub name: String,
    /// Human-readable provenance of the corpus.
    pub source: String,
    /// What the corpus models.
    pub description: String,
    /// Write stream: each element is one full-file version written over the
    /// previous (single-element for one-shot corpora).
    pub versions: Vec<Vec<u8>>,
}

impl Corpus {
    /// One-shot corpus from a single byte string.
    pub fn single(bytes: Vec<u8>, name: &str, source: &str, description: &str) -> Corpus {
        Corpus {
            name: name.to_string(),
            source: source.to_string(),
            description: description.to_string(),
            versions: vec![bytes],
        }
    }

    /// Versioned corpus.
    pub fn versioned(
        versions: Vec<Vec<u8>>,
        name: &str,
        source: &str,
        description: &str,
    ) -> Corpus {
        assert!(!versions.is_empty(), "corpus needs at least one version");
        Corpus {
            name: name.to_string(),
            source: source.to_string(),
            description: description.to_string(),
            versions,
        }
    }

    /// Final (logical) content bytes.
    pub fn final_bytes(&self) -> &[u8] {
        self.versions.last().expect("non-empty")
    }

    /// Logical materialized bytes (final version length).
    pub fn logical_bytes(&self) -> u64 {
        self.final_bytes().len() as u64
    }

    /// Total bytes written across all versions (write-path volume).
    pub fn written_bytes(&self) -> u64 {
        self.versions.iter().map(|v| v.len() as u64).sum()
    }

    /// BLAKE3 content hash of the final version (hex).
    pub fn content_hash(&self) -> String {
        blake3::hash(self.final_bytes()).to_hex().to_string()
    }

    /// Per-version BLAKE3 hashes (hex), for write-stream verification.
    pub fn version_hashes(&self) -> Vec<String> {
        self.versions
            .iter()
            .map(|v| blake3::hash(v).to_hex().to_string())
            .collect()
    }
}

/// The EntropyFS source tree packed into one logical byte string.
///
/// Framing: `u32 LE name_len, name, u64 LE content_len, content` per file,
/// files sorted by path. Deterministic per revision; the pack hash is the
/// corpus hash.
pub fn source_tree_pack(repo_root: &Path) -> Result<Vec<u8>, String> {
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    for dir in ["docs", "src", "evidence"] {
        collect_tree(&repo_root.join(dir), &mut files)?;
    }
    for name in [
        "README.md",
        "Cargo.toml",
        "Cargo.lock",
        "rust-toolchain.toml",
        "LICENSE",
        "LICENSE-MIT",
        "LICENSE-APACHE",
    ] {
        let p = repo_root.join(name);
        if p.is_file() {
            files.push(p);
        }
    }
    files.sort();
    let mut out: Vec<u8> = Vec::new();
    for p in &files {
        let rel = p
            .strip_prefix(repo_root)
            .map_err(|_| "corpus path outside repo".to_string())?
            .to_string_lossy()
            .into_owned();
        let bytes = std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()))?;
        let name_len =
            u32::try_from(rel.len()).map_err(|_| format!("corpus filename too long: {rel}"))?;
        out.extend_from_slice(&name_len.to_le_bytes());
        out.extend_from_slice(rel.as_bytes());
        out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        out.extend_from_slice(&bytes);
    }
    Ok(out)
}

fn collect_tree(dir: &Path, files: &mut Vec<std::path::PathBuf>) -> Result<(), String> {
    let mut entries: Vec<std::path::PathBuf> = Vec::new();
    let rd = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for e in rd {
        let e = e.map_err(|e| e.to_string())?;
        let p = e.path();
        if p.is_dir() {
            collect_tree(&p, files)?;
        } else if p.is_file() {
            entries.push(p);
        }
    }
    entries.sort();
    files.extend(entries);
    Ok(())
}

/// The structured synthetic corpus (text / zeros / low-cardinality /
/// random-ish), **byte-for-byte identical** to the Phase-4 ablation corpus
/// (§43) so campaign runs reproduce the `evidence/ablation-2026-08-25`
/// fixture and the DSFB write-throughput signal on the same input. Keep
/// this generator in sync with `cli/benchmark.rs`.
pub fn structured(size_mib: u64) -> Corpus {
    let mut bytes: Vec<u8> = Vec::with_capacity((size_mib * 1024 * 1024) as usize);
    let mut chunk: Vec<u8> = Vec::with_capacity(64 * 1024);
    let mut written = 0u64;
    while written < size_mib * 1024 * 1024 {
        chunk.clear();
        let pattern = (written / (1024 * 1024)) % 4;
        match pattern {
            0 => {
                for i in 0..65536u32 {
                    chunk.push(b'a' + (i % 26) as u8);
                }
            }
            1 => chunk.resize(65536, 0),
            2 => {
                for i in 0..65536u32 {
                    chunk.push((i % 7) as u8);
                }
            }
            _ => {
                for i in 0..65536u32 {
                    chunk.push((i.wrapping_mul(2654435761) >> 8) as u8);
                }
            }
        }
        bytes.extend_from_slice(&chunk);
        written += 65536;
    }
    Corpus::single(
        bytes,
        "structured",
        "synthetic: 4 MiB zones of text/zeros/low-cardinality/random (Phase-4 ablation corpus, byte-identical)",
        "structured, low-cardinality, periodic and random-looking 64 KiB chunks; identical to the Phase-4 ablation corpus generator",
    )
}

/// Versioned drift corpus (H2): a base file plus successive versions with
/// deterministic, growing, per-chunk mutations. Sequential writes of these
/// versions give the P0 (previous-version-of-same-chunk) channel maximum
/// opportunity.
pub fn versioned(base_mib: u64, versions: usize) -> Corpus {
    let chunk_count = (base_mib * 1024 * 1024 / 65536) as usize;
    let mut chunks: Vec<Vec<u8>> = Vec::with_capacity(chunk_count);
    let base_seed = 0x9e37_79b9_7f4a_7c15u64;
    for j in 0..chunk_count {
        let mut seed = base_seed ^ (j as u64).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        let mut c: Vec<u8> = Vec::with_capacity(65536);
        match j % 4 {
            0 => {
                for i in 0..65536u32 {
                    c.push(b'A' + ((j as u32 * 65536 + i) % 26) as u8);
                }
            }
            1 => c.resize(65536, 0),
            2 => {
                for i in 0..65536u32 {
                    c.push(((j as u32 * 65536 + i) % 7) as u8);
                }
            }
            _ => {
                for _ in 0..65536 {
                    c.push((splitmix64(&mut seed) >> 32) as u8);
                }
            }
        }
        chunks.push(c);
    }

    let mut stream: Vec<Vec<u8>> = Vec::new();
    stream.push(chunks.concat());
    for v in 1..versions {
        let mut next: Vec<Vec<u8>> = Vec::with_capacity(chunk_count);
        let mut vseed = 0xd1b5_4a32_d192_ed03u64 ^ (v as u64).wrapping_mul(0x9e37_79b9);
        for (j, base_chunk) in chunks.iter().enumerate() {
            let mut c = base_chunk.clone();
            // Mutation count grows with version and varies per chunk.
            let n = (4 + 12 * v) * (1 + j % 3);
            for _ in 0..n {
                let pos = (splitmix64(&mut vseed) % 65536) as usize;
                let val = (splitmix64(&mut vseed) >> 32) as u8;
                c[pos] ^= val;
            }
            next.push(c);
        }
        stream.push(next.concat());
    }
    Corpus::versioned(
        stream,
        "versioned",
        "synthetic: 8 drift versions of a 4 MiB structured file (deterministic per-chunk mutations)",
        "H2 base+residual test: P0 previous-version bases should capture small residuals on early versions",
    )
}

/// Shuffled-temporal-history control: the same versioned byte stream, but
/// each version's chunks are placed at permuted offsets (stride coprime to
/// the chunk count, differing per version). P0 (previous bytes at the same
/// offset) then points at unrelated chunks, so temporal/base gains must
/// disappear if the history signal is causal.
pub fn shuffled_versioned(base_mib: u64, versions: usize) -> Corpus {
    let seq = versioned(base_mib, versions);
    let chunk_count = (base_mib * 1024 * 1024 / 65536) as usize;
    // Strides coprime to 64 (and 16, 8, 4, 2 divisors), differing per version.
    let strides = [
        3u64, 5, 7, 9, 11, 13, 15, 17, 19, 21, 23, 25, 27, 29, 31, 33,
    ];
    let mut stream: Vec<Vec<u8>> = Vec::new();
    for (v, version) in seq.versions.iter().enumerate() {
        let stride = strides[v.min(strides.len() - 1)];
        let mut permuted: Vec<u8> = vec![0u8; version.len()];
        for j in 0..chunk_count {
            let dst = (((j as u64 * stride) % chunk_count as u64) * 65536) as usize;
            let src = j * 65536;
            permuted[dst..dst + 65536].copy_from_slice(&version[src..src + 65536]);
        }
        stream.push(permuted);
    }
    Corpus::versioned(
        stream,
        "shuffled",
        "synthetic: versioned stream with per-version chunk permutation (temporal adjacency destroyed)",
        "methodology §5 negative control: shuffled temporal history must eliminate base+residual gains",
    )
}

/// Incompressible control: a splitmix64 byte stream with a fixed seed.
/// Expected: RAW or near-RAW accounting (methodology §5).
pub fn urandom(size_mib: u64, seed: u64) -> Corpus {
    let mut bytes: Vec<u8> = Vec::with_capacity((size_mib * 1024 * 1024) as usize);
    let mut s = seed;
    while (bytes.len() as u64) < size_mib * 1024 * 1024 {
        bytes.extend_from_slice(&splitmix64(&mut s).to_le_bytes());
    }
    bytes.truncate((size_mib * 1024 * 1024) as usize);
    Corpus::single(
        bytes,
        "urandom",
        "synthetic: splitmix64 stream, fixed seed (incompressible control)",
        "methodology §5 negative control: random input must fall back toward RAW",
    )
}

/// Already-compressed control: `zstd -19` of the source pack. Expected:
/// little or no additional gain (methodology §5). Best-effort: if `zstd` is
/// absent the corpus is skipped by the caller.
pub fn compressed_zstd(pack: &[u8], level: i32) -> Result<Corpus, String> {
    let tmp_in = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
    std::fs::write(tmp_in.path(), pack).map_err(|e| e.to_string())?;
    let tmp_out = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
    let out_path = tmp_out.path().to_path_buf();
    let status = Command::new("zstd")
        .args(["-q", &format!("-{level}"), "-c"])
        .arg(tmp_in.path())
        .stdout(std::fs::File::create(&out_path).map_err(|e| e.to_string())?)
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|e| format!("zstd: {e}"))?;
    if !status.success() {
        return Err(format!("zstd exit {status}"));
    }
    let bytes = std::fs::read(&out_path).map_err(|e| e.to_string())?;
    Ok(Corpus::single(
        bytes,
        "compressed-z19",
        &format!("zstd -{level} of the source pack (already-compressed control)"),
        "methodology §5 negative control: already-compressed data must show little or no additional gain",
    ))
}

/// splitmix64 (fixed seed → deterministic, incompressible-looking stream).
fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structured_is_reproducible() {
        let a = structured(1);
        let b = structured(1);
        assert_eq!(a.final_bytes(), b.final_bytes());
        assert_eq!(a.content_hash(), b.content_hash());
        assert_eq!(a.logical_bytes(), 1024 * 1024);
    }

    #[test]
    fn versioned_has_distinct_versions() {
        let c = versioned(1, 4);
        assert_eq!(c.versions.len(), 4);
        let hashes = c.version_hashes();
        for w in hashes.windows(2) {
            assert_ne!(w[0], w[1], "versions must drift");
        }
        assert_eq!(c.final_bytes().len(), 1024 * 1024);
    }

    #[test]
    fn shuffled_preserves_byte_multiset() {
        let seq = versioned(1, 3);
        let shuf = shuffled_versioned(1, 3);
        for (a, b) in seq.versions.iter().zip(shuf.versions.iter()) {
            let mut sa: Vec<u8> = a.clone();
            let mut sb: Vec<u8> = b.clone();
            sa.sort_unstable();
            sb.sort_unstable();
            assert_eq!(sa, sb, "permutation must not change the byte multiset");
        }
        // The final contents differ from sequential placement.
        assert_ne!(seq.final_bytes(), shuf.final_bytes());
    }

    #[test]
    fn urandom_is_deterministic() {
        let a = urandom(1, 42);
        let b = urandom(1, 42);
        assert_eq!(a.content_hash(), b.content_hash());
        let c = urandom(1, 43);
        assert_ne!(a.content_hash(), c.content_hash());
    }

    #[test]
    fn source_pack_is_deterministic() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let a = source_tree_pack(root).unwrap();
        let b = source_tree_pack(root).unwrap();
        assert_eq!(a, b);
        assert!(!a.is_empty());
        // The pack must actually contain sources.
        let s = String::from_utf8_lossy(&a);
        assert!(s.contains("src/lib.rs") || s.contains("src/main.rs"));
    }

    #[test]
    fn splitmix_stream_is_incompressible_looking() {
        // Adjacent-byte first-order estimate: a constant stream would have
        // zero distinct bytes; a real RNG has ~256 distinct byte values.
        let c = urandom(1, 7);
        let mut set = std::collections::BTreeSet::new();
        for b in c.final_bytes().iter().take(65536) {
            set.insert(*b);
        }
        assert!(set.len() > 200, "expected near-uniform byte distribution");
    }
}
