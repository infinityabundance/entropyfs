//! Phase-12D-0 oracle: the grammar-addressed entropy concept, tested
//! OFFLINE with full byte accounting (diagnostic — never a format
//! change).
//!
//! The 12D brief's first deliverable: **train grammar candidates on a
//! real tree, encode all members, FULLY account grammar + state +
//! residual + descriptor bytes, and compare against the incumbents — if
//! it loses, stop.**
//!
//! # The model under test
//!
//! ```text
//! X = Render(G, Θ) ⊕ R
//!   G = a bounded template grammar (Literal skeleton + Slot positions)
//!   Θ = per-member production state (the slot values)
//!   R = the exact residual (0 when the grammar covers every byte)
//! ```
//!
//! The grammar object (the literal skeleton) is stored ONCE; each member
//! stores only its slot values (state), the uncovered bytes (residual),
//! and a tiny descriptor (grammar id + lengths). Nothing is free: the
//! grammar's own bytes are counted exactly once, and if
//! `|G| + Σ(|Θ| + |R| + descriptor)` does not beat the incumbents, the
//! concept loses on this corpus.
//!
//! # The bounded induction
//!
//! For each member set: the longest common PREFIX and SUFFIX become the
//! grammar's leading/trailing literals; the varying middle is the slot.
//! A second pass splits the middle on the longest internal common
//! substring (when the members share a mid-file literal), bounding the
//! slot count at [`MAX_SLOTS`]. This is the simplest sound template
//! grammar — the brief's "literal skeleton + variable slots" level; it
//! deliberately does NOT attempt full grammar discovery (the brief's
//! "don't discover the perfect grammar; use bounded candidate families").
//!
//! # The corpora
//!
//! - **generated-config**: the brief's own example — N generated config
//!   files sharing a skeleton with per-member field values. The
//!   grammar-friendly class (the honest positive case).
//! - **diverse**: a mixed tree (source-like text, binary-ish noise,
//!   zeros, prose) with no shared skeleton — the honest negative
//!   control: the grammar must LOSE here (the concept is not magic).
//!
//! # The incumbents
//!
//! - EntropyFS foreground: the tree written to a fresh store, reachable
//!   bytes after a checkpoint.
//! - EntropyFS settled: plus the background optimizer (`optimize_pass` +
//!   the shared-dict pass) — the in-repo dictionary/cohort machinery.
//! - zstd whole-pack and zstd per-file (external `zstd`, best-effort:
//!   skipped when the binary is absent).
//!
//! # The verdict rule
//!
//! The grammar is adopted for a format-bit investigation (12D-1) only if
//! the FULLY-ACCOUNTED total beats EVERY incumbent on the
//! grammar-friendly corpus while the negative control still loses (the
//! concept is real, not coincidence). Any other outcome: record and
//! stop, per the brief.
//!
//! # Phase-12D-1 (the second round, `grammar_ec_oracle`)
//!
//! The 12D-0 verdict STOPPED because zstd-whole (29 731 B) beat the
//! fully-accounted RAW-skeleton grammar (66 059 B): the grammar stored
//! its irregular skeleton LITERALLY while zstd entropy-coded it. The
//! 12D-1 round is the brief's own "persisted entropy" refinement: the
//! grammar object is itself a byte string, so in the real design it
//! would be stored as a normal content-addressed CHUNK — put through
//! the store's representation search and charged its smallest valid
//! candidate's persisted bytes (descriptor + model + objects +
//! integrity). `grammar_ec_total = chunk_cost(skeleton) + Σ(state +
//! descriptor)` with nothing hidden, exactly the 12D-0 accounting with
//! the literal skeleton replaced by its entropy-coded form.
//!
//! The 12D-1 gate is the same: the entropy-coded grammar must beat
//! EVERY incumbent (zstd-whole included) on the grammar-friendly corpus
//! while the diverse control still loses — only then is the format-bit
//! investigation justified.
//!
//! The probe prints the table and writes its TSV to `$GRAMMAR_ORACLE_OUT`
//! when set; `$GRAMMAR_ORACLE_MODE` stamps the header. Debug runs a
//! reduced smoke sweep.

#![forbid(unsafe_code)]

use std::time::Instant;

use crate::optimizer::policy::OptimizeOptions;
use crate::store::transaction::CrashHooks;
use crate::store::{NewEntry, Store, StoreConfig};
use tempfile::TempDir;

/// Bounded slot count per grammar (the induction never exceeds this).
const MAX_SLOTS: usize = 8;
/// Bounded member count for the induction (the corpus is capped).
/// Bounded member count for the grammar corpus.
#[allow(dead_code)] // documented corpus bound; exercised via the corpus builder
const MAX_MEMBERS: usize = 512;

