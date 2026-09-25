//! The method as the temporaries rules edit and match it: an instruction list whose nodes keep
//! their identity while the rules insert and remove instructions around them, and which labels
//! separate two instructions.
//!
//! kotlinc's rules match raw adjacency. A label kotlinc keeps between two instructions — one a
//! jump, a switch or a protected range names (an arrival), or one a line number or a local's range
//! names (a mark) — defeats a pattern; a label nothing names does not, since ASM would not have
//! created it. Labels never move while the rules run, so what a run of them in front of an
//! instruction is made of is fixed when the body is read.

use std::collections::{HashMap, HashSet};

use super::super::insn_list::{InsnList, NodeId};
use crate::jvm::method_node::{Insn, LabelId, MethodNode, Node};

pub(super) struct Body {
    pub(super) list: InsnList,
    label_nodes: HashMap<LabelId, NodeId>,
    /// The number of the instruction each label stood in front of when the body was read: where
    /// kotlinc's label still stands after the rules removed the instructions around it.
    origins: HashMap<LabelId, usize>,
    /// Labels a jump or switch targets, or a protected range starts, ends or handles at.
    arrivals: HashSet<LabelId>,
    /// Labels a line number starts at, or a local's range starts or ends at.
    marks: HashSet<LabelId>,
    /// Where each protected range starts, and where each ends or is handled.
    protected_starts: HashSet<LabelId>,
    protected_ends: HashSet<LabelId>,
    handlers: HashSet<LabelId>,
}

impl Body {
    /// `method`'s body, taken out of it to be edited; its tables stay behind.
    pub(super) fn take(method: &mut MethodNode) -> Body {
        let mut arrivals = HashSet::new();
        let mut marks = HashSet::new();
        let mut origins = HashMap::new();
        let mut ordinal = 0usize;
        for node in &method.nodes {
            match node {
                Node::Insn(insn) => {
                    arrivals.extend(insn.jump_targets());
                    ordinal += 1;
                }
                Node::Line { start, .. } => {
                    marks.insert(*start);
                }
                Node::Label(label) => {
                    origins.insert(*label, ordinal);
                }
            }
        }
        let mut protected_starts = HashSet::new();
        let mut protected_ends = HashSet::new();
        let mut handlers = HashSet::new();
        for block in &method.try_catch_blocks {
            protected_starts.insert(block.start);
            protected_ends.insert(block.end);
            handlers.insert(block.handler);
            arrivals.extend([block.start, block.end, block.handler]);
        }
        for local in &method.local_variables {
            marks.extend([local.start, local.end]);
        }
        let list = InsnList::new(std::mem::take(&mut method.nodes));
        let label_nodes = list
            .ids()
            .into_iter()
            .filter_map(|id| match list.node(id) {
                Node::Label(label) => Some((*label, id)),
                _ => None,
            })
            .collect();
        Body {
            list,
            label_nodes,
            origins,
            arrivals,
            marks,
            protected_starts,
            protected_ends,
            handlers,
        }
    }

    pub(super) fn insn(&self, id: NodeId) -> &Insn {
        match self.list.node(id) {
            Node::Insn(insn) => insn,
            _ => unreachable!("an instruction's identity names an instruction"),
        }
    }

    /// Whether `id` is still in the body.
    pub(super) fn has(&self, id: NodeId) -> bool {
        self.list.contains(id)
    }

    /// The first instruction at or after the node `from`.
    fn insn_from(&self, from: Option<NodeId>) -> Option<NodeId> {
        let mut cursor = from;
        while let Some(id) = cursor {
            if matches!(self.list.node(id), Node::Insn(_)) {
                return Some(id);
            }
            cursor = self.list.next(id);
        }
        None
    }

    pub(super) fn first_insn(&self) -> Option<NodeId> {
        self.insn_from(self.list.first())
    }

    pub(super) fn next_insn(&self, id: NodeId) -> Option<NodeId> {
        self.insn_from(self.list.next(id))
    }

    pub(super) fn prev_insn(&self, id: NodeId) -> Option<NodeId> {
        let mut cursor = self.list.prev(id);
        while let Some(at) = cursor {
            if matches!(self.list.node(at), Node::Insn(_)) {
                return Some(at);
            }
            cursor = self.list.prev(at);
        }
        None
    }

