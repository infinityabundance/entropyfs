//! Persistent copy-on-write B-tree over content-addressed immutable nodes
//! (ADR-0007, `docs/format/ondisk-v1.md` §4).
//!
//! Nodes are immutable objects: mutation rewrites the path from root to the
//! touched leaf, producing new nodes; unchanged nodes are shared. Node
//! content id = BLAKE3(payload). Keys are raw bytes compared
//! lexicographically (filenames are never assumed UTF-8).
//!
//! v1 tradeoff (documented): delete collapses empty nodes but leaves
//! under-full nodes in place; fsck and GC compaction tolerate and repair
//! this. Order (fanout) is a format parameter.

#![forbid(unsafe_code)]

use std::cmp::Ordering;

use crate::core::extent::ChunkId;
use crate::format::codec::{CodecError, Reader, Writer};

/// Node kinds.
pub const NODE_LEAF: u8 = 0x01;
/// Internal node kind.
pub const NODE_INTERNAL: u8 = 0x02;
/// Default fanout (max entries per node).
pub const DEFAULT_ORDER: u16 = 64;

/// A B-tree entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Key bytes (length-prefixed in the codec).
    pub key: Vec<u8>,
    /// Value bytes (leaf) or child node id (internal).
    pub value: Vec<u8>,
}

/// A key/value pair returned by scans and predecessor lookups.
pub type EntryPair = (Vec<u8>, Vec<u8>);

/// Scan output: entries, whether more remain, and the last key returned.
pub type ScanOut = (Vec<EntryPair>, bool, Option<Vec<u8>>);

/// Split promotion: (separator key, right child id).
pub type Promotion = Option<(Vec<u8>, ChunkId)>;

/// Remove result: (new node id, or `None` when the node collapsed, and the
/// removed value).
pub type RemoveResult = (Option<ChunkId>, Option<Vec<u8>>);

/// A decoded node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    /// Leaf: entries only.
    Leaf {
        /// Entries in key order.
        entries: Vec<Entry>,
    },
    /// Internal: first child (keys < entries[0].key) plus (key, child)
    /// pairs; the last child covers keys >= last key.
    Internal {
        /// Child covering keys strictly below `entries[0].key`.
        first_child: ChunkId,
        /// (separator key, child id) pairs.
        entries: Vec<Entry>,
    },
}

impl Node {
    /// Node kind tag.
    pub fn kind_tag(&self) -> u8 {
        match self {
            Node::Leaf { .. } => NODE_LEAF,
            Node::Internal { .. } => NODE_INTERNAL,
        }
    }

    /// Entry count.
    pub fn len(&self) -> usize {
        match self {
            Node::Leaf { entries } => entries.len(),
            Node::Internal { entries, .. } => entries.len(),
        }
    }

    /// Whether empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Encode to payload bytes.
    pub fn encode(&self, order: u16) -> Vec<u8> {
        let mut w = Writer::new();
        w.u8(self.kind_tag());
        w.u16(order);
        w.u16(self.len() as u16);
        match self {
            Node::Leaf { entries } => {
                for e in entries {
                    // key: u16 length prefix + bytes
                    w.bytes16(&e.key).expect("key fits u16");
                    // value: u32 length prefix + bytes
                    w.bytes32(&e.value).expect("value fits u32");
                }
            }
            Node::Internal {
                first_child,
                entries,
            } => {
                w.bytes(first_child.as_bytes());
                for e in entries {
                    w.bytes16(&e.key).expect("key fits u16");
                    w.bytes(&e.value);
                }
            }
        }
        w.into_bytes()
    }

    /// Decode a node payload.
    ///
    /// `expected_order` must match the order the node was encoded with;
    /// a mismatch means the node was written by an inconsistent tree
    /// configuration and is treated as corrupt (fsck-grade invariant).
    pub fn decode(bytes: &[u8], expected_order: u16, max_fanout: u32) -> Result<Node, CodecError> {
        let mut r = Reader::new(bytes);
        let kind = r.u8()?;
        let encoded_order = r.u16()?;
        if encoded_order != expected_order {
            return Err(CodecError::Malformed);
        }
        let count = r.u16()? as usize;
        if count > max_fanout as usize {
            return Err(CodecError::TooLong);
        }
        match kind {
            NODE_LEAF => {
                let mut entries = Vec::with_capacity(count);
                for _ in 0..count {
                    let key = r.bytes16()?.to_vec();
                    let value = r.bytes32()?.to_vec();
                    entries.push(Entry { key, value });
                }
                // Keys must be strictly increasing.
                for w in entries.windows(2) {
                    if w[0].key >= w[1].key {
                        return Err(CodecError::Malformed);
                    }
                }
                if !r.done() {
                    return Err(CodecError::Malformed);
                }
                Ok(Node::Leaf { entries })
            }
            NODE_INTERNAL => {
                let first_child = ChunkId::new(r.take(32)?.try_into().unwrap());
                let mut entries = Vec::with_capacity(count);
                for _ in 0..count {
                    let key = r.bytes16()?.to_vec();
                    let child = ChunkId::new(r.take(32)?.try_into().unwrap());
                    entries.push(Entry {
                        key,
                        value: child.as_bytes().to_vec(),
                    });
                }
                for w in entries.windows(2) {
                    if w[0].key >= w[1].key {
                        return Err(CodecError::Malformed);
                    }
                }
                if !r.done() {
                    return Err(CodecError::Malformed);
                }
                Ok(Node::Internal {
                    first_child,
                    entries,
                })
            }
            _ => Err(CodecError::Malformed),
        }
    }

    /// Content id of the node.
    pub fn id(&self, order: u16) -> ChunkId {
        ChunkId::of(&self.encode(order))
    }
}

/// Object provider: fetches node payloads (committed or pending) and
/// registers new node payloads. Implemented by the transaction.
pub trait ObjectProvider {
    /// Fetch a node payload by content id.
    fn get(&self, id: &ChunkId) -> Result<Option<Vec<u8>>, BTreeError>;
    /// Register a new immutable node payload.
    fn put(&mut self, id: ChunkId, bytes: Vec<u8>);
}

/// B-tree errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BTreeError {
    /// Provider fetch failed.
    Provider(String),
    /// Corrupt node payload.
    Corrupt(String),
    /// Key too long for the format.
    KeyTooLong,
    /// Value too large.
    ValueTooLarge,
    /// Internal invariant violation.
    Invariant(String),
}