/// The bounded induction's search cap: the LCS is computed over at most
/// this many bytes of each region's head; a common tail beyond the cap is
/// verified once and emitted as a literal, else folded into the slot.
const LCS_HEAD: usize = 1024;
/// The longest literal the induction emits (short structural literals).
const MAX_LITERAL: usize = 256;

/// One induced template grammar: the literal skeleton segments and the
/// slot lengths per member. Render(G, Θ) = segments[0] ⊕ slot0 ⊕
/// segments[1] ⊕ slot1 ⊕ … ⊕ segments[k]; every member byte is either a
/// literal (the skeleton, stored once) or a slot value (the state,
/// stored per member) — the residual is structurally 0 for this
/// induction, so the fully-accounted total is
/// `grammar + Σ(state + descriptor)` with nothing hidden.
#[derive(Debug, Clone)]
struct TemplateGrammar {
    /// Literal segments: `segments[0] + slot0 + segments[1] + …`.
    segments: Vec<Vec<u8>>,
    /// Slot lengths per member.
    slot_lens: Vec<Vec<usize>>,
}

/// The persisted size of one literal segment under the brief's `Repeat`
/// node: a segment that is k ≥ 2 repetitions of a block stores the block
/// once plus the count (2 bytes) — periodic filler (e.g. the padding)
/// is not stored literally. Returns `(block_len, count)`.
fn repeat_shape(seg: &[u8]) -> (usize, usize) {
    if seg.is_empty() {
        return (0, 0);
    }
    // The minimal period: the smallest p with seg[i] == seg[i % p] for
    // every i (bounded by the segment length).
    for p in 1..=seg.len() / 2 {
        if seg.iter().enumerate().all(|(i, &b)| seg[i % p] == b) {
            return (p, seg.len() / p);
        }
    }
    (seg.len(), 1)
}

impl TemplateGrammar {
    /// The grammar object's persisted size (the skeleton bytes, once,
    /// with `Repeat`-compressed periodic segments).
    fn grammar_bytes(&self) -> u64 {
        self.segments
            .iter()
            .map(|s| {
                let (block, count) = repeat_shape(s);
                if count >= 2 {
                    (block + 2) as u64
                } else {
                    s.len() as u64
                }
            })
            .sum()
    }

    /// The ACTUAL persisted skeleton payload bytes (exactly the bytes
    /// [`TemplateGrammar::grammar_bytes`] accounts): each segment stored
    /// either literally or as `block + 2-byte count` under the brief's
    /// `Repeat` node. This is the grammar object's literal form; 12D-1
    /// entropy-codes it (see [`grammar_chunk_cost`]).
    fn skeleton_payload(&self) -> Vec<u8> {
        let mut payload = Vec::with_capacity(self.grammar_bytes() as usize);
        for seg in &self.segments {
            let (block, count) = repeat_shape(seg);
            if count >= 2 {
                payload.extend_from_slice(&seg[..block]);
                payload.push((count & 0xff) as u8);
                payload.push(((count >> 8) & 0xff) as u8);
            } else {
                payload.extend_from_slice(seg);
            }
        }
        payload
    }

    /// The fully-accounted total for all members: grammar once +
    /// Σ(state + descriptor) per member. The state is the slot values
    /// (raw — the conservative accounting; the brief's rank/residual
    /// tightening is 12D-1 refinement); the descriptor is the per-member
    /// header (grammar id + slot count + per-slot lengths).
    fn total_bytes(&self) -> u64 {
        let mut total = self.grammar_bytes();
        for lens in &self.slot_lens {
            let state: u64 = lens.iter().map(|l| *l as u64).sum();
            let descriptor = 8 + 1 + lens.len() as u64;
            total += state + descriptor;
        }
        total
    }
}

fn common_prefix(members: &[&[u8]]) -> usize {
    let mut n = 0usize;
    'outer: while n < members[0].len() {
        let b = members[0][n];
        for m in members.iter().skip(1) {
            if n >= m.len() || m[n] != b {
                break 'outer;
            }
        }
        n += 1;
    }
    n
}

fn common_suffix(members: &[&[u8]], prefix: usize) -> usize {
    let mut n = 0usize;
    'outer: while n < members[0].len().saturating_sub(prefix) {
        let b = members[0][members[0].len() - 1 - n];
        for m in members.iter().skip(1) {
            if n >= m.len().saturating_sub(prefix) || m[m.len() - 1 - n] != b {
                break 'outer;
            }
        }
        n += 1;
    }
    n
}

