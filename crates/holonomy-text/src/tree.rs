//! The immutable path-copied tree behind [`crate::Spine`].
//!
//! Interior nodes carry three aggregates — bytes, slab count and height — so position math is a
//! descent, not a scan. Weights are byte lengths today; the Eye's layout heights will ride the
//! same aggregate once Block 5 defines them.
//!
//! Every mutation rebuilds only the nodes on one root-to-leaf path and returns a new root; the
//! old root stays valid, which is why undo is a pointer swap and why a sixty-year history costs
//! pointers instead of plaintext (RFC-003 §2). The tree is AVL-balanced so a path is O(log N)
//! even when segments arrive in order — the degenerate linked-list shape an unbalanced rope would
//! reach after a day of typing is impossible by construction.

use std::sync::Arc;

use crate::slab::Slab;

/// A node of the spine. Empty is a real variant so every operation stays total.
pub(crate) enum Node {
    /// No bytes.
    Empty,
    /// One slab reference.
    Leaf(Slab),
    /// A balanced interior node with cached aggregates.
    Branch {
        /// Left subtree.
        left: Arc<Node>,
        /// Right subtree.
        right: Arc<Node>,
        /// Total bytes under this node.
        bytes: u64,
        /// Total slabs under this node.
        slabs: u32,
        /// Tree height in nodes (Empty = 0, Leaf = 1).
        height: u32,
    },
}

impl std::fmt::Debug for Node {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => formatter.write_str("Empty"),
            Self::Leaf(slab) => formatter.debug_tuple("Leaf").field(slab).finish(),
            Self::Branch {
                bytes,
                slabs,
                height,
                ..
            } => formatter
                .debug_struct("Branch")
                .field("bytes", bytes)
                .field("slabs", slabs)
                .field("height", height)
                .finish_non_exhaustive(),
        }
    }
}

impl Node {
    pub(crate) fn bytes(&self) -> u64 {
        match self {
            Self::Empty => 0,
            Self::Leaf(slab) => u64::from(slab.len()),
            Self::Branch { bytes, .. } => *bytes,
        }
    }

    pub(crate) fn slabs(&self) -> u32 {
        match self {
            Self::Empty => 0,
            Self::Leaf(_) => 1,
            Self::Branch { slabs, .. } => *slabs,
        }
    }

    pub(crate) fn height(&self) -> u32 {
        match self {
            Self::Empty => 0,
            Self::Leaf(_) => 1,
            Self::Branch { height, .. } => *height,
        }
    }
}

/// The empty tree.
pub(crate) fn empty() -> Arc<Node> {
    Arc::new(Node::Empty)
}

/// A single slab.
pub(crate) fn leaf(slab: Slab) -> Arc<Node> {
    Arc::new(Node::Leaf(slab))
}

/// The node joining two subtrees, normalizing away empties and recomputing aggregates.
pub(crate) fn branch(left: Arc<Node>, right: Arc<Node>) -> Arc<Node> {
    if matches!(left.as_ref(), Node::Empty) {
        return right;
    }
    if matches!(right.as_ref(), Node::Empty) {
        return left;
    }
    let bytes = left.bytes() + right.bytes();
    let slabs = left.slabs() + right.slabs();
    let height = left.height().max(right.height()) + 1;
    Arc::new(Node::Branch {
        left,
        right,
        bytes,
        slabs,
        height,
    })
}

fn children(node: &Arc<Node>) -> (Arc<Node>, Arc<Node>) {
    match node.as_ref() {
        Node::Branch { left, right, .. } => (Arc::clone(left), Arc::clone(right)),
        // Branch children are never Empty (see `branch`), and callers only ask for children of a
        // node whose height proves it is a Branch.
        Node::Empty | Node::Leaf(_) => unreachable!("a balanced join only descends into branches"),
    }
}

