//! The iteration order of a `java.util.HashMap<Integer, …>` built with the default constructor and
//! filled by `put` with distinct keys, as kotlinc's `StringSwitch` builds its hash buckets.
//!
//! This is a port of OpenJDK's `HashMap.putVal`, `resize`, `treeifyBin`, and the `TreeNode` bin
//! operations that change the `next` chain an iterator walks: `treeify`, `putTreeVal`,
//! `balanceInsertion` with its rotations, `moveRootToFront`, `split`, and `untreeify`. Keys are
//! never removed, so nothing else is modeled.
//!
//! An `Integer` key hashes to its value, and `hash(key) = h ^ (h >>> 16)` is a bijection on 32-bit
//! values, so distinct keys always have distinct spread hashes. The tree descent therefore always
//! decides by the (signed) spread hash and never reaches `compareComparables` or `tieBreakOrder`.

const DEFAULT_INITIAL_CAPACITY: usize = 16;
const TREEIFY_THRESHOLD: usize = 8;
const UNTREEIFY_THRESHOLD: usize = 6;
const MIN_TREEIFY_CAPACITY: usize = 64;

/// The positions of `keys` (distinct, in insertion order) in the map's iteration order.
pub(super) fn iteration_order(keys: &[i32]) -> Vec<usize> {
    let mut map = HashMap::default();
    for &key in keys {
        map.put(key);
    }
    let mut order = Vec::with_capacity(keys.len());
    for &first in &map.table {
        let mut entry = first;
        while let Some(node) = entry {
            order.push(node);
            entry = map.nodes[node].next;
        }
    }
    order
}

/// A `HashMap.Node`, or a `TreeNode` when `tree` is set; nodes are indexed by insertion order.
#[derive(Default)]
struct Node {
    hash: i32,
    next: Option<usize>,
    tree: bool,
    parent: Option<usize>,
    left: Option<usize>,
    right: Option<usize>,
    prev: Option<usize>,
    red: bool,
}

/// One half of a split bin: its first node and its length.
#[derive(Clone, Copy)]
struct Half {
    head: usize,
    count: usize,
}

#[derive(Default)]
struct HashMap {
    nodes: Vec<Node>,
    table: Vec<Option<usize>>,
    threshold: usize,
}

/// `HashMap.hash`: the key's hash with its high half folded into the low half.
fn spread(key: i32) -> i32 {
    key ^ ((key as u32) >> 16) as i32
}

/// The table slot of a spread hash, `(n - 1) & hash`.
fn slot(hash: i32, capacity: usize) -> usize {
    (hash as u32 as usize) & (capacity - 1)
}

impl HashMap {
    /// `putVal` for a key not yet in the map.
    fn put(&mut self, key: i32) {
        if self.table.is_empty() {
            self.resize();
        }
        let hash = spread(key);
        let node = self.nodes.len();
        self.nodes.push(Node {
            hash,
            ..Node::default()
        });
        let index = slot(hash, self.table.len());
        match self.table[index] {
            None => self.table[index] = Some(node),
            Some(first) if self.nodes[first].tree => self.put_tree_val(first, node),
            Some(first) => {
                let mut tail = first;
                let mut bin_count = 0;
                while let Some(next) = self.nodes[tail].next {
                    tail = next;
                    bin_count += 1;
                }
                self.nodes[tail].next = Some(node);
                if bin_count >= TREEIFY_THRESHOLD - 1 {
                    self.treeify_bin(hash);
                }
            }
        }
        if self.nodes.len() > self.threshold {
            self.resize();
        }
    }

    /// `resize`: double the table (the first one has 16 slots), splitting each bin in two.
    fn resize(&mut self) {
        let old_capacity = self.table.len();
        let capacity = if old_capacity == 0 {
            DEFAULT_INITIAL_CAPACITY
        } else {
            old_capacity * 2
        };
        // The load factor is 0.75.
        self.threshold = capacity / 4 * 3;
        let old_table = std::mem::replace(&mut self.table, vec![None; capacity]);
        for (index, first) in old_table.into_iter().enumerate() {
            let Some(first) = first else { continue };
            if self.nodes[first].next.is_none() {
                self.table[slot(self.nodes[first].hash, capacity)] = Some(first);
            } else if self.nodes[first].tree {
                self.split(first, index, old_capacity);
            } else {
                let [lo, hi] = self.split_chain(first, old_capacity);
                self.table[index] = lo.map(|half| half.head);
                self.table[index + old_capacity] = hi.map(|half| half.head);
            }
        }
    }