/// The longest byte substring common to every region's HEAD (at most
/// [`LCS_HEAD`] bytes), as `(length, position-in-region[0])`. The naive
/// scan is bounded: the candidate length caps at [`MAX_LITERAL`] (the
/// internal skeleton literals are short structural strings — the long
/// common material is the prefix/suffix, already extracted) and the
/// position scan is over the head. The tail beyond the head is handled
/// by the caller (verified once as a literal, else folded into a slot).
fn longest_common_substring(regions: &[&[u8]]) -> Option<(usize, usize)> {
    let first = regions[0];
    let n = first.len().min(LCS_HEAD);
    if n == 0 {
        return None;
    }
    let max_len = n.min(MAX_LITERAL);
    for len in (1..=max_len).rev() {
        for pos in 0..=n - len {
            let cand = &first[pos..pos + len];
            if regions.iter().all(|r| contains_head(r, cand)) {
                return Some((len, pos));
            }
        }
    }
    None
}

/// Whether `needle` occurs in the head (first [`LCS_HEAD`] bytes) of
/// `haystack`.
fn contains_head(haystack: &[u8], needle: &[u8]) -> bool {
    let head = &haystack[..haystack.len().min(LCS_HEAD)];
    head.windows(needle.len()).any(|w| w == needle)
}

/// The oracle's substring helper (kept for the exhibit drivers; the
/// corpus itself uses the same primitive).
#[allow(dead_code)]
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Induce a template grammar from a member set: the longest common
/// prefix/suffix become leading/trailing literals; the middle is split
/// by repeatedly cutting the longest common internal substring as a
/// literal (bounded by [`MAX_SLOTS`]), so the varying fields become the
/// slots. Every member byte is covered by construction (a slot or a
/// literal) — the template is EXACT for these corpora.
fn induce(members: &[&[u8]]) -> TemplateGrammar {
    assert!(!members.is_empty());
    let prefix = common_prefix(members);
    let suffix = common_suffix(members, prefix);
    let mut segments = vec![members[0][..prefix].to_vec()];
    // The working region per member: the middle between the common
    // prefix and suffix.
    let mut region: Vec<&[u8]> = members
        .iter()
        .map(|m| &m[prefix..m.len() - suffix])
        .collect();
    let mut slot_lens: Vec<Vec<usize>> = vec![Vec::new(); members.len()];
    for _ in 0..MAX_SLOTS {
        if region.iter().all(|r| r.is_empty()) {
            break;
        }
        match longest_common_substring(&region) {
            Some((len, pos)) => {
                // The left pieces become a slot; the literal is emitted;
                // the right pieces continue as the new region.
                for (m, lens) in slot_lens.iter_mut().enumerate() {
                    lens.push(pos.min(region[m].len()));
                }
                segments.push(region[0][pos..pos + len].to_vec());
                region = region
                    .iter()
                    .map(|r| {
                        let _start = pos.min(r.len());
                        let end = (pos + len).min(r.len());
                        &r[end..]
                    })
                    .collect();
            }
            None => {
                // No common internal literal: the whole region is one
                // slot.
                for (m, lens) in slot_lens.iter_mut().enumerate() {
                    lens.push(region[m].len());
                }
                region = vec![&[][..]; members.len()];
            }
        }
    }
    segments.push(members[0][members[0].len() - suffix..].to_vec());
    TemplateGrammar {
        segments,
        slot_lens,
    }
}

/// A generated-config file: shared skeleton + per-member field values.
/// A generated-config file: a NON-PERIODIC shared skeleton (deterministic
/// irregular bytes, identical across members — incompressible by RANS and
/// not periodic, so PERIODIC/RANS cannot trivialize it and the
/// dictionary/grammar machinery must carry the shared structure) plus
/// per-member field values, padded to a full 64 KiB chunk.
fn config_file(i: usize, seed: u64) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"# generated config\n");
    // The shared irregular skeleton (~60 KiB, identical for every member).
    let mut s = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0x12d_0001;
    while out.len() < 60000 {
        for _ in 0..32 {
            s = s
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            out.push(b"abcdefghijklmnopqrstuvwxyz0123456789{}();,= \n"[((s >> 33) as usize) % 45]);
        }
        out.extend_from_slice(b"// common section\n");
    }
    // The per-member fields (the grammar's slots).
    out.extend_from_slice(format!("host = node-{i:04}\n").as_bytes());
    out.extend_from_slice(format!("port = {}\n", 8000 + (i * 7 + seed as usize) % 200).as_bytes());
    out.extend_from_slice(b"user = svc\n");
    out.extend_from_slice(format!("flags = {}\n", "ab".repeat(i % 5)).as_bytes());
    out.extend_from_slice(b"# end\n");
    out.truncate(65536);
    out
}