impl std::fmt::Display for BTreeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for BTreeError {}

impl From<CodecError> for BTreeError {
    fn from(e: CodecError) -> Self {
        BTreeError::Corrupt(e.to_string())
    }
}

/// Look up a key; returns the value bytes.
pub fn get<P: ObjectProvider>(
    root: ChunkId,
    key: &[u8],
    order: u16,
    max_fanout: u32,
    provider: &P,
) -> Result<Option<Vec<u8>>, BTreeError> {
    if root.is_zero() {
        // Empty tree: nothing to find.
        return Ok(None);
    }
    let mut node_id = root;
    let mut depth = 0u32;
    loop {
        if depth > 128 {
            return Err(BTreeError::Invariant("tree depth exceeded".into()));
        }
        depth += 1;
        let node = fetch(node_id, order, max_fanout, provider)?;
        match node {
            Node::Leaf { entries } => {
                return Ok(entries
                    .binary_search_by(|e| e.key.as_slice().cmp(key))
                    .ok()
                    .map(|i| entries[i].value.clone()));
            }
            Node::Internal {
                first_child,
                entries,
            } => {
                node_id = match entries.binary_search_by(|e| e.key.as_slice().cmp(key)) {
                    Ok(i) => child_id(&entries[i]),
                    Err(0) => first_child,
                    Err(i) => child_id(&entries[i - 1]),
                };
            }
        }
    }
}

/// The largest key strictly less than `key` (predecessor), with its value.
pub fn predecessor<P: ObjectProvider>(
    root: ChunkId,
    key: &[u8],
    order: u16,
    max_fanout: u32,
    provider: &P,
) -> Result<Option<EntryPair>, BTreeError> {
    if root.is_zero() {
        return Ok(None);
    }
    let mut node_id = root;
    let mut candidate: Option<EntryPair> = None;
    let mut depth = 0u32;
    loop {
        if depth > 128 {
            return Err(BTreeError::Invariant("tree depth exceeded".into()));
        }
        depth += 1;
        let node = fetch(node_id, order, max_fanout, provider)?;
        match node {
            Node::Leaf { entries } => {
                match entries.binary_search_by(|e| e.key.as_slice().cmp(key)) {
                    Ok(_) | Err(0) => {
                        // No key strictly less than `key` in this leaf.
                        return Ok(candidate);
                    }
                    Err(i) => {
                        let e = &entries[i - 1];
                        return Ok(Some((e.key.clone(), e.value.clone())));
                    }
                }
            }
            Node::Internal {
                first_child,
                entries,
            } => {
                match entries.binary_search_by(|e| e.key.as_slice().cmp(key)) {
                    Ok(i) => {
                        // key == entries[i].key: it lives in child_i's
                        // range; the separator itself is not < key.
                        node_id = child_id(&entries[i]);
                    }
                    Err(0) => {
                        // key < entries[0].key: descend first_child.
                        node_id = first_child;
                    }
                    Err(i) => {
                        // entries[i-1].key < key: remember it and descend
                        // into child_{i-1}.
                        let e = &entries[i - 1];
                        candidate = Some((e.key.clone(), e.value.clone()));
                        node_id = child_id(e);
                    }
                }
            }
        }
    }
}

/// Insert (or replace) a key; returns the new root id.
pub fn insert<P: ObjectProvider>(
    root: ChunkId,
    key: &[u8],
    value: &[u8],
    order: u16,
    max_fanout: u32,
    provider: &mut P,
) -> Result<ChunkId, BTreeError> {
    if key.len() > u16::MAX as usize {
        return Err(BTreeError::KeyTooLong);
    }
    if value.len() > u32::MAX as usize {
        return Err(BTreeError::ValueTooLarge);
    }
    if root.is_zero() {
        // Empty tree: a single leaf node becomes the root.
        let leaf = Node::Leaf {
            entries: vec![Entry {
                key: key.to_vec(),
                value: value.to_vec(),
            }],
        };
        let id = leaf.id(order);
        provider.put(id, leaf.encode(order));
        return Ok(id);
    }
    let (new_node, promote) = insert_rec(root, key, value, order, max_fanout, provider, 0)?;
    match promote {
        None => Ok(new_node),
        Some((pkey, right)) => {
            // New root: covers [.., pkey) via new_node (the split left),
            // [pkey, ..) via right.
            let new_root = Node::Internal {
                first_child: new_node,
                entries: vec![Entry {
                    key: pkey,
                    value: right.as_bytes().to_vec(),
                }],
            };
            let id = new_root.id(order);
            provider.put(id, new_root.encode(order));
            Ok(id)
        }
    }
}

/// Remove a key; returns the new root id (equal to `root` if unchanged).
pub fn remove<P: ObjectProvider>(
    root: ChunkId,
    key: &[u8],
    order: u16,
    max_fanout: u32,
    provider: &mut P,
) -> Result<ChunkId, BTreeError> {
    if root.is_zero() {
        // Empty tree: key absent; the empty root is unchanged.
        return Ok(ChunkId::ZERO);
    }
    match remove_rec(root, key, order, max_fanout, provider, 0)? {
        (Some(new_root), _) => Ok(new_root),
        (None, _) => {
            // The root collapsed to empty: an empty tree is a zero id.
            Ok(ChunkId::ZERO)
        }
    }
}

/// Scan keys in `[start, end)` (inclusive lower, exclusive upper);
/// `None` bounds are open. Returns up to `limit` entries in key order,
/// plus whether more remain and the last key returned.
pub fn scan<P: ObjectProvider>(
    root: ChunkId,
    start: Option<&[u8]>,
    end: Option<&[u8]>,
    limit: usize,
    order: u16,
    max_fanout: u32,
    provider: &P,
) -> Result<ScanOut, BTreeError> {
    if root.is_zero() {
        return Ok((Vec::new(), false, None));
    }
    let params = ScanParams {
        order,
        max_fanout,
        provider,
        limit,
    };
    let mut state = ScanState {
        out: Vec::new(),
        has_more: false,
        last_key: None,
    };
    scan_rec(root, start, end, 0, &params, &mut state)?;
    Ok((state.out, state.has_more, state.last_key))
}

/// Full scan (no limit).
pub fn scan_all<P: ObjectProvider>(
    root: ChunkId,
    order: u16,
    max_fanout: u32,
    provider: &P,
) -> Result<Vec<EntryPair>, BTreeError> {
    let (entries, _, _) = scan(root, None, None, usize::MAX, order, max_fanout, provider)?;
    Ok(entries)
}

