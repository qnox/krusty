//! ASM's `InsnList` over [`Node`]s: an instruction list whose nodes keep their identity while
//! others are inserted and removed around them. kotlinc's passes hold on to nodes (a suspension
//! point's first and last instruction, a line number to move) across edits that shift every
//! position, so a port needs node identities rather than indices. [`EditableMethod`] is a
//! [`MethodNode`] whose body is being edited through such a list.

use std::cell::RefCell;

use crate::jvm::method_node::{LabelId, MethodNode, Node};

/// A node's identity in its [`InsnList`], stable while other nodes come and go.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct NodeId(u32);

#[derive(Clone, Debug)]
struct Entry {
    node: Node,
    prev: Option<NodeId>,
    next: Option<NodeId>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct InsnList {
    entries: Vec<Option<Entry>>,
    first: Option<NodeId>,
    last: Option<NodeId>,
    /// The nodes in order, rebuilt on the first query after an edit (ASM's `cache`).
    order: RefCell<Option<Vec<NodeId>>>,
    /// Each live node's position in `order`, indexed by id.
    positions: RefCell<Vec<usize>>,
}

impl InsnList {
    pub(crate) fn new(nodes: Vec<Node>) -> Self {
        let mut list = InsnList::default();
        for node in nodes {
            list.add(node);
        }
        list
    }

    fn entry(&self, id: NodeId) -> &Entry {
        self.entries[id.0 as usize]
            .as_ref()
            .expect("a removed node is not in the list")
    }

    fn entry_mut(&mut self, id: NodeId) -> &mut Entry {
        self.invalidate();
        self.entries[id.0 as usize]
            .as_mut()
            .expect("a removed node is not in the list")
    }

    fn invalidate(&mut self) {
        *self.order.get_mut() = None;
    }

    fn allocate(&mut self, node: Node) -> NodeId {
        let id = NodeId(u32::try_from(self.entries.len()).expect("a method has under 2^32 nodes"));
        self.entries.push(Some(Entry {
            node,
            prev: None,
            next: None,
        }));
        id
    }

    pub(crate) fn node(&self, id: NodeId) -> &Node {
        &self.entry(id).node
    }

    /// Whether `id` is still in the list.
    pub(crate) fn contains(&self, id: NodeId) -> bool {
        self.entries
            .get(id.0 as usize)
            .is_some_and(|entry| entry.is_some())
    }

    /// Replaces the node `id` stands for, keeping its identity (ASM's `set`, whose callers keep
    /// no reference to the old node).
    pub(crate) fn set(&mut self, id: NodeId, node: Node) {
        self.entry_mut(id).node = node;
    }

    pub(crate) fn first(&self) -> Option<NodeId> {
        self.first
    }

    pub(crate) fn last(&self) -> Option<NodeId> {
        self.last
    }

    pub(crate) fn next(&self, id: NodeId) -> Option<NodeId> {
        self.entry(id).next
    }

    pub(crate) fn prev(&self, id: NodeId) -> Option<NodeId> {
        self.entry(id).prev
    }

    /// Appends `node` and returns its identity.
    pub(crate) fn add(&mut self, node: Node) -> NodeId {
        let id = self.allocate(node);
        match self.last {
            Some(last) => self.link(Some(last), id, None),
            None => self.link(None, id, None),
        }
        id
    }

    fn link(&mut self, prev: Option<NodeId>, id: NodeId, next: Option<NodeId>) {
        self.invalidate();
        {
            let entry = self.entries[id.0 as usize].as_mut().expect("a live node");
            entry.prev = prev;
            entry.next = next;
        }
        match prev {
            Some(prev) => self.entry_mut(prev).next = Some(id),
            None => self.first = Some(id),
        }
        match next {
            Some(next) => self.entry_mut(next).prev = Some(id),
            None => self.last = Some(id),
        }
    }

    /// Inserts `nodes` right after `anchor` (ASM's `insert(location, list)`), or at the start
    /// when `anchor` is `None`; returns their identities in order.
    pub(crate) fn insert_after(&mut self, anchor: Option<NodeId>, nodes: Vec<Node>) -> Vec<NodeId> {
        let mut prev = anchor;
        let mut ids = Vec::with_capacity(nodes.len());
        for node in nodes {
            let next = match prev {
                Some(prev) => self.next(prev),
                None => self.first,
            };
            let id = self.allocate(node);
            self.link(prev, id, next);
            ids.push(id);
            prev = Some(id);
        }
        ids
    }

    /// Inserts `nodes` right before `anchor` (ASM's `insertBefore`); returns their identities.
    pub(crate) fn insert_before(&mut self, anchor: NodeId, nodes: Vec<Node>) -> Vec<NodeId> {
        let prev = self.prev(anchor);
        self.insert_after(prev, nodes)
    }