/// A diverse member: no shared skeleton (the negative control), each a
/// full 64 KiB chunk of mixed content.
fn diverse_file(i: usize, seed: u64) -> Vec<u8> {
    let mut out = Vec::new();
    match i % 4 {
        0 => {
            out.extend_from_slice(
                format!("module {i}: fn main() {{ println!(\"hello {i}\"); }}\n").as_bytes(),
            );
            let alpha: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789{}();,= \n";
            let mut s = seed.wrapping_add(i as u64 * 17);
            while out.len() < 65536 {
                for _ in 0..64 {
                    s = s
                        .wrapping_mul(6364136223846793005)
                        .wrapping_add(1442695040888963407);
                    out.push(alpha[((s >> 33) as usize) % alpha.len()]);
                }
                out.extend_from_slice(format!("// section {i}\n").as_bytes());
            }
        }
        1 => {
            let mut s = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ (i as u64) << 32;
            for _ in 0..65536 {
                s = s
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                out.push((s >> 33) as u8);
            }
        }
        2 => out.extend_from_slice(&vec![0u8; 65536]),
        _ => {
            out.extend_from_slice(
                format!("record {i}: key={} value={}\n", i * 3, i * 7).as_bytes(),
            );
            while out.len() < 65536 {
                out.extend_from_slice(&vec![b'x'; 256]);
            }
        }
    }
    out.truncate(65536);
    out
}

/// EntropyFS reachable bytes for a member set (fresh store + checkpoint,
/// then with the background passes).
fn efs_reachable(members: &[Vec<u8>], settled: bool) -> (u64, u64) {
    let dir = TempDir::new().unwrap();
    let cfg = StoreConfig {
        segment_size: 128 * 1024 * 1024,
        ..Default::default()
    };
    let store = Store::create(dir.path(), &cfg, [0x77; 16]).unwrap();
    let root = store.current_root().root_dir_ino;
    let hooks = &CrashHooks::none();
    let opts = OptimizeOptions::default();
    let fg = store.foreground_policy();
    let mut inos = Vec::new();
    for (i, m) in members.iter().enumerate() {
        let ino = store
            .create_entry(
                root,
                format!("f{i:04}").as_bytes(),
                NewEntry::file(0o644, 1000, 1000),
                hooks,
            )
            .unwrap();
        store.epoch_write(ino, 0, m, opts, fg, hooks).unwrap();
        inos.push(ino);
    }
    store.epoch_checkpoint(hooks).unwrap();
    if settled {
        crate::optimizer::background::optimize_pass(&store, opts, None, None).unwrap();
        crate::optimizer::background::shared_dict_pass(&store, opts, None).unwrap();
        store.epoch_checkpoint(hooks).unwrap();
    }
    let logical = store.logical_bytes().unwrap();
    let reachable: u64 = crate::store::gc::mark_live(&store)
        .unwrap()
        .into_iter()
        .filter_map(|id| store.object_index().get(&id).map(|loc| loc.total_size()))
        .sum();
    let _ = inos;
    (logical, reachable)
}

/// zstd whole-pack size (best-effort; None when the binary is absent).
fn zstd_whole(members: &[Vec<u8>]) -> Option<u64> {
    let pack: Vec<u8> = members.iter().flat_map(|m| m.iter().copied()).collect();
    let tmp_in = tempfile::NamedTempFile::new().ok()?;
    std::fs::write(tmp_in.path(), &pack).ok()?;
    let tmp_out = tempfile::NamedTempFile::new().ok()?;
    let out_path = tmp_out.path().to_path_buf();
    let status = std::process::Command::new("zstd")
        .args(["-q", "-19", "-c"])
        .arg(tmp_in.path())
        .stdout(std::fs::File::create(&out_path).ok()?)
        .stderr(std::process::Stdio::null())
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }
    std::fs::metadata(out_path).ok().map(|m| m.len())
}