/// Bulk-build a B-tree from SORTED entries (Phase-9H), staging each final
/// node exactly once.
///
/// The COW `insert` path stages every intermediate path version on disk;
/// rebuilding a large tree by repeated inserts therefore physically writes
/// (and indexes) far more nodes than the final tree contains — the
/// compaction-pass equivalent of write amplification. This loader builds
/// the tree bottom-up from sorted entries: leaves hold up to `order`
/// entries, internal nodes hold up to `order` separator entries (order+1
/// children), separators are the leftmost key of each right child, and
/// every node is final — nothing is staged that the final root does not
/// reference. Empty input yields a ZERO root (empty tree).
///
/// The result satisfies the same node-shape invariants the COW path
/// produces (≤ `order` entries per node, ≤ `order + 1` children), so
/// later inserts/removes operate on it normally.
pub fn bulk_load<P: ObjectProvider>(
    entries: &[(Vec<u8>, Vec<u8>)],
    order: u16,
    max_fanout: u32,
    provider: &mut P,
) -> Result<ChunkId, BTreeError> {
    if entries.is_empty() {
        return Ok(ChunkId::ZERO);
    }
    if order == 0 {
        return Err(BTreeError::Invariant("zero order".into()));
    }
    if order as u32 > max_fanout {
        return Err(BTreeError::Invariant("order exceeds max fanout".into()));
    }
    // Level 0: leaves (max `order` entries each; the last may be short).
    // (node id, leftmost key of the subtree)
    let mut level: Vec<(ChunkId, Vec<u8>)> = Vec::new();
    for chunk in entries.chunks(order as usize) {
        let node = Node::Leaf {
            entries: chunk
                .iter()
                .map(|(k, v)| Entry {
                    key: k.clone(),
                    value: v.clone(),
                })
                .collect(),
        };
        let id = node.id(order);
        let leftmost = chunk[0].0.clone();
        provider.put(id, node.encode(order));
        level.push((id, leftmost));
    }
    // Higher levels: group children into internal nodes of `order`
    // separator entries (order + 1 children); the leftmost key of a child
    // is the leftmost key of its first_child subtree (propagated).
    let children_per_node = order as usize + 1;
    while level.len() > 1 {
        let mut next: Vec<(ChunkId, Vec<u8>)> = Vec::new();
        for chunk in level.chunks(children_per_node) {
            let first_child = chunk[0].0;
            let entries: Vec<Entry> = chunk[1..]
                .iter()
                .map(|(id, leftmost)| Entry {
                    key: leftmost.clone(),
                    value: id.as_bytes().to_vec(),
                })
                .collect();
            let node = Node::Internal {
                first_child,
                entries,
            };
            let id = node.id(order);
            let leftmost = chunk[0].1.clone();
            provider.put(id, node.encode(order));
            next.push((id, leftmost));
        }
        level = next;
    }
    Ok(level[0].0)
}

/// Count entries (for fsck/stat).
pub fn count<P: ObjectProvider>(
    root: ChunkId,
    order: u16,
    max_fanout: u32,
    provider: &P,
) -> Result<u64, BTreeError> {
    let entries = scan_all(root, order, max_fanout, provider)?;
    Ok(entries.len() as u64)
}

/// Bulk COW patch (Phase-10D metadata writeback epoch): apply a SORTED
/// batch of `(key, Option<value>)` operations — `Some` = upsert,
/// `None` = remove — to an existing tree in one pass, rewriting each
/// affected LEAF once and each affected ANCESTOR once. Unchanged subtrees
/// retain their content ids, so a sparse patch over a large tree touches
/// only the paths that actually change (the `bulk_load` alternative would
/// rebuild the whole tree). Returns the new root.
///
/// The batch must be sorted by key with no duplicates (the caller
/// canonicalizes; a violation is an invariant error, never silent).
pub fn apply_sorted_batch<P: ObjectProvider>(
    root: ChunkId,
    batch: &[(Vec<u8>, Option<Vec<u8>>)],
    order: u16,
    max_fanout: u32,
    provider: &mut P,
) -> Result<ChunkId, BTreeError> {
    if batch.is_empty() {
        return Ok(root);
    }
    if order == 0 {
        return Err(BTreeError::Invariant("zero order".into()));
    }
    // Strictly increasing keys, no duplicates.
    for w in batch.windows(2) {
        if w[0].0 >= w[1].0 {
            return Err(BTreeError::Invariant(
                "apply_sorted_batch: batch not sorted / duplicate keys".into(),
            ));
        }
    }
    if root.is_zero() {
        // Empty tree: bulk-load the batch's inserts.
        let entries: Vec<(Vec<u8>, Vec<u8>)> = batch
            .iter()
            .filter_map(|(k, v)| v.clone().map(|v| (k.clone(), v)))
            .collect();
        return bulk_load(&entries, order, max_fanout, provider);
    }
    match patch_rec(root, batch, 0, order, max_fanout, provider)? {
        (Some(id), None) => Ok(id),
        (None, _) => Ok(ChunkId::ZERO), // the whole tree collapsed to empty
        (Some(left), Some((pkey, right))) => {
            // The root split: a new root covers [.., pkey) via left and
            // [pkey, ..) via right.
            let new_root = Node::Internal {
                first_child: left,
                entries: vec![Entry {
                    key: pkey,
                    value: right.as_bytes().to_vec(),
                }],
            };
            let id = new_root.id(order);
            provider.put(id, new_root.encode(order));
            Ok(id)
        }
    }
}

/// Patch result: (new subtree root, or `None` when the subtree collapsed
/// to empty; optional split promotion, exactly like `insert_rec`). An
/// unchanged subtree returns its own id and no promotion.
type PatchOut = (Option<ChunkId>, Promotion);