    /// Distribute a bin's `next` chain between the low and the high slot by the hash bit `bit`,
    /// each half keeping its order and linking `prev` back along it.
    fn split_chain(&mut self, first: usize, bit: usize) -> [Option<Half>; 2] {
        let mut halves: [Option<(Half, usize)>; 2] = [None, None];
        let mut entry = Some(first);
        while let Some(node) = entry {
            entry = self.nodes[node].next.take();
            let high = self.nodes[node].hash as u32 as usize & bit != 0;
            match &mut halves[usize::from(high)] {
                half @ None => {
                    self.nodes[node].prev = None;
                    *half = Some((
                        Half {
                            head: node,
                            count: 1,
                        },
                        node,
                    ));
                }
                Some((half, tail)) => {
                    self.nodes[node].prev = Some(*tail);
                    self.nodes[*tail].next = Some(node);
                    *tail = node;
                    half.count += 1;
                }
            }
        }
        halves.map(|half| half.map(|(half, _)| half))
    }

    /// `treeifyBin`: a table smaller than 64 slots grows instead; otherwise the bin of `hash`
    /// becomes a red-black tree.
    fn treeify_bin(&mut self, hash: i32) {
        let capacity = self.table.len();
        if capacity < MIN_TREEIFY_CAPACITY {
            self.resize();
            return;
        }
        let Some(first) = self.table[slot(hash, capacity)] else {
            return;
        };
        let mut previous = None;
        let mut entry = Some(first);
        while let Some(node) = entry {
            let node_ref = &mut self.nodes[node];
            node_ref.tree = true;
            node_ref.prev = previous;
            previous = Some(node);
            entry = node_ref.next;
        }
        self.treeify(first);
    }

    /// `TreeNode.treeify`: insert the bin's nodes, in `next` order, into a fresh red-black tree,
    /// then move its root to the front of the bin.
    fn treeify(&mut self, first: usize) {
        let mut root: Option<usize> = None;
        let mut entry = Some(first);
        while let Some(node) = entry {
            entry = self.nodes[node].next;
            self.nodes[node].left = None;
            self.nodes[node].right = None;
            let Some(tree_root) = root else {
                self.nodes[node].parent = None;
                self.nodes[node].red = false;
                root = Some(node);
                continue;
            };
            let (parent, goes_left) = self.descend(tree_root, self.nodes[node].hash);
            self.nodes[node].parent = Some(parent);
            self.attach(parent, node, goes_left);
            root = Some(self.balance_insertion(tree_root, node));
        }
        if let Some(root) = root {
            self.move_root_to_front(root);
        }
    }

    /// `TreeNode.putTreeVal` for a new node: attach it as a leaf, link it right after its parent
    /// in the `next` chain, rebalance, and move the root to the front of the bin.
    fn put_tree_val(&mut self, first: usize, node: usize) {
        let root = self.root(first);
        let (parent, goes_left) = self.descend(root, self.nodes[node].hash);
        let parent_next = self.nodes[parent].next;
        self.nodes[node].tree = true;
        self.nodes[node].next = parent_next;
        self.attach(parent, node, goes_left);
        self.nodes[parent].next = Some(node);
        self.nodes[node].parent = Some(parent);
        self.nodes[node].prev = Some(parent);
        if let Some(parent_next) = parent_next {
            self.nodes[parent_next].prev = Some(node);
        }
        let root = self.balance_insertion(root, node);
        self.move_root_to_front(root);
    }

    /// `TreeNode.root`.
    fn root(&self, mut node: usize) -> usize {
        while let Some(parent) = self.nodes[node].parent {
            node = parent;
        }
        node
    }

    /// The leaf parent a node with `hash` attaches to, and whether as its left child: a smaller
    /// hash goes left (`dir <= 0`).
    fn descend(&self, root: usize, hash: i32) -> (usize, bool) {
        let mut parent = root;
        loop {
            let goes_left = self.nodes[parent].hash > hash;
            let child = if goes_left {
                self.nodes[parent].left
            } else {
                self.nodes[parent].right
            };
            match child {
                Some(child) => parent = child,
                None => return (parent, goes_left),
            }
        }
    }

    fn attach(&mut self, parent: usize, node: usize, goes_left: bool) {
        if goes_left {
            self.nodes[parent].left = Some(node);
        } else {
            self.nodes[parent].right = Some(node);
        }
    }