/// Phase 12D-1: the persisted cost of the grammar skeleton as a CHUNK —
/// the smallest candidate over the standalone families the write path
/// would give a grammar object (byte-rANS, sequence-rANS, the
/// configurational families, RAW), charged the candidate's FULL
/// accounted persisted bytes (descriptor + model + objects + integrity
/// — `CostBreakdown::persisted_bytes`, the store's own authority).
/// Returns `(bytes, winning family name)`.
///
/// This is the "the grammar object is itself data" refinement: in the
/// real `Representation::Grammar { grammar: ChunkId, .. }` design the
/// skeleton would be a normal content-addressed object, and this is
/// exactly what the store would charge for it.
fn grammar_chunk_cost(
    limits: &crate::core::limits::Limits,
    policy: &crate::core::cost::Policy,
    payload: &[u8],
) -> (u64, &'static str) {
    use crate::core::candidate::{Candidate, Encoder};
    let cid = crate::core::extent::ChunkId::of(payload);
    let base_ctx = crate::core::candidate::CandidateContext {
        limits,
        policy,
        content_id: cid,
        bases: &[],
        dedup: None,
    };
    let mut best: Option<(u64, &'static str)> = None;
    let mut consider = |cands: Vec<Candidate>, name: &'static str| {
        for c in cands {
            let b = c.cost.persisted_bytes();
            if best.map(|(x, _)| b < x).unwrap_or(true) {
                best = Some((b, name));
            }
        }
    };
    consider(
        crate::rans::residual::RansEncoder.encode(payload, &base_ctx),
        "RANS",
    );
    consider(
        crate::rans::sequence::SequenceEncoder.encode(payload, &base_ctx),
        "SEQ_RANS",
    );
    consider(
        crate::entropy::sparse::SparseEncoder.encode(payload, &base_ctx),
        "SPARSE",
    );
    consider(
        crate::entropy::palette::PaletteEncoder.encode(payload, &base_ctx),
        "PALETTE",
    );
    consider(
        crate::entropy::periodic::PeriodicEncoder.encode(payload, &base_ctx),
        "PERIODIC",
    );
    consider(
        crate::entropy::sparse64::SparseBlock64Encoder.encode(payload, &base_ctx),
        "SPARSE64",
    );
    if let Some(r) = crate::core::candidate::raw_candidate(payload, cid, limits) {
        let b = r.cost.persisted_bytes();
        if best.map(|(x, _)| b < x).unwrap_or(true) {
            best = Some((b, "RAW"));
        }
    }
    best.unwrap_or_else(|| (payload.len() as u64 + 64, "RAW"))
}