/// Rebalances a seam whose sides differ by at most two, with AVL single/double rotations.
fn balance(left: Arc<Node>, right: Arc<Node>) -> Arc<Node> {
    let (left_height, right_height) = (left.height(), right.height());
    if left_height > right_height + 1 {
        let (outer, inner) = children(&left);
        if outer.height() >= inner.height() {
            branch(outer, branch(inner, right))
        } else {
            let (inner_left, inner_right) = children(&inner);
            branch(branch(outer, inner_left), branch(inner_right, right))
        }
    } else if right_height > left_height + 1 {
        let (inner, outer) = children(&right);
        if outer.height() >= inner.height() {
            branch(branch(left, inner), outer)
        } else {
            let (inner_left, inner_right) = children(&inner);
            branch(branch(left, inner_left), branch(inner_right, outer))
        }
    } else {
        branch(left, right)
    }
}

/// Concatenates two subtrees, path-copying O(|height difference|) nodes.
pub(crate) fn join(left: Arc<Node>, right: Arc<Node>) -> Arc<Node> {
    if matches!(left.as_ref(), Node::Empty) {
        return right;
    }
    if matches!(right.as_ref(), Node::Empty) {
        return left;
    }
    let (left_height, right_height) = (left.height(), right.height());
    if left_height > right_height + 1 {
        let (outer, inner) = children(&left);
        balance(outer, join(inner, right))
    } else if right_height > left_height + 1 {
        let (inner, outer) = children(&right);
        balance(join(left, inner), outer)
    } else {
        branch(left, right)
    }
}

/// Splits a subtree at a byte offset into the bytes before it and the bytes from it on.
///
/// `at` must not exceed the subtree's length. Cutting inside a slab splits that slab into two
/// leaves referencing the same segment, which is legal: a segment's bytes may be spread across
/// several slabs.
pub(crate) fn split(node: &Arc<Node>, at: u64) -> (Arc<Node>, Arc<Node>) {
    match node.as_ref() {
        Node::Empty => (empty(), empty()),
        Node::Leaf(slab) => {
            let len = u64::from(slab.len());
            if at == 0 {
                (empty(), Arc::clone(node))
            } else if at >= len {
                (Arc::clone(node), empty())
            } else {
                let head = at as u32;
                let tail = slab.len() - head;
                (
                    leaf(slab.slice_clamped(0, head)),
                    leaf(slab.slice_clamped(head, tail)),
                )
            }
        }
        Node::Branch { left, right, .. } => {
            let left_bytes = left.bytes();
            if at <= left_bytes {
                let (before, after) = split(left, at);
                (before, join(after, Arc::clone(right)))
            } else {
                let (before, after) = split(right, at - left_bytes);
                (join(Arc::clone(left), before), after)
            }
        }
    }
}

/// Builds a perfectly balanced tree from slabs in document order, in O(n).
pub(crate) fn build_balanced(slabs: &[Slab]) -> Arc<Node> {
    match slabs.len() {
        0 => empty(),
        1 => leaf(slabs[0]),
        count => {
            let middle = count / 2;
            branch(
                build_balanced(&slabs[..middle]),
                build_balanced(&slabs[middle..]),
            )
        }
    }
}