    pub(crate) fn remove(&mut self, id: NodeId) {
        let (prev, next) = {
            let entry = self.entry(id);
            (entry.prev, entry.next)
        };
        match prev {
            Some(prev) => self.entry_mut(prev).next = next,
            None => self.first = next,
        }
        match next {
            Some(next) => self.entry_mut(next).prev = prev,
            None => self.last = prev,
        }
        self.invalidate();
        self.entries[id.0 as usize] = None;
    }

    fn ensure_order(&self) {
        if self.order.borrow().is_some() {
            return;
        }
        let mut order = Vec::new();
        let mut positions = vec![usize::MAX; self.entries.len()];
        let mut cursor = self.first;
        while let Some(id) = cursor {
            positions[id.0 as usize] = order.len();
            order.push(id);
            cursor = self.entry(id).next;
        }
        *self.order.borrow_mut() = Some(order);
        *self.positions.borrow_mut() = positions;
    }

    /// The node's position (ASM's `indexOf`).
    pub(crate) fn index_of(&self, id: NodeId) -> usize {
        self.ensure_order();
        let position = self.positions.borrow()[id.0 as usize];
        debug_assert_ne!(position, usize::MAX, "a removed node has no position");
        position
    }

    /// The node at `index` (ASM's `get`).
    #[cfg(test)]
    pub(crate) fn at(&self, index: usize) -> NodeId {
        self.ensure_order();
        self.order
            .borrow()
            .as_ref()
            .expect("the order was just built")[index]
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.ensure_order();
        self.order.borrow().as_ref().map_or(0, Vec::len)
    }

    /// The identities in order.
    pub(crate) fn ids(&self) -> Vec<NodeId> {
        self.ensure_order();
        self.order
            .borrow()
            .clone()
            .expect("the order was just built")
    }

    /// The position of the label node placing `label`.
    pub(crate) fn label_node(&self, label: LabelId) -> Option<NodeId> {
        self.ids()
            .into_iter()
            .find(|id| matches!(self.node(*id), Node::Label(placed) if *placed == label))
    }

    pub(crate) fn to_nodes(&self) -> Vec<Node> {
        self.ids()
            .into_iter()
            .map(|id| self.node(id).clone())
            .collect()
    }
}

/// A [`MethodNode`] whose instructions are being edited as an [`InsnList`]. `method.nodes` stays
/// empty until [`EditableMethod::finish`]; the try/catch and local-variable tables refer to labels,
/// which keep their identity on their own.
pub(crate) struct EditableMethod {
    pub method: MethodNode,
    pub insns: InsnList,
}

impl EditableMethod {
    pub(crate) fn new(mut method: MethodNode) -> Self {
        let nodes = std::mem::take(&mut method.nodes);
        EditableMethod {
            method,
            insns: InsnList::new(nodes),
        }
    }

    /// The method as it stands, for an analysis: its node positions are the list's indices.
    pub(crate) fn snapshot(&self) -> MethodNode {
        let mut method = self.method.clone();
        method.nodes = self.insns.to_nodes();
        method
    }

    /// The position of the label node placing `label`, which the method must contain.
    pub(crate) fn label_index(&self, label: LabelId) -> usize {
        let id = self
            .insns
            .label_node(label)
            .expect("a label the method refers to is placed in it");
        self.insns.index_of(id)
    }

    /// ASM's `indexOf` for a label a local-variable entry or try/catch block names: its position,
    /// or `-1` while it is not placed (a state label the transformation places later).
    pub(crate) fn label_order(&self, label: LabelId) -> isize {
        self.insns.label_node(label).map_or(-1, |id| {
            isize::try_from(self.insns.index_of(id)).expect("a position fits isize")
        })
    }

    pub(crate) fn finish(mut self) -> MethodNode {
        self.method.nodes = self.insns.to_nodes();
        self.method
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jvm::method_node::Insn;

    fn op(op: u8) -> Node {
        Node::Insn(Insn::Op(op))
    }

    #[test]
    fn nodes_keep_their_identity_across_edits() {
        let mut list = InsnList::new(vec![op(1), op(2), op(3)]);
        let second = list.at(1);
        list.insert_before(second, vec![op(10), op(11)]);
        assert_eq!(list.index_of(second), 3);
        list.insert_after(Some(second), vec![op(12)]);
        list.insert_after(None, vec![op(0)]);
        list.remove(list.at(1));
        assert_eq!(
            list.to_nodes(),
            vec![op(0), op(10), op(11), op(2), op(12), op(3)]
        );
        assert_eq!(list.index_of(second), 3);
        assert_eq!(list.len(), 6);
        list.set(second, op(20));
        assert_eq!(list.node(list.at(3)), &op(20));
        let last = list.last().expect("a last node");
        list.remove(last);
        assert_eq!(list.node(list.last().expect("a last node")), &op(12));
    }
}