    /// Every instruction, in order.
    pub(super) fn instructions(&self) -> Vec<NodeId> {
        let mut ids = Vec::new();
        let mut cursor = self.first_insn();
        while let Some(id) = cursor {
            ids.push(id);
            cursor = self.next_insn(id);
        }
        ids
    }

    /// The instruction `label` stands in front of.
    pub(super) fn insn_at(&self, label: LabelId) -> Option<NodeId> {
        self.insn_from(Some(self.label_nodes[&label]))
    }

    /// The nodes standing between instruction `id` and the instruction before it, in order.
    pub(super) fn run_before(&self, id: NodeId) -> Vec<NodeId> {
        let mut run = Vec::new();
        let mut cursor = self.list.prev(id);
        while let Some(at) = cursor {
            if matches!(self.list.node(at), Node::Insn(_)) {
                break;
            }
            run.push(at);
            cursor = self.list.prev(at);
        }
        run.reverse();
        run
    }

    /// The labels standing in front of instruction `id`, in order.
    pub(super) fn labels_before(&self, id: NodeId) -> Vec<LabelId> {
        self.run_before(id)
            .into_iter()
            .filter_map(|at| match self.list.node(at) {
                Node::Label(label) => Some(*label),
                _ => None,
            })
            .collect()
    }

    /// Whether a label kotlinc keeps stands in front of instruction `id`.
    pub(super) fn labelled(&self, id: NodeId) -> bool {
        self.labels_before(id)
            .iter()
            .any(|label| self.arrivals.contains(label) || self.marks.contains(label))
    }

    /// Whether a label with non-trivial predecessors stands in front of instruction `id`.
    pub(super) fn arrived(&self, id: NodeId) -> bool {
        self.labels_before(id)
            .iter()
            .any(|label| self.arrivals.contains(label))
    }

    /// Whether a line number or a local's range starts or ends in front of instruction `id`.
    pub(super) fn marked(&self, id: NodeId) -> bool {
        self.labels_before(id)
            .iter()
            .any(|label| self.marks.contains(label))
    }

    /// Whether a protected range starts in front of instruction `id`.
    pub(super) fn protected_start(&self, id: NodeId) -> bool {
        self.labels_before(id)
            .iter()
            .any(|label| self.protected_starts.contains(label))
    }

    /// Whether a protected range started or ended where `label` stood when the body was read. An
    /// instruction the rules removed since still separates the two.
    pub(super) fn protected_bound_at(&self, label: LabelId) -> bool {
        let origin = self.origins[&label];
        self.protected_starts
            .iter()
            .chain(&self.protected_ends)
            .any(|bound| self.origins[bound] == origin)
    }

    /// Whether a protected range starts, ends or is handled in front of instruction `id`.
    pub(super) fn protected_label(&self, id: NodeId) -> bool {
        self.labels_before(id).iter().any(|label| {
            self.protected_starts.contains(label)
                || self.protected_ends.contains(label)
                || self.handlers.contains(label)
        })
    }

    /// The `len` instructions from `first` on, when there are that many and no label kotlinc keeps
    /// stands between them.
    pub(super) fn raw_sequence(&self, first: NodeId, len: usize) -> Option<Vec<NodeId>> {
        let mut ids = vec![first];
        while ids.len() < len {
            let next = self.next_insn(*ids.last().expect("at least the first"))?;
            if self.labelled(next) {
                return None;
            }
            ids.push(next);
        }
        Some(ids)
    }

    /// Whether instruction `a` stands before instruction `b`.
    pub(super) fn precedes(&self, a: NodeId, b: NodeId) -> bool {
        self.list.index_of(a) < self.list.index_of(b)
    }

    /// Insert `insn` right in front of the node `anchor`.
    pub(super) fn insert_before(&mut self, anchor: NodeId, insn: Insn) -> NodeId {
        self.list.insert_before(anchor, vec![Node::Insn(insn)])[0]
    }

    /// Insert `insn` right after the node `anchor`.
    pub(super) fn insert_after(&mut self, anchor: NodeId, insn: Insn) -> NodeId {
        self.list.insert_after(Some(anchor), vec![Node::Insn(insn)])[0]
    }

    pub(super) fn replace(&mut self, id: NodeId, insn: Insn) {
        self.list.set(id, Node::Insn(insn));
    }

    pub(super) fn remove(&mut self, id: NodeId) {
        self.list.remove(id);
    }
}