/// Returns the new subtree root (or `None` when the subtree collapsed to
/// empty). An unchanged subtree returns its own id.
fn patch_rec<P: ObjectProvider>(
    node_id: ChunkId,
    batch: &[(Vec<u8>, Option<Vec<u8>>)],
    depth: u32,
    order: u16,
    max_fanout: u32,
    provider: &mut P,
) -> Result<PatchOut, BTreeError> {
    if depth > 128 {
        return Err(BTreeError::Invariant("tree depth exceeded".into()));
    }
    // Phase-10F: an empty batch leaves this subtree untouched — return its
    // id WITHOUT fetching the node. Without this, a tiny batch (the epoch
    // checkpoint applies 1-2 entries per commit) still fetched (and the
    // internal loop recursed into) EVERY node of the tree: O(tree) per
    // apply, the dominant write-path floor.
    if batch.is_empty() {
        return Ok((Some(node_id), None));
    }
    let node = fetch(node_id, order, max_fanout, provider)?;
    match node {
        Node::Leaf { entries } => {
            // Merge the batch into the leaf: a sorted walk over both lists.
            let mut out: Vec<Entry> = Vec::with_capacity(entries.len() + batch.len());
            let mut ei = 0usize;
            let mut bi = 0usize;
            let mut changed = false;
            while ei < entries.len() || bi < batch.len() {
                let take_existing = match (entries.get(ei), batch.get(bi)) {
                    (Some(e), Some((k, _))) => e.key.as_slice() < k.as_slice(),
                    (Some(_), None) => true,
                    (None, Some(_)) => false,
                    (None, None) => unreachable!(),
                };
                if take_existing {
                    out.push(entries[ei].clone());
                    ei += 1;
                    continue;
                }
                let (k, v) = &batch[bi];
                bi += 1;
                match v {
                    Some(value) => {
                        // Upsert: replace an existing entry or insert.
                        if entries.get(ei).map(|e| e.key.as_slice() == k.as_slice()) == Some(true) {
                            if entries[ei].value.as_slice() != value.as_slice() {
                                changed = true;
                                out.push(Entry {
                                    key: k.clone(),
                                    value: value.clone(),
                                });
                            } else {
                                out.push(entries[ei].clone());
                            }
                            ei += 1;
                        } else {
                            changed = true;
                            out.push(Entry {
                                key: k.clone(),
                                value: value.clone(),
                            });
                        }
                    }
                    None => {
                        // Remove: drop the existing entry if present
                        // (removing an absent key is a no-op).
                        if entries.get(ei).map(|e| e.key.as_slice() == k.as_slice()) == Some(true) {
                            changed = true;
                            ei += 1;
                        }
                    }
                }
            }
            if !changed {
                return Ok((Some(node_id), None));
            }
            if out.is_empty() {
                return Ok((None, None));
            }
            if out.len() as u16 > order {
                // Over-full: split like `insert_rec` (left gets the first
                // half, the promoted median starts the right half).
                let mid = out.len() / 2;
                let right_entries = out.split_off(mid);
                let median_key = right_entries[0].key.clone();
                let left = Node::Leaf { entries: out };
                let right = Node::Leaf {
                    entries: right_entries,
                };
                let left_id = left.id(order);
                let right_id = right.id(order);
                provider.put(left_id, left.encode(order));
                provider.put(right_id, right.encode(order));
                return Ok((Some(left_id), Some((median_key, right_id))));
            }
            let n = Node::Leaf { entries: out };
            let id = n.id(order);
            provider.put(id, n.encode(order));
            Ok((Some(id), None))
        }
        Node::Internal {
            first_child,
            entries,
        } => {
            // Partition the batch across the children and recurse. Child
            // ranges (batch keys are sorted):
            //   first_child: keys < entries[0].key
            //   child i:     keys in [entries[i].key, entries[i+1].key)
            //   last child:  keys >= entries[last].key
            let batch_keys: Vec<&[u8]> = batch.iter().map(|(k, _)| k.as_slice()).collect();
            // Ordered child slots: (separator, child); the first slot's
            // separator is None (the first_child). A child's split
            // promotion inserts an extra slot right after it.
            let mut slots: Vec<(Option<Vec<u8>>, ChunkId)> = Vec::with_capacity(entries.len() + 1);
            slots.push((None, first_child));
            for e in &entries {
                slots.push((Some(e.key.clone()), child_id(e)));
            }
            let mut changed = false;
            let mut lo = 0usize;
            let mut out_slots: Vec<(Option<Vec<u8>>, Option<ChunkId>)> =
                Vec::with_capacity(slots.len());
            for (si, (sep, child)) in slots.iter().enumerate() {
                // Range of this child's keys in the batch.
                let hi = if si == 0 {
                    match entries.first() {
                        Some(f) => {
                            partition_point(&batch_keys[lo..], |k| k < f.key.as_slice()) + lo
                        }
                        None => batch.len(),
                    }
                } else if si < slots.len() - 1 {
                    let cur = sep.as_ref().unwrap().as_slice();
                    let next = slots[si + 1].0.as_ref().unwrap().as_slice();
                    partition_point(&batch_keys[lo..], |k| k >= cur && k < next) + lo
                } else {
                    let cur = sep.as_ref().unwrap().as_slice();
                    partition_point(&batch_keys[lo..], |k| k >= cur) + lo
                };
                let slice = &batch[lo..hi];
                lo = hi;
                if slice.is_empty() {
                    // Phase-10F: no batch keys fall in this child's range;
                    // the child is unchanged — no fetch, no recursion.
                    out_slots.push((sep.clone(), Some(*child)));
                    continue;
                }
                let (new_child, promote) =
                    patch_rec(*child, slice, depth + 1, order, max_fanout, provider)?;
                changed = changed || new_child != Some(*child);
                out_slots.push((sep.clone(), new_child));
                if let Some((pkey, right)) = promote {
                    // The child split: the promoted right half becomes a
                    // new separator entry after this child's separator.
                    out_slots.push((Some(pkey), Some(right)));
                    changed = true;
                }
            }
            if !changed {
                return Ok((Some(node_id), None));
            }
            // Compact: drop collapsed children. A collapsed first_child
            // promotes the next surviving slot to first_child (its
            // separator becomes redundant); a collapsed entry child drops
            // its separator.
            let mut compacted: Vec<(Option<Vec<u8>>, ChunkId)> =
                Vec::with_capacity(out_slots.len());
            let mut seen_survivor = false;
            for (sep, child) in out_slots {
                if let Some(c) = child {
                    let s = if seen_survivor { sep } else { None };
                    compacted.push((s, c));
                    seen_survivor = true;
                }
                // collapsed child: its separator (if any) drops too
            }
            if compacted.is_empty() {
                return Ok((None, None)); // whole subtree collapsed
            }
            if compacted.len() == 1 {
                // Only one child survives: collapse the level (the parent
                // already has the correct separator for the subtree).
                return Ok((Some(compacted[0].1), None));
            }
            let new_first_child = compacted[0].1;
            let mut new_entries: Vec<Entry> = Vec::with_capacity(compacted.len() - 1);
            for (sep, c) in compacted.into_iter().skip(1) {
                new_entries.push(Entry {
                    key: sep.expect("entry slot has a separator"),
                    value: c.as_bytes().to_vec(),
                });
            }
            if new_entries.len() as u16 > order {
                // Over-full: split like `split_internal`.
                let mid = new_entries.len() / 2;
                let median_key = new_entries[mid].key.clone();
                let right_first = child_id(&new_entries[mid]);
                let right_entries = new_entries[mid + 1..].to_vec();
                let left = Node::Internal {
                    first_child: new_first_child,
                    entries: new_entries[..mid].to_vec(),
                };
                let right = Node::Internal {
                    first_child: right_first,
                    entries: right_entries,
                };
                let left_id = left.id(order);
                let right_id = right.id(order);
                provider.put(left_id, left.encode(order));
                provider.put(right_id, right.encode(order));
                return Ok((Some(left_id), Some((median_key, right_id))));
            }
            let n = Node::Internal {
                first_child: new_first_child,
                entries: new_entries,
            };
            let id = n.id(order);
            provider.put(id, n.encode(order));
            Ok((Some(id), None))
        }
    }
}

