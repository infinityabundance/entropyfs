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