    /// `TreeNode.moveRootToFront`: unlink the root from the bin's `next` chain and make it the
    /// bin's first node.
    fn move_root_to_front(&mut self, root: usize) {
        let index = slot(self.nodes[root].hash, self.table.len());
        let first = self.table[index];
        if first == Some(root) {
            return;
        }
        self.table[index] = Some(root);
        let root_prev = self.nodes[root].prev;
        let root_next = self.nodes[root].next;
        if let Some(root_next) = root_next {
            self.nodes[root_next].prev = root_prev;
        }
        if let Some(root_prev) = root_prev {
            self.nodes[root_prev].next = root_next;
        }
        if let Some(first) = first {
            self.nodes[first].prev = Some(root);
        }
        self.nodes[root].next = first;
        self.nodes[root].prev = None;
    }

    /// `TreeNode.split`: divide a tree bin between the low and the high slot keeping `next` order;
    /// a half of at most six nodes becomes a plain chain, and a larger one is re-treeified when
    /// the other half is not empty (else it keeps the tree as it is).
    fn split(&mut self, first: usize, index: usize, bit: usize) {
        let [lo, hi] = self.split_chain(first, bit);
        for (half, other, target) in [(lo, hi, index), (hi, lo, index + bit)] {
            let Some(half) = half else { continue };
            self.table[target] = Some(half.head);
            if half.count <= UNTREEIFY_THRESHOLD {
                self.untreeify(half.head);
            } else if other.is_some() {
                self.treeify(half.head);
            }
        }
    }

    /// `TreeNode.untreeify`: the bin's nodes become plain nodes in `next` order.
    fn untreeify(&mut self, head: usize) {
        let mut entry = Some(head);
        while let Some(node) = entry {
            let node_ref = &mut self.nodes[node];
            *node_ref = Node {
                hash: node_ref.hash,
                next: node_ref.next,
                ..Node::default()
            };
            entry = node_ref.next;
        }
    }

    fn is_red(&self, node: Option<usize>) -> bool {
        node.is_some_and(|node| self.nodes[node].red)
    }

    /// `TreeNode.balanceInsertion`: restore the red-black invariants after inserting `node`,
    /// returning the (possibly new) root.
    fn balance_insertion(&mut self, mut root: usize, mut node: usize) -> usize {
        self.nodes[node].red = true;
        loop {
            let Some(mut parent) = self.nodes[node].parent else {
                self.nodes[node].red = false;
                return node;
            };
            let grandparent = match self.nodes[parent].parent {
                Some(grandparent) if self.nodes[parent].red => grandparent,
                _ => return root,
            };
            let uncle_left = self.nodes[grandparent].left;
            if uncle_left == Some(parent) {
                let uncle = self.nodes[grandparent].right;
                if self.is_red(uncle) {
                    self.recolor(uncle, parent, grandparent);
                    node = grandparent;
                } else {
                    let mut grandparent = Some(grandparent);
                    if self.nodes[parent].right == Some(node) {
                        node = parent;
                        root = self.rotate_left(root, node);
                        parent = self.nodes[node].parent.expect("rotated node has a parent");
                        grandparent = self.nodes[parent].parent;
                    }
                    self.nodes[parent].red = false;
                    if let Some(grandparent) = grandparent {
                        self.nodes[grandparent].red = true;
                        root = self.rotate_right(root, grandparent);
                    }
                }
            } else if self.is_red(uncle_left) {
                self.recolor(uncle_left, parent, grandparent);
                node = grandparent;
            } else {
                let mut grandparent = Some(grandparent);
                if self.nodes[parent].left == Some(node) {
                    node = parent;
                    root = self.rotate_right(root, node);
                    parent = self.nodes[node].parent.expect("rotated node has a parent");
                    grandparent = self.nodes[parent].parent;
                }
                self.nodes[parent].red = false;
                if let Some(grandparent) = grandparent {
                    self.nodes[grandparent].red = true;
                    root = self.rotate_left(root, grandparent);
                }
            }
        }
    }

    /// A red uncle: the parent and uncle turn black and the grandparent red.
    fn recolor(&mut self, uncle: Option<usize>, parent: usize, grandparent: usize) {
        if let Some(uncle) = uncle {
            self.nodes[uncle].red = false;
        }
        self.nodes[parent].red = false;
        self.nodes[grandparent].red = true;
    }

    /// `TreeNode.rotateLeft`.
    fn rotate_left(&mut self, mut root: usize, node: usize) -> usize {
        let Some(right) = self.nodes[node].right else {
            return root;
        };
        let right_left = self.nodes[right].left;
        self.nodes[node].right = right_left;
        if let Some(right_left) = right_left {
            self.nodes[right_left].parent = Some(node);
        }
        let parent = self.nodes[node].parent;
        self.nodes[right].parent = parent;
        match parent {
            None => {
                root = right;
                self.nodes[right].red = false;
            }
            Some(parent) if self.nodes[parent].left == Some(node) => {
                self.nodes[parent].left = Some(right)
            }
            Some(parent) => self.nodes[parent].right = Some(right),
        }
        self.nodes[right].left = Some(node);
        self.nodes[node].parent = Some(right);
        root
    }