/// `partition_point` for a slice of byte slices against a predicate
/// (Rust's slice::partition_point with a closure over borrowed keys).
fn partition_point<F>(slice: &[&[u8]], mut pred: F) -> usize
where
    F: FnMut(&[u8]) -> bool,
{
    let mut lo = 0usize;
    let mut hi = slice.len();
    while lo < hi {
        let mid = (lo + hi) / 2;
        if pred(slice[mid]) {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

fn fetch<P: ObjectProvider>(
    node_id: ChunkId,
    order: u16,
    max_fanout: u32,
    provider: &P,
) -> Result<Node, BTreeError> {
    if node_id.is_zero() {
        return Err(BTreeError::Invariant("zero node id".into()));
    }
    let bytes = provider
        .get(&node_id)?
        .ok_or_else(|| BTreeError::Invariant(format!("missing node {node_id}")))?;
    Node::decode(&bytes, order, max_fanout).map_err(|e| BTreeError::Corrupt(e.to_string()))
}

fn child_id(entry: &Entry) -> ChunkId {
    ChunkId::new(entry.value.as_slice().try_into().expect("32-byte child id"))
}

/// Returns (new node id for the caller's slot, optional promote).
///
/// COW invariant: a node whose content changed gets a NEW content id and
/// returns it so the parent updates its child pointer (ADR-0007). A node
/// whose content is unchanged keeps its id.
fn insert_rec<P: ObjectProvider>(
    node_id: ChunkId,
    key: &[u8],
    value: &[u8],
    order: u16,
    max_fanout: u32,
    provider: &mut P,
    depth: u32,
) -> Result<(ChunkId, Promotion), BTreeError> {
    if depth > 128 {
        return Err(BTreeError::Invariant("tree depth exceeded".into()));
    }
    let node = fetch(node_id, order, max_fanout, provider)?;
    match node {
        Node::Leaf { mut entries } => {
            match entries.binary_search_by(|e| e.key.as_slice().cmp(key)) {
                Ok(i) => entries[i].value = value.to_vec(),
                Err(i) => entries.insert(
                    i,
                    Entry {
                        key: key.to_vec(),
                        value: value.to_vec(),
                    },
                ),
            }
            if entries.len() as u16 > order {
                // Split: left gets the first half, right the second half;
                // the first entry of the right half is the promoted median.
                let mid = entries.len() / 2;
                let right_entries = entries.split_off(mid);
                let median_key = right_entries[0].key.clone();
                let left = Node::Leaf {
                    entries: entries.clone(),
                };
                let right = Node::Leaf {
                    entries: right_entries,
                };
                let left_id = left.id(order);
                let right_id = right.id(order);
                provider.put(left_id, left.encode(order));
                provider.put(right_id, right.encode(order));
                Ok((left_id, Some((median_key, right_id))))
            } else {
                let n = Node::Leaf { entries };
                let id = n.id(order);
                provider.put(id, n.encode(order));
                Ok((id, None))
            }
        }
        Node::Internal {
            first_child,
            mut entries,
        } => {
            let (child_slot_idx, child_current) =
                match entries.binary_search_by(|e| e.key.as_slice().cmp(key)) {
                    Ok(i) => (Some(i), child_id(&entries[i])),
                    Err(0) => (None, first_child),
                    Err(i) => (Some(i - 1), child_id(&entries[i - 1])),
                };
            let (new_child, promote) = insert_rec(
                child_current,
                key,
                value,
                order,
                max_fanout,
                provider,
                depth + 1,
            )?;
            match promote {
                Some((pkey, right)) => {
                    // The child split: its left half occupies the slot,
                    // and (pkey, right) is inserted at the boundary.
                    let new_entry = Entry {
                        key: pkey,
                        value: right.as_bytes().to_vec(),
                    };
                    match child_slot_idx {
                        None => {
                            entries.insert(0, new_entry);
                            if entries.len() as u16 > order {
                                return split_internal(
                                    new_child, entries, order, max_fanout, provider,
                                );
                            }
                            let n = Node::Internal {
                                first_child: new_child,
                                entries,
                            };
                            let id = n.id(order);
                            provider.put(id, n.encode(order));
                            Ok((id, None))
                        }
                        Some(idx) => {
                            entries[idx].value = new_child.as_bytes().to_vec();
                            entries.insert(idx + 1, new_entry);
                            if entries.len() as u16 > order {
                                return split_internal(
                                    first_child,
                                    entries,
                                    order,
                                    max_fanout,
                                    provider,
                                );
                            }
                            let n = Node::Internal {
                                first_child,
                                entries,
                            };
                            let id = n.id(order);
                            provider.put(id, n.encode(order));
                            Ok((id, None))
                        }
                    }
                }
                None => {
                    // Child content changed (new id) without splitting:
                    // update the slot pointer. The node itself changes too,
                    // so it gets a new id.
                    match child_slot_idx {
                        None => {
                            let n = Node::Internal {
                                first_child: new_child,
                                entries,
                            };
                            let id = n.id(order);
                            provider.put(id, n.encode(order));
                            Ok((id, None))
                        }
                        Some(idx) => {
                            entries[idx].value = new_child.as_bytes().to_vec();
                            let n = Node::Internal {
                                first_child,
                                entries,
                            };
                            let id = n.id(order);
                            provider.put(id, n.encode(order));
                            Ok((id, None))
                        }
                    }
                }
            }
        }
    }
}

/// Split an over-full internal node; returns (left_id, promote).
fn split_internal<P: ObjectProvider>(
    first_child: ChunkId,
    entries: Vec<Entry>,
    order: u16,
    max_fanout: u32,
    provider: &mut P,
) -> Result<(ChunkId, Promotion), BTreeError> {
    let _ = max_fanout;
    let mid = entries.len() / 2;
    let median_key = entries[mid].key.clone();
    let right_first = child_id(&entries[mid]);
    let right_entries = entries[mid + 1..].to_vec();
    let left = Node::Internal {
        first_child,
        entries: entries[..mid].to_vec(),
    };
    let right = Node::Internal {
        first_child: right_first,
        entries: right_entries,
    };
    let left_id = left.id(order);
    let right_id = right.id(order);
    provider.put(left_id, left.encode(order));
    provider.put(right_id, right.encode(order));
    Ok((left_id, Some((median_key, right_id))))
}

/// Returns (new node id or None if the node collapsed to empty, removed
/// value). COW invariant: changed nodes return their new content id.
fn remove_rec<P: ObjectProvider>(
    node_id: ChunkId,
    key: &[u8],
    order: u16,
    max_fanout: u32,
    provider: &mut P,
    depth: u32,
) -> Result<RemoveResult, BTreeError> {
    if depth > 128 {
        return Err(BTreeError::Invariant("tree depth exceeded".into()));
    }
    let node = fetch(node_id, order, max_fanout, provider)?;
    match node {
        Node::Leaf { mut entries } => {
            match entries.binary_search_by(|e| e.key.as_slice().cmp(key)) {
                Ok(i) => {
                    let removed = entries.remove(i).value;
                    if entries.is_empty() {
                        // Node collapses; caller removes the separator.
                        return Ok((None, Some(removed)));
                    }
                    let n = Node::Leaf { entries };
                    let id = n.id(order);
                    provider.put(id, n.encode(order));
                    Ok((Some(id), Some(removed)))
                }
                Err(_) => Ok((Some(node_id), None)),
            }
        }
        Node::Internal {
            first_child,
            mut entries,
        } => {
            let (child_slot_idx, child_current) =
                match entries.binary_search_by(|e| e.key.as_slice().cmp(key)) {
                    Ok(i) => (Some(i), child_id(&entries[i])),
                    Err(0) => (None, first_child),
                    Err(i) => (Some(i - 1), child_id(&entries[i - 1])),
                };
            let (child_result, removed) =
                remove_rec(child_current, key, order, max_fanout, provider, depth + 1)?;
            match child_result {
                None => {
                    // Child collapsed to empty: drop the separator entry.
                    match child_slot_idx {
                        None => {
                            if entries.is_empty() {
                                return Err(BTreeError::Invariant(
                                    "first_child collapsed with no entries".into(),
                                ));
                            }
                            let new_first = child_id(&entries[0]);
                            entries.remove(0);
                            if entries.is_empty() {
                                // Collapse this node to its only child.
                                return Ok((Some(new_first), removed));
                            }
                            let n = Node::Internal {
                                first_child: new_first,
                                entries,
                            };
                            let id = n.id(order);
                            provider.put(id, n.encode(order));
                            Ok((Some(id), removed))
                        }
                        Some(idx) => {
                            entries.remove(idx);
                            if entries.is_empty() {
                                // Only first_child remains: collapse.
                                return Ok((Some(first_child), removed));
                            }
                            let n = Node::Internal {
                                first_child,
                                entries,
                            };
                            let id = n.id(order);
                            provider.put(id, n.encode(order));
                            Ok((Some(id), removed))
                        }
                    }
                }
                Some(new_child) => {
                    if new_child == child_current {
                        // Subtree unchanged (key absent); node keeps its id.
                        return Ok((Some(node_id), removed));
                    }
                    // Child changed identity: update the slot pointer.
                    match child_slot_idx {
                        None => {
                            let n = Node::Internal {
                                first_child: new_child,
                                entries,
                            };
                            let id = n.id(order);
                            provider.put(id, n.encode(order));
                            Ok((Some(id), removed))
                        }
                        Some(idx) => {
                            entries[idx].value = new_child.as_bytes().to_vec();
                            let n = Node::Internal {
                                first_child,
                                entries,
                            };
                            let id = n.id(order);
                            provider.put(id, n.encode(order));
                            Ok((Some(id), removed))
                        }
                    }
                }
            }
        }
    }
}

/// Helper: the smaller of two optional bounds (None = open).
fn min_bound<'a>(a: Option<&'a [u8]>, b: Option<&'a [u8]>) -> Option<&'a [u8]> {
    match (a, b) {
        (Some(x), Some(y)) => Some(if x <= y { x } else { y }),
        (Some(x), None) => Some(x),
        (None, Some(y)) => Some(y),
        (None, None) => None,
    }
}

/// Shared scan parameters threaded through the recursion.
struct ScanParams<'a, P: ObjectProvider> {
    order: u16,
    max_fanout: u32,
    provider: &'a P,
    limit: usize,
}

/// Mutable scan output state.
struct ScanState {
    out: Vec<EntryPair>,
    has_more: bool,
    last_key: Option<Vec<u8>>,
}

fn scan_rec<P: ObjectProvider>(
    node_id: ChunkId,
    start: Option<&[u8]>,
    end: Option<&[u8]>,
    depth: u32,
    params: &ScanParams<'_, P>,
    state: &mut ScanState,
) -> Result<(), BTreeError> {
    if depth > 128 {
        return Err(BTreeError::Invariant("tree depth exceeded".into()));
    }
    if state.out.len() >= params.limit {
        state.has_more = true;
        return Ok(());
    }
    if node_id.is_zero() {
        return Ok(());
    }
    let node = fetch(node_id, params.order, params.max_fanout, params.provider)?;
    match node {
        Node::Leaf { entries } => {
            for e in entries {
                // Range filter.
                if let Some(s) = start {
                    if e.key.as_slice() < s {
                        continue;
                    }
                }
                if let Some(en) = end {
                    if e.key.as_slice() >= en {
                        return Ok(());
                    }
                }
                if state.out.len() >= params.limit {
                    state.has_more = true;
                    return Ok(());
                }
                state.last_key = Some(e.key.clone());
                state.out.push((e.key, e.value));
            }
            Ok(())
        }
        Node::Internal {
            first_child,
            entries,
        } => {
            // Recurse into children in key order, respecting bounds.
            // Child covering [.., entries[0].key): its end is the min of
            // entries[0].key and the query end.
            let first_end = min_bound(entries.first().map(|e| e.key.as_slice()), end);
            if !start.is_some_and(|s| entries.first().is_some_and(|e| s >= e.key.as_slice())) {
                scan_rec(first_child, start, first_end, depth + 1, params, state)?;
            }
            for (i, e) in entries.iter().enumerate() {
                if state.out.len() >= params.limit {
                    state.has_more = true;
                    return Ok(());
                }
                let child_start = start.filter(|s| *s >= e.key.as_slice());
                // The i-th entry's child covers [entries[i].key,
                // entries[i+1].key); bound it also by the query end.
                let child_end = min_bound(entries.get(i + 1).map(|n| n.key.as_slice()), end);
                scan_rec(
                    child_id(e),
                    child_start,
                    child_end,
                    depth + 1,
                    params,
                    state,
                )?;
            }
            Ok(())
        }
    }
}

/// Verify tree invariants (fsck): keys strictly increasing, children
/// present, no cycles (depth-bounded).
pub fn verify<P: ObjectProvider>(
    root: ChunkId,
    order: u16,
    max_fanout: u32,
    provider: &P,
) -> Result<u64, BTreeError> {
    if root.is_zero() {
        return Ok(0);
    }
    verify_rec(root, order, max_fanout, provider, 0)
}

fn verify_rec<P: ObjectProvider>(
    node_id: ChunkId,
    order: u16,
    max_fanout: u32,
    provider: &P,
    depth: u32,
) -> Result<u64, BTreeError> {
    if depth > 128 {
        return Err(BTreeError::Invariant("tree depth exceeded".into()));
    }
    let node = fetch(node_id, order, max_fanout, provider)?;
    match node {
        Node::Leaf { entries } => Ok(entries.len() as u64),
        Node::Internal {
            first_child,
            entries,
        } => {
            if first_child.is_zero() {
                return Err(BTreeError::Invariant("zero first child".into()));
            }
            let mut total = verify_rec(first_child, order, max_fanout, provider, depth + 1)?;
            for e in &entries {
                let cid = child_id(e);
                if cid.is_zero() {
                    return Err(BTreeError::Invariant("zero child id".into()));
                }
                total += verify_rec(cid, order, max_fanout, provider, depth + 1)?;
            }
            Ok(total)
        }
    }
}

/// Ordering helper for tests.
pub fn cmp_key(a: &[u8], b: &[u8]) -> Ordering {
    a.cmp(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[derive(Default)]
    struct MemProvider {
        nodes: HashMap<ChunkId, Vec<u8>>,
        puts: usize,
    }

    impl ObjectProvider for MemProvider {
        fn get(&self, id: &ChunkId) -> Result<Option<Vec<u8>>, BTreeError> {
            Ok(self.nodes.get(id).cloned())
        }
        fn put(&mut self, id: ChunkId, bytes: Vec<u8>) {
            self.nodes.insert(id, bytes);
            self.puts += 1;
        }
    }

    fn key(n: u64) -> Vec<u8> {
        n.to_be_bytes().to_vec()
    }

    fn value(n: u64) -> Vec<u8> {
        format!("value-{n}").into_bytes()
    }

    #[test]
    fn insert_get_remove_roundtrip() {
        let mut p = MemProvider::default();
        let order = 8u16;
        let mut root = ChunkId::ZERO;
        let n = 500u64;
        // Insert in random-ish deterministic order.
        for i in 0..n {
            let k = (i * 37 + 11) % n;
            root = insert(root, &key(k), &value(k), order, 4096, &mut p).unwrap();
        }
        // All present.
        for k in 0..n {
            let v = get(root, &key(k), order, 4096, &p).unwrap();
            assert_eq!(v, Some(value(k)));
        }
        // Verify invariants.
        assert_eq!(verify(root, order, 4096, &p).unwrap(), n);
        // Scan full.
        let all = scan_all(root, order, 4096, &p).unwrap();
        assert_eq!(all.len(), n as usize);
        for w in all.windows(2) {
            assert!(w[0].0 < w[1].0);
        }
        // Remove half.
        for k in (0..n).step_by(2) {
            root = remove(root, &key(k), order, 4096, &mut p).unwrap();
        }
        for k in 0..n {
            let v = get(root, &key(k), order, 4096, &p).unwrap();
            if k % 2 == 0 {
                assert_eq!(v, None, "key {k} should be removed");
            } else {
                assert_eq!(v, Some(value(k)));
            }
        }
        assert_eq!(verify(root, order, 4096, &p).unwrap(), n / 2);
        // Remove the rest.
        for k in (1..n).step_by(2) {
            root = remove(root, &key(k), order, 4096, &mut p).unwrap();
        }
        assert_eq!(root, ChunkId::ZERO);
    }

    #[test]
    fn replace_updates_value() {
        let mut p = MemProvider::default();
        let order = 8u16;
        let mut root = ChunkId::ZERO;
        root = insert(root, &key(1), b"a", order, 4096, &mut p).unwrap();
        root = insert(root, &key(1), b"b", order, 4096, &mut p).unwrap();
        assert_eq!(
            get(root, &key(1), order, 4096, &p).unwrap(),
            Some(b"b".to_vec())
        );
        assert_eq!(verify(root, order, 4096, &p).unwrap(), 1);
    }

    #[test]
    fn apply_sorted_batch_matches_sequential_ops() {
        // Build a tree, then apply a mixed sorted batch (upserts +
        // removes) and require the result to match sequential insert/remove
        // exactly — content, invariants, and the final root's content id.
        let order = 8u16;
        let n = 400u64;

        let build = |inserts: &[(u64, u64)]| {
            let mut p = MemProvider::default();
            let mut root = ChunkId::ZERO;
            for &(k, v) in inserts {
                root = insert(root, &key(k), &value(v), order, 4096, &mut p).unwrap();
            }
            (p, root)
        };

        let inserts: Vec<(u64, u64)> = (0..n).map(|k| (k, k)).collect();
        let (mut p, root) = build(&inserts);

        // A mixed batch: upsert every 3rd key with a new value, remove
        // every 7th key, add 20 fresh keys at the high end, remove 10 at
        // the low end (exercises first_child collapse paths). Built as one
        // sorted, duplicate-free pass.
        let mut batch: Vec<(Vec<u8>, Option<Vec<u8>>)> = Vec::new();
        for k in 0..n + 20 {
            let op = if k < 10 {
                Some(None) // low-end removals
            } else if k < n {
                if k % 7 == 0 {
                    Some(None)
                } else if k % 3 == 0 {
                    Some(Some(value(k + 1000)))
                } else {
                    None // untouched
                }
            } else {
                Some(Some(value(k))) // high-end fresh inserts
            };
            if let Some(op) = op {
                batch.push((key(k), op));
            }
        }

        // Reference: sequential ops.
        let (mut refp, mut refroot) = build(&inserts);
        for (k, v) in &batch {
            match v {
                Some(vv) => {
                    refroot = insert(refroot, k, vv, order, 4096, &mut refp).unwrap();
                }
                None => {
                    refroot = remove(refroot, k, order, 4096, &mut refp).unwrap();
                }
            }
        }

        // Batch application.
        let new_root = apply_sorted_batch(root, &batch, order, 4096, &mut p).unwrap();
        // Content must match the sequential result exactly (the tree SHAPE
        // may legitimately differ — sequential inserts split at different
        // moments than one bulk patch — so compare content + invariants,
        // not the root id).
        let expected = scan_all(refroot, order, 4096, &refp).unwrap();
        let got = scan_all(new_root, order, 4096, &p).unwrap();
        assert_eq!(got, expected, "content must match the sequential result");
        assert_eq!(
            verify(new_root, order, 4096, &p).unwrap(),
            expected.len() as u64
        );

        // Idempotence: applying the SAME batch again changes nothing.
        let mut p2 = p;
        let again = apply_sorted_batch(new_root, &batch, order, 4096, &mut p2).unwrap();
        assert_eq!(again, new_root, "re-applying the batch must be a no-op");
    }

    #[test]
    fn apply_sorted_batch_collapses_to_empty() {
        let order = 4u16;
        let mut p = MemProvider::default();
        let mut root = ChunkId::ZERO;
        for k in 0..50u64 {
            root = insert(root, &key(k), &value(k), order, 4096, &mut p).unwrap();
        }
        let batch: Vec<(Vec<u8>, Option<Vec<u8>>)> = (0..50u64).map(|k| (key(k), None)).collect();
        let new_root = apply_sorted_batch(root, &batch, order, 4096, &mut p).unwrap();
        assert_eq!(new_root, ChunkId::ZERO, "all keys removed -> empty tree");
        assert_eq!(scan_all(new_root, order, 4096, &p).unwrap().len(), 0);
    }

    #[test]
    fn apply_sorted_batch_keeps_unchanged_subtrees() {
        // A single-key patch must not rewrite nodes outside its path.
        let order = 8u16;
        let mut p = MemProvider::default();
        let mut root = ChunkId::ZERO;
        for k in 0..500u64 {
            root = insert(root, &key(k), &value(k), order, 4096, &mut p).unwrap();
        }
        let puts_before = p.puts;
        let batch = vec![(key(250), Some(value(9999)))];
        let new_root = apply_sorted_batch(root, &batch, order, 4096, &mut p).unwrap();
        assert_eq!(verify(new_root, order, 4096, &p).unwrap(), 500);
        assert_eq!(
            get(new_root, &key(250), order, 4096, &p).unwrap(),
            Some(value(9999))
        );
        // Only the path nodes (depth + 1 leaf) may be new; far fewer than
        // a full rebuild (500 inserts put 500/8 ~= 60+ leaf + internal
        // nodes).
        let new_puts = p.puts - puts_before;
        assert!(
            new_puts < 8,
            "sparse patch must touch only the affected path (put {new_puts})"
        );
        // The unchanged sibling subtrees keep their ids: the whole-tree
        // scan still resolves through the retained nodes.
        let mut all = scan_all(new_root, order, 4096, &p).unwrap();
        assert_eq!(all.len(), 500);
        let _ = all.pop();
        assert_eq!(
            get(new_root, &key(0), order, 4096, &p).unwrap(),
            Some(value(0))
        );
    }

    #[test]
    fn range_scan() {
        let mut p = MemProvider::default();
        let order = 4u16;
        let mut root = ChunkId::ZERO;
        for i in 0..100u64 {
            root = insert(root, &key(i), &value(i), order, 4096, &mut p).unwrap();
        }
        // Keys in [10, 20)
        let (entries, more, last) = scan(
            root,
            Some(&key(10)),
            Some(&key(20)),
            usize::MAX,
            order,
            4096,
            &p,
        )
        .unwrap();
        assert!(!more);
        assert_eq!(entries.len(), 10);
        assert_eq!(entries[0].0, key(10));
        assert_eq!(last, Some(key(19)));
        // Bounded scan
        let (entries2, more2, _) = scan(root, None, None, 5, order, 4096, &p).unwrap();
        assert_eq!(entries2.len(), 5);
        assert!(more2);
    }

    #[test]
    fn string_keys_lexicographic() {
        let mut p = MemProvider::default();
        let order = 8u16;
        let mut root = ChunkId::ZERO;
        for name in ["zeta", "alpha", "mike", "bravo"] {
            root = insert(root, name.as_bytes(), b"v", order, 4096, &mut p).unwrap();
        }
        let all = scan_all(root, order, 4096, &p).unwrap();
        let names: Vec<String> = all
            .iter()
            .map(|(k, _)| String::from_utf8(k.clone()).unwrap())
            .collect();
        assert_eq!(names, vec!["alpha", "bravo", "mike", "zeta"]);
    }

    #[test]
    fn corrupt_node_typed_error() {
        let mut p = MemProvider::default();
        let order = 8u16;
        let mut root = ChunkId::ZERO;
        root = insert(root, b"k", b"v", order, 4096, &mut p).unwrap();
        // Corrupt the root node payload.
        let bad = p.nodes.get(&root).cloned().unwrap();
        let mut bad = bad;
        bad[0] ^= 0xFF; // node kind byte
        let fake_id = ChunkId::of(&bad);
        p.nodes.insert(fake_id, bad);
        let res = get(fake_id, b"k", order, 4096, &p);
        assert!(res.is_err());
    }
}