/// Verifies every structural invariant the rest of the crate relies on, returning the
/// `(bytes, slabs, height)` recomputed from the leaves.
///
/// Exists for the property tests: aggregates are cached, so a bug can produce a tree whose
/// lengths look right while its caches, its balance or its emptiness discipline are wrong. The
/// checker recomputes from the bottom up and panics on the first disagreement.
#[cfg(test)]
pub(crate) fn check_invariants(node: &Arc<Node>) -> (u64, u32, u32) {
    match node.as_ref() {
        Node::Empty => (0, 0, 0),
        Node::Leaf(slab) => {
            assert!(!slab.is_empty(), "a leaf must carry bytes");
            (u64::from(slab.len()), 1, 1)
        }
        Node::Branch { left, right, .. } => {
            assert!(
                !matches!(left.as_ref(), Node::Empty),
                "an Empty child must have been normalized away"
            );
            assert!(
                !matches!(right.as_ref(), Node::Empty),
                "an Empty child must have been normalized away"
            );
            let (left_bytes, left_slabs, left_height) = check_invariants(left);
            let (right_bytes, right_slabs, right_height) = check_invariants(right);
            assert!(
                left_height.abs_diff(right_height) <= 1,
                "AVL invariant broken: {left_height} vs {right_height}"
            );
            assert_eq!(
                node.bytes(),
                left_bytes + right_bytes,
                "bytes cache drifted"
            );
            assert_eq!(node.slabs(), left_slabs + right_slabs, "slab cache drifted");
            assert_eq!(
                node.height(),
                left_height.max(right_height) + 1,
                "height cache drifted"
            );
            (
                left_bytes + right_bytes,
                left_slabs + right_slabs,
                left_height.max(right_height) + 1,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::Arc;

    use super::*;
    use crate::slab::{SegmentId, Slab};

    fn slab(segment: u64, len: u32) -> Slab {
        Slab::new(SegmentId::new(segment), 0, len)
    }

    /// Verifies aggregates and the AVL invariant, returning the subtree's slab list.
    fn check(node: &Arc<Node>) -> Vec<Slab> {
        check_invariants(node);
        fn collect(node: &Arc<Node>, out: &mut Vec<Slab>) {
            match node.as_ref() {
                Node::Empty => {}
                Node::Leaf(value) => out.push(*value),
                Node::Branch { left, right, .. } => {
                    collect(left, out);
                    collect(right, out);
                }
            }
        }
        let mut slabs = Vec::new();
        collect(node, &mut slabs);
        slabs
    }

    fn collect_ptrs(node: &Arc<Node>, out: &mut HashSet<*const Node>) {
        out.insert(Arc::as_ptr(node));
        if let Node::Branch { left, right, .. } = node.as_ref() {
            collect_ptrs(left, out);
            collect_ptrs(right, out);
        }
    }

    #[test]
    fn appends_stay_balanced_and_ordered() {
        let mut root = empty();
        for index in 0..1_024u64 {
            root = join(root, leaf(slab(index, 10)));
        }
        let slabs = check(&root);
        assert_eq!(slabs.len(), 1_024);
        // AVL worst case is well under 24; a degenerate list would be 1,024 deep.
        assert!(
            root.height() <= 24,
            "height {} is not logarithmic",
            root.height()
        );
        assert!(slabs
            .iter()
            .enumerate()
            .all(|(index, value)| value.segment() == SegmentId::new(index as u64)));
    }

    #[test]
    fn balanced_construction_is_logarithmic_too() {
        let slabs: Vec<Slab> = (0..916).map(|index| slab(index, 10)).collect();
        let root = build_balanced(&slabs);
        assert_eq!(check(&root).len(), 916);
        assert!(
            root.height() <= 11,
            "height {} is not logarithmic",
            root.height()
        );
    }

    #[test]
    fn splitting_and_rejoining_every_byte_preserves_the_sequence() {
        let slabs: Vec<Slab> = (0..7).map(|index| slab(index, 10)).collect();
        let root = build_balanced(&slabs);
        for at in 0..=70u64 {
            let (left, right) = split(&root, at);
            assert_eq!(left.bytes() + right.bytes(), 70);
            // Rejoining may leave a segment split across two leaves; compare bytes, not leaves.
            let joined = check(&join(left, right));
            let total: u64 = joined.iter().map(|value| u64::from(value.len())).sum();
            assert_eq!(total, 70);
        }
    }

    #[test]
    fn a_split_inside_one_slab_cuts_the_segment_range() {
        let root = leaf(Slab::new(SegmentId::new(5), 100, 10));
        let (left, right) = split(&root, 4);
        match (left.as_ref(), right.as_ref()) {
            (Node::Leaf(head), Node::Leaf(tail)) => {
                assert_eq!((head.start(), head.len()), (100, 4));
                assert_eq!((tail.start(), tail.len()), (104, 6));
                assert_eq!(head.segment(), tail.segment());
            }
            other => panic!("expected two leaves, got {other:?}"),
        }
    }

    #[test]
    fn appending_shares_every_unaffected_subtree() {
        let slabs: Vec<Slab> = (0..1_024).map(|index| slab(index, 10)).collect();
        let before = build_balanced(&slabs);
        let after = join(Arc::clone(&before), leaf(slab(1_024, 10)));

        let mut old_ptrs = HashSet::new();
        collect_ptrs(&before, &mut old_ptrs);
        let mut new_ptrs = HashSet::new();
        collect_ptrs(&after, &mut new_ptrs);

        let shared = old_ptrs.intersection(&new_ptrs).count();
        let allocated = new_ptrs.len() - shared;
        assert!(
            allocated <= 2 * before.height() as usize + 2,
            "append allocated {allocated} nodes for a height-{} tree",
            before.height()
        );
        assert!(
            shared >= old_ptrs.len() - 2 * before.height() as usize,
            "append copied more than its path: {shared} of {} shared",
            old_ptrs.len()
        );
    }

    #[test]
    fn random_join_and_split_shapes_keep_every_invariant() {
        use std::collections::BTreeSet;

        use crate::test_support::XorShift;

        for seed in [1u64, 7, 42, 0xDEAD_BEEF] {
            let mut rng = XorShift::new(seed);
            let mut root = empty();
            let mut expected: Vec<Slab> = Vec::new();
            // Where every leaf starts, tracked independently of the tree. A cut spends a leaf
            // exactly when it lands inside one — that is, when its offset is not already here —
            // and joins never merge fragments back (a segment's bytes may span several slabs).
            let mut boundaries: BTreeSet<u64> = BTreeSet::new();

            for _ in 0..400 {
                match rng.below(3) {
                    0 => {
                        // Append.
                        let next = slab(rng.below(64), 1 + rng.below(24) as u32);
                        boundaries.insert(root.bytes());
                        root = join(root, leaf(next));
                        expected.push(next);
                    }
                    1 => {
                        // Split at a random offset and rejoin: shape churn without content change.
                        let at = rng.below(root.bytes() + 1);
                        if at > 0 && at < root.bytes() {
                            boundaries.insert(at);
                        }
                        let (left, right) = split(&root, at);
                        check_invariants(&left);
                        check_invariants(&right);
                        root = join(left, right);
                    }
                    _ => {
                        // Concatenate an independently built tree onto the end.
                        let count = 1 + rng.below(5) as usize;
                        let extra: Vec<Slab> = (0..count)
                            .map(|_| slab(rng.below(64), 1 + rng.below(16) as u32))
                            .collect();
                        let mut offset = root.bytes();
                        for value in &extra {
                            boundaries.insert(offset);
                            offset += u64::from(value.len());
                        }
                        root = join(root, build_balanced(&extra));
                        expected.extend(extra);
                    }
                }
                let (bytes, slabs, _) = check_invariants(&root);
                let expected_bytes: u64 = expected.iter().map(|value| u64::from(value.len())).sum();
                assert_eq!(bytes, expected_bytes, "seed {seed}");
                assert_eq!(
                    u64::from(slabs),
                    boundaries.len() as u64,
                    "seed {seed}: a leaf was gained or lost"
                );
            }
        }
    }

    #[test]
    fn deep_sequences_never_degenerate() {
        // 4,096 appends without balancing would be a 4,096-deep list.
        let mut root = empty();
        for index in 0..4_096u64 {
            root = join(root, leaf(slab(index, 10)));
        }
        // AVL worst case for 4,096 leaves is 18; degeneracy would be 4,096.
        assert!(
            root.height() <= 24,
            "height {} is not logarithmic",
            root.height()
        );
        assert_eq!(check(&root).len(), 4_096);
    }
}