#[test]
fn grammar_oracle() {
    let n = if cfg!(debug_assertions) { 24 } else { 200 };
    let t0 = Instant::now();
    // ---- Corpus 1: the grammar-friendly generated-config tree. ----
    let configs: Vec<Vec<u8>> = (0..n).map(|i| config_file(i, 7)).collect();
    let config_refs: Vec<&[u8]> = configs.iter().map(|c| c.as_slice()).collect();
    let grammar = induce(&config_refs);
    let grammar_total = grammar.total_bytes();
    let logical: u64 = configs.iter().map(|c| c.len() as u64).sum();
    let (efs_fg_logical, efs_fg) = efs_reachable(&configs, false);
    let (efs_set_logical, efs_settled) = efs_reachable(&configs, true);
    let zstd_w = zstd_whole(&configs);

    // ---- Corpus 2: the diverse negative control. ----
    let diverse: Vec<Vec<u8>> = (0..n).map(|i| diverse_file(i, 13)).collect();
    let div_refs: Vec<&[u8]> = diverse.iter().map(|c| c.as_slice()).collect();
    let div_grammar = induce(&div_refs);
    let div_total = div_grammar.total_bytes();
    let div_logical: u64 = diverse.iter().map(|c| c.len() as u64).sum();
    let (_, div_efs_fg) = efs_reachable(&diverse, false);
    let (_, div_efs_settled) = efs_reachable(&diverse, true);
    let div_zstd = zstd_whole(&diverse);

    println!(
        "\n==== Phase-12D grammar oracle (n = {n}; {} s) ====",
        t0.elapsed().as_secs_f32()
    );
    println!(
        "corpus: generated-config — {} files, {logical} logical bytes, grammar skeleton {} B, {} slots/member",
        n,
        grammar.grammar_bytes(),
        grammar.slot_lens[0].len()
    );
    println!(
        "{:<22} {:>12} {:>9} {:>9}",
        "representation", "bytes", "ratio", "vs efs"
    );
    let rows = [
        ("grammar (fully accounted)", grammar_total, "grammar"),
        ("EntropyFS foreground", efs_fg, "efs"),
        ("EntropyFS settled (+dict)", efs_settled, "efs"),
    ];
    for (name, bytes, kind) in rows {
        let ratio = logical as f64 / bytes.max(1) as f64;
        let vs = if kind == "grammar" {
            format!("{:.2}x efs", efs_fg as f64 / bytes.max(1) as f64)
        } else {
            "—".to_string()
        };
        println!("{name:<22} {bytes:>12} {ratio:>8.2}x {vs:>9}");
    }
    if let Some(z) = zstd_w {
        println!(
            "{:<22} {z:>12} {:>8.2}x {:>9}",
            "zstd -19 whole pack",
            logical as f64 / z.max(1) as f64,
            efs_fg as f64 / z.max(1) as f64
        );
    }
    println!(
        "diverse negative control: grammar {div_total} B ({:.2}x), EntropyFS fg {div_efs_fg} B ({:.2}x), settled {div_efs_settled} B, zstd {:?}",
        div_logical as f64 / div_total.max(1) as f64,
        div_logical as f64 / div_efs_fg.max(1) as f64,
        div_zstd
    );

    // ---- The verdict rule ----
    let zstd_beats = zstd_w.map(|z| z < grammar_total).unwrap_or(false);
    let grammar_wins = grammar_total < efs_fg
        && grammar_total < efs_settled
        && zstd_w.map(|z| grammar_total < z).unwrap_or(true)
        && grammar_total < logical;
    let diverse_loses = div_total >= div_efs_fg;
    println!("\n-- verdict --");
    if grammar_wins && diverse_loses {
        println!(
            "ADOPT-FOR-INVESTIGATION: the fully-accounted grammar beats every incumbent on the grammar-friendly corpus and loses on the diverse control — 12D-1 (the format-bit investigation) is justified."
        );
    } else if grammar_wins && !diverse_loses {
        println!(
            "CONDITIONAL: the grammar wins on the config corpus but also 'wins' on the diverse control (likely an induction artifact — investigate before any format work)."
        );
    } else if zstd_beats {
        let z = zstd_w.unwrap_or(0);
        println!(
            "STOP per the brief's gate: the fully-accounted RAW-skeleton grammar beats EntropyFS settled ({:.1}x) but NOT every incumbent — zstd-whole ({z} B vs grammar {grammar_total} B, grammar is {:.1}x LARGER). The identified refinement: the grammar object is itself data and must be entropy-coded (the brief's 'persisted entropy'); the raw-skeleton accounting is the conservative bound. 12D-1 (the format-bit investigation) is NOT justified on this evidence.",
            efs_settled as f64 / grammar_total.max(1) as f64,
            grammar_total as f64 / z.max(1) as f64
        );
    } else {
        println!(
            "STOP: the fully-accounted grammar does not beat the incumbents ({grammar_total} B vs EntropyFS fg {efs_fg} B) — the 12D-1 format-bit investigation is NOT justified on this evidence (the brief's 'if it loses, stop')."
        );
    }
    let _ = efs_fg_logical;
    let _ = efs_set_logical;

    // ---- TSV ----
    let mut tsv = String::new();
    tsv.push_str("mode\tcorpus\tlogical\tgrammar_total\tgrammar_ratio\tefs_fg\tefs_settled\tzstd_whole\tverdict\n");
    let stamp = std::env::var("GRAMMAR_ORACLE_MODE").unwrap_or_else(|_| "unknown".into());
    tsv.push_str(&format!(
        "{stamp}\tgenerated-config\t{logical}\t{grammar_total}\t{:.2}\t{efs_fg}\t{efs_settled}\t{}\t{}\n",
        logical as f64 / grammar_total.max(1) as f64,
        zstd_w.map(|z| z.to_string()).unwrap_or_else(|| "na".into()),
        if grammar_wins { "adopt-for-investigation" } else { "stop" }
    ));
    tsv.push_str(&format!(
        "{stamp}\tdiverse\t{div_logical}\t{div_total}\t{:.2}\t{div_efs_fg}\t{div_efs_settled}\t{}\t{}\n",
        div_logical as f64 / div_total.max(1) as f64,
        div_zstd.map(|z| z.to_string()).unwrap_or_else(|| "na".into()),
        if diverse_loses { "loses-as-expected" } else { "wins-unexpectedly" }
    ));
    if let Ok(path) = std::env::var("GRAMMAR_ORACLE_OUT") {
        if let Some(parent) = std::path::Path::new(&path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&path, &tsv).expect("write oracle summary");
        println!("oracle summary written to {path}");
    }
}