    /// `TreeNode.rotateRight`.
    fn rotate_right(&mut self, mut root: usize, node: usize) -> usize {
        let Some(left) = self.nodes[node].left else {
            return root;
        };
        let left_right = self.nodes[left].right;
        self.nodes[node].left = left_right;
        if let Some(left_right) = left_right {
            self.nodes[left_right].parent = Some(node);
        }
        let parent = self.nodes[node].parent;
        self.nodes[left].parent = parent;
        match parent {
            None => {
                root = left;
                self.nodes[left].red = false;
            }
            Some(parent) if self.nodes[parent].right == Some(node) => {
                self.nodes[parent].right = Some(left)
            }
            Some(parent) => self.nodes[parent].left = Some(left),
        }
        self.nodes[left].right = Some(node);
        self.nodes[node].parent = Some(left);
        root
    }
}

#[cfg(test)]
mod tests {
    use super::iteration_order;

    /// The keys in the order `java.util.HashMap.keySet()` returns them after `put`ting `keys`.
    fn iterated(keys: &[i32]) -> Vec<i32> {
        iteration_order(keys)
            .into_iter()
            .map(|index| keys[index])
            .collect()
    }

    // Each expected order was read from `new HashMap<Integer, Object>()` after `put`ting the keys
    // in the given order (OpenJDK 21).

    /// Slots iterate in order, each in insertion order, before and after the thirteenth key grows
    /// the table; `0x10000` spreads to slot 1 and `0x30000` to slot 3.
    #[test]
    fn slots_keep_insertion_order_through_a_load_factor_resize() {
        assert_eq!(iterated(&[33, 1, 17, 2]), [33, 1, 17, 2]);
        assert_eq!(
            iterated(&[0x10000, 1, 0x30000, 15, 14, 13, 12, 11, 10, 9, 8, 7, 6]),
            [0x10000, 1, 0x30000, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]
        );
    }

    /// The ninth key of one 16-slot bin treeifies it, which grows the table instead.
    #[test]
    fn treeifying_a_small_table_resizes_it() {
        assert_eq!(
            iterated(&[2083, 2080, 2096, 2112, 2128, 2144, 2099, 2160, 2176, 2192, 2208]),
            [2080, 2112, 2144, 2176, 2208, 2083, 2096, 2128, 2160, 2192, 2099]
        );
    }

    /// At 64 slots the bin becomes a red-black tree, and each insertion moves its root first.
    #[test]
    fn a_tree_bin_iterates_root_first() {
        assert_eq!(
            iterated(&[
                2117, 2112, 2176, 2240, 2304, 2368, 2432, 2496, 2560, 2624, 2688, 2752, 2816, 2880,
                3072, 2088
            ]),
            [
                2304, 2112, 2176, 2240, 2368, 2432, 2496, 2560, 2624, 2688, 2752, 2816, 2880, 3072,
                2117, 2088
            ]
        );
    }

    /// Growing past 48 entries splits the tree bin: the nine-node half is re-treeified, the
    /// six-node half becomes a plain chain.
    #[test]
    fn a_split_tree_bin_retreeifies_or_untreeifies_each_half() {
        assert_eq!(
            iterated(&[
                2112, 2176, 2240, 2304, 2368, 2432, 2496, 2560, 2624, 2688, 2752, 2816, 3072, 3200,
                3328, 2087, 2121, 2154, 2187, 2220, 2253, 2286, 2319, 2352, 2385, 2422, 2455, 2488,
                2521, 2554, 2587, 2620, 2653, 2686, 2719, 2785, 2818, 2855, 2889, 3077, 3111, 3144,
                3177, 3210, 3243, 3276, 3309, 3342, 3375
            ]),
            [
                2560, 2304, 2176, 2432, 2688, 2816, 3072, 3200, 3328, 2818, 3077, 3210, 2187, 3342,
                2319, 2455, 2587, 2719, 2087, 2855, 3111, 3243, 2220, 3375, 2352, 2488, 2620, 2112,
                2240, 2368, 2496, 2624, 2752, 3144, 2121, 2889, 3276, 2253, 2385, 2521, 2653, 2785,
                3177, 2154, 3309, 2286, 2422, 2554, 2686
            ]
        );
    }
}