/// Phase 12D-1: the entropy-coded grammar skeleton round.
///
/// The 12D-0 round STOPPED because the grammar stored its irregular
/// skeleton LITERALLY (66 059 B fully accounted) while zstd-whole
/// entropy-coded it (29 731 B) — 2.2× smaller. This round applies the
/// brief's own "persisted entropy" refinement: the grammar object is
/// itself a byte string, so in the real design it is stored as a normal
/// content-addressed chunk and charged its smallest valid candidate's
/// persisted bytes (`grammar_chunk_cost`, the store's own accounting
/// authority: byte-rANS / sequence-rANS / configurational / RAW, exact
/// cost selection). Full accounting: `chunk_cost(skeleton) + Σ(state +
/// descriptor)`. The gate is unchanged: the entropy-coded grammar must
/// beat EVERY incumbent (zstd-whole included) on the grammar-friendly
/// corpus while the diverse control still loses — only then is the
/// format-bit investigation justified.
#[test]
fn grammar_ec_oracle() {
    let n = if cfg!(debug_assertions) { 24 } else { 200 };
    let t0 = Instant::now();
    let limits = crate::core::limits::Limits::default();
    let policy = crate::core::cost::Policy::balanced();

    // ---- Corpus 1: the grammar-friendly generated-config tree. ----
    let configs: Vec<Vec<u8>> = (0..n).map(|i| config_file(i, 7)).collect();
    let config_refs: Vec<&[u8]> = configs.iter().map(|c| c.as_slice()).collect();
    let grammar = induce(&config_refs);
    let skeleton = grammar.skeleton_payload();
    let (chunk_cost, chunk_family) = grammar_chunk_cost(&limits, &policy, &skeleton);
    let state_descriptors = grammar.total_bytes() - grammar.grammar_bytes();
    let grammar_raw_total = grammar.total_bytes(); // the 12D-0 baseline
    let grammar_ec_total = chunk_cost + state_descriptors;
    let logical: u64 = configs.iter().map(|c| c.len() as u64).sum();
    let (_, efs_fg) = efs_reachable(&configs, false);
    let (_, efs_settled) = efs_reachable(&configs, true);
    let zstd_w = zstd_whole(&configs);

    // ---- Corpus 2: the diverse negative control. ----
    let diverse: Vec<Vec<u8>> = (0..n).map(|i| diverse_file(i, 13)).collect();
    let div_refs: Vec<&[u8]> = diverse.iter().map(|c| c.as_slice()).collect();
    let div_grammar = induce(&div_refs);
    let div_skeleton = div_grammar.skeleton_payload();
    let (div_chunk_cost, div_chunk_family) = grammar_chunk_cost(&limits, &policy, &div_skeleton);
    let div_state_descriptors = div_grammar.total_bytes() - div_grammar.grammar_bytes();
    let div_ec_total = div_chunk_cost + div_state_descriptors;
    let div_logical: u64 = diverse.iter().map(|c| c.len() as u64).sum();
    let (_, div_efs_fg) = efs_reachable(&diverse, false);
    let (_, div_efs_settled) = efs_reachable(&diverse, true);
    let div_zstd = zstd_whole(&diverse);

    println!(
        "\n==== Phase-12D-1 entropy-coded grammar oracle (n = {n}; {} s) ====",
        t0.elapsed().as_secs_f32()
    );
    println!(
        "corpus: generated-config — {} files, {logical} logical B, skeleton {} B (chunk-coded by {chunk_family}: {chunk_cost} B), state+descriptors {state_descriptors} B",
        n,
        skeleton.len(),
    );
    println!("{:<26} {:>12} {:>9}", "representation", "bytes", "ratio");
    let ratio_of = |b: u64| logical as f64 / b.max(1) as f64;
    println!(
        "{:<26} {:>12} {:>8.2}x",
        "grammar raw skeleton (12D-0)",
        grammar_raw_total,
        ratio_of(grammar_raw_total)
    );
    println!(
        "{:<26} {:>12} {:>8.2}x",
        "grammar entropy-coded (12D-1)",
        grammar_ec_total,
        ratio_of(grammar_ec_total)
    );
    println!(
        "{:<26} {:>12} {:>8.2}x",
        "EntropyFS settled (+dict)",
        efs_settled,
        ratio_of(efs_settled)
    );
    if let Some(z) = zstd_w {
        println!(
            "{:<26} {:>12} {:>8.2}x",
            "zstd -19 whole pack",
            z,
            ratio_of(z)
        );
    }
    println!(
        "skeleton chunk-cost decomposition: {chunk_cost} B via {chunk_family}; skeleton literal {skeleton_len} B (entropy-coded at {:.2} bits/byte)",
        chunk_cost as f64 * 8.0 / skeleton.len().max(1) as f64,
        skeleton_len = skeleton.len(),
    );
    println!(
        "diverse control: skeleton {div_skeleton_len} B chunk-coded {div_chunk_cost} B ({div_chunk_family}), EC total {div_ec_total} B vs EntropyFS fg {div_efs_fg} B, zstd {:?}",
        div_zstd,
        div_skeleton_len = div_skeleton.len(),
    );

    // ---- The verdict rule (the same gate as 12D-0, now with the
    // entropy-coded grammar) ----
    let zstd_beats = zstd_w.map(|z| z < grammar_ec_total).unwrap_or(false);
    let grammar_wins = grammar_ec_total < efs_fg
        && grammar_ec_total < efs_settled
        && zstd_w.map(|z| grammar_ec_total < z).unwrap_or(true)
        && grammar_ec_total < logical;
    let diverse_loses = div_ec_total >= div_efs_fg;
    println!("\n-- 12D-1 verdict --");
    if grammar_wins && diverse_loses {
        println!(
            "ADOPT-FOR-INVESTIGATION: the entropy-coded grammar ({grammar_ec_total} B, {:.1}x) beats EVERY incumbent (EntropyFS settled {efs_settled} B, zstd-whole {} B) on the grammar-friendly corpus and loses on the diverse control — the format-bit investigation is justified.",
            ratio_of(grammar_ec_total),
            zstd_w.map(|z| z.to_string()).unwrap_or_else(|| "na".into())
        );
    } else if grammar_wins && !diverse_loses {
        println!(
            "CONDITIONAL: the entropy-coded grammar wins on the config corpus but also 'wins' on the diverse control (likely an induction artifact — investigate before any format work)."
        );
    } else if zstd_beats {
        let z = zstd_w.unwrap_or(0);
        println!(
            "STOP per the brief's gate: the entropy-coded grammar ({grammar_ec_total} B) beats EntropyFS settled ({efs_settled} B) and the raw-skeleton 12D-0 grammar ({grammar_raw_total} B) but NOT every incumbent — zstd-whole ({z} B) remains {:.1}x smaller. The skeleton entropy-codes to {:.2} bits/byte (via {chunk_family}); zstd's context modeling on the LCG-text skeleton still wins. The format-bit investigation is NOT justified on this evidence.",
            grammar_ec_total as f64 / z.max(1) as f64,
            chunk_cost as f64 * 8.0 / skeleton.len().max(1) as f64
        );
    } else {
        println!(
            "STOP: the entropy-coded grammar does not beat the incumbents ({grammar_ec_total} B vs EntropyFS settled {efs_settled} B) — the format-bit investigation is NOT justified (the brief's 'if it loses, stop')."
        );
    }

    // ---- TSV ----
    let mut tsv = String::new();
    tsv.push_str("mode\tcorpus\tlogical\tgrammar_raw\tgrammar_ec\tgrammar_ec_ratio\tchunk_family\tskeleton_bits_byte\tefs_fg\tefs_settled\tzstd_whole\tverdict\n");
    let stamp = std::env::var("GRAMMAR_ORACLE_MODE").unwrap_or_else(|_| "unknown".into());
    tsv.push_str(&format!(
        "{stamp}\tgenerated-config\t{logical}\t{grammar_raw_total}\t{grammar_ec_total}\t{:.2}\t{chunk_family}\t{:.2}\t{efs_fg}\t{efs_settled}\t{}\t{}\n",
        ratio_of(grammar_ec_total),
        chunk_cost as f64 * 8.0 / skeleton.len().max(1) as f64,
        zstd_w.map(|z| z.to_string()).unwrap_or_else(|| "na".into()),
        if grammar_wins { "adopt-for-investigation" } else { "stop" }
    ));
    tsv.push_str(&format!(
        "{stamp}\tdiverse\t{div_logical}\t{}\t{div_ec_total}\t{:.2}\t{div_chunk_family}\t{:.2}\t{div_efs_fg}\t{div_efs_settled}\t{}\t{}\n",
        div_grammar.total_bytes(),
        div_logical as f64 / div_ec_total.max(1) as f64,
        div_chunk_cost as f64 * 8.0 / div_skeleton.len().max(1) as f64,
        div_zstd.map(|z| z.to_string()).unwrap_or_else(|| "na".into()),
        if diverse_loses { "loses-as-expected" } else { "wins-unexpectedly" }
    ));
    if let Ok(path) = std::env::var("GRAMMAR_ORACLE_OUT") {
        if let Some(parent) = std::path::Path::new(&path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&path, &tsv).expect("write oracle summary");
        println!("oracle summary written to {path}");
    }
}
