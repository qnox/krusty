//! The bridge between a finished method's [`MethodNode`] and the index-addressed instruction list
//! kotlinc's rewrite passes still work on.
//!
//! A method is read into a node when its class is written, so its exception ranges, line numbers
//! and local ranges hang off labels rather than byte offsets. The passes (temporaries, the stack
//! peephole, the `goto` and jump cleanups, dead code, local slots) predate that form: they address
//! an instruction by its index in the emitted body and return a `(Insn, Placement)` list. The
//! bridge hands them that list ([`IndexedBody`]) together with the original index each label
//! stands in front of, and turns their result back into a node ([`relabel`]) by standing each
//! original index's labels where its instructions landed. Every table then moves with its labels
//! when the node is assembled.
//!
//! This module is a migration adapter: it shrinks as the passes move onto [`MethodNode`] and goes
//! when the last of them has.

use std::collections::{BTreeMap, BTreeSet};

use crate::jvm::classfile::bytecode_analysis::Handler;
use crate::jvm::classfile::temporaries::Placement;
use crate::jvm::inline::{BranchTarget, Insn};
use crate::jvm::method_node::{
    self, AssembleError, ConstantPoolView, ConstantSink, LabelId, LocalVariable, MethodNode, Node,
    TryCatchBlock,
};

const GOTO: u8 = 0xa7;
const JSR: u8 = 0xa8;
const GOTO_W: u8 = 0xc8;
const JSR_W: u8 = 0xc9;

/// A method body in the passes' form: its instructions addressed by index, and its tables by the
/// index of the instruction each boundary stands in front of (`insns.len()` for the end of code).
pub(super) struct IndexedBody {
    pub insns: Vec<Insn>,
    /// The original index each label of the node stands in front of.
    labels: BTreeMap<LabelId, usize>,
    /// The protected ranges, in exception-table order.
    pub handlers: Vec<Handler>,
    /// The line numbers as `(index, line)`, in table order.
    pub lines: Vec<(usize, u16)>,
    /// Each local variable's range as `[start, end)`, in table order; `None` for one that starts at
    /// the end of the code and so covers no instruction.
    pub locals: Vec<Option<(usize, usize)>>,
}

impl IndexedBody {
    /// `node` in the passes' form, its operands interned as pool indices by `sink`.
    pub(super) fn new(
        node: &MethodNode,
        sink: &mut impl ConstantSink,
    ) -> Result<IndexedBody, AssembleError> {
        let mut labels = BTreeMap::new();
        let mut count = 0;
        for entry in &node.nodes {
            match entry {
                Node::Label(label) => {
                    labels.insert(*label, count);
                }
                Node::Insn(_) => count += 1,
                Node::Line { .. } => {}
            }
        }
        let index = |label: LabelId| {
            labels
                .get(&label)
                .copied()
                .ok_or(AssembleError::UnplacedLabel(label))
        };
        let mut insns = Vec::with_capacity(count);
        let mut lines = Vec::new();
        for entry in &node.nodes {
            let insn = match entry {
                Node::Label(_) => continue,
                Node::Line { line, start } => {
                    lines.push((index(*start)?, *line));
                    continue;
                }
                Node::Insn(insn) => insn,
            };
            insns.push(match insn {
                method_node::Insn::Jump { op, target } => Insn::Branch {
                    op: *op,
                    target: BranchTarget::Internal(index(*target)?),
                },
                method_node::Insn::TableSwitch {
                    low,
                    default,
                    labels,
                    ..
                } => Insn::TableSwitch {
                    default: index(*default)?,
                    low: *low,
                    targets: labels
                        .iter()
                        .map(|&label| index(label))
                        .collect::<Result<_, _>>()?,
                },
                method_node::Insn::LookupSwitch {
                    default,
                    keys,
                    labels,
                } => Insn::LookupSwitch {
                    default: index(*default)?,
                    pairs: keys
                        .iter()
                        .zip(labels)
                        .map(|(&key, &label)| Ok((key, index(label)?)))
                        .collect::<Result<_, _>>()?,
                },
                insn => match insn.encode_in_place(sink)? {
                    Some(bytes) => Insn::Plain {
                        op: bytes[0],
                        operands: bytes[1..].to_vec(),
                    },
                    None => unreachable!("only jumps and switches depend on where they stand"),
                },
            });
        }
        let handlers = node
            .try_catch_blocks
            .iter()
            .map(|block| {
                Ok(Handler {
                    start: index(block.start)?,
                    end: index(block.end)?,
                    handler: index(block.handler)?,
                })
            })
            .collect::<Result<_, AssembleError>>()?;
        let locals = node
            .local_variables
            .iter()
            .map(|local| {
                let start = index(local.start)?;
                Ok((start < count).then_some((start, index(local.end)?)))
            })
            .collect::<Result<_, AssembleError>>()?;
        Ok(IndexedBody {
            insns,
            labels,
            handlers,
            lines,
            locals,
        })
    }

    fn index_of(&self, label: LabelId) -> usize {
        self.labels[&label]
    }
}

/// What the passes made of an [`IndexedBody`], and what its tables lost on the way.
pub(super) struct PassOutcome<'a> {
    /// The rewritten instructions, each with the original index it belongs to.
    pub nodes: &'a [(Insn, Placement)],
    /// Whether the branch at a placement jumps to a label standing after the instructions a rule
    /// inserted in front of its target (see `temporaries::Rewrite::late_labels`).
    pub late_branch: &'a dyn Fn(Placement) -> bool,
    /// The original indices a null-check fold left a value on the stack at.
    pub stack_targets: &'a [usize],
    /// Per line number, per protected range and per local variable, whether dead-code
    /// elimination removed it; empty when it removed nothing.
    pub removed_lines: &'a [bool],
    pub removed_handlers: &'a [bool],
    pub removed_locals: &'a [bool],
    /// A local slot's number after the passes renumbered them.
    pub slot: &'a dyn Fn(u16) -> Option<u16>,
    /// The original index of the method's implicit `return`, if it has one.
    pub implicit_return: Option<usize>,
}

/// A rewritten body back in node form, with the label its implicit `return` now stands at.
pub(super) struct Relabelled {
    pub node: MethodNode,
    pub implicit_return: Option<LabelId>,
}

/// Where the labels of each original index stand among the rewritten nodes: the early position is
/// in front of every instruction of the index's group, the late one after the instructions a rule
/// inserted in front of it. Index `insns.len()` (the end of the code) stands after every node.
struct Positions {
    early: Vec<usize>,
    late: Vec<usize>,
}

impl Positions {
    fn of(nodes: &[(Insn, Placement)], count: usize) -> Positions {
        let end = nodes.len();
        let mut early = vec![end; count + 1];
        let mut next = 0;
        for (k, slot) in early.iter_mut().enumerate().take(count) {
            while next < end && nodes[next].1.group() < k {
                next += 1;
            }
            *slot = next;
        }
        let mut late = vec![end; count + 1];
        let mut next = 0;
        for (k, slot) in late.iter_mut().enumerate().take(count) {
            while next < end && (nodes[next].1.group() < k || nodes[next].1 == Placement::Before(k))
            {
                next += 1;
            }
            *slot = next;
        }
        Positions { early, late }
    }
}

/// The labels of the rewritten node, handed out per original index and position.
struct Labels {
    early: BTreeMap<usize, LabelId>,
    late: BTreeMap<usize, LabelId>,
}

impl Labels {
    fn early(&mut self, node: &mut MethodNode, index: usize) -> LabelId {
        *self.early.entry(index).or_insert_with(|| node.new_label())
    }

    fn late(&mut self, node: &mut MethodNode, index: usize) -> LabelId {
        *self.late.entry(index).or_insert_with(|| node.new_label())
    }
}

/// `rewritten` as a node over `original`'s tables: each original index's labels stand where its
/// instructions landed, each removed table entry is gone, and each local has its new slot.
pub(super) fn relabel(
    original: &MethodNode,
    indexed: &IndexedBody,
    rewritten: &PassOutcome<'_>,
    pool: &impl ConstantPoolView,
) -> Option<Relabelled> {
    let count = indexed.insns.len();
    let positions = Positions::of(rewritten.nodes, count);
    // A line or local boundary at a null-check fold's target describes the original instruction
    // after its labels, so it belongs after the `pop` inserted in front of that instruction. Other
    // insertions in front of an index (the `dup` beside a branch) do not move a table boundary.
    let debug_late: BTreeSet<usize> = rewritten
        .stack_targets
        .iter()
        .copied()
        .filter(|&target| positions.early[target] != positions.late[target])
        .collect();
    let removed = |table: &[bool], at: usize| table.get(at).copied().unwrap_or(false);

    let mut node = original.clone();
    let mut labels = Labels {
        early: BTreeMap::new(),
        late: BTreeMap::new(),
    };
    // Every original label keeps standing at its index; the first one at an index is its early
    // label.
    let mut original_labels: Vec<(usize, LabelId)> = indexed
        .labels
        .iter()
        .map(|(&label, &index)| (index, label))
        .collect();
    original_labels.sort_unstable();
    for &(index, label) in &original_labels {
        labels.early.entry(index).or_insert(label);
    }
    let debug_label = |labels: &mut Labels, node: &mut MethodNode, index: usize| {
        if debug_late.contains(&index) {
            labels.late(node, index)
        } else {
            labels.early(node, index)
        }
    };

    let mut lines_at: BTreeMap<LabelId, Vec<u16>> = BTreeMap::new();
    for (at, &(index, line)) in indexed.lines.iter().enumerate() {
        if !removed(rewritten.removed_lines, at) {
            let label = debug_label(&mut labels, &mut node, index);
            lines_at.entry(label).or_default().push(line);
        }
    }
    let mut try_catch_blocks = Vec::with_capacity(original.try_catch_blocks.len());
    for (at, block) in original.try_catch_blocks.iter().enumerate() {
        if removed(rewritten.removed_handlers, at) {
            continue;
        }
        let mut relabel = |label| labels.early(&mut node, indexed.index_of(label));
        try_catch_blocks.push(TryCatchBlock {
            start: relabel(block.start),
            end: relabel(block.end),
            handler: relabel(block.handler),
            catch_type: block.catch_type.clone(),
        });
    }
    let mut local_variables = Vec::with_capacity(original.local_variables.len());
    for (at, local) in original.local_variables.iter().enumerate() {
        if removed(rewritten.removed_locals, at) {
            continue;
        }
        local_variables.push(LocalVariable {
            name: local.name.clone(),
            desc: local.desc.clone(),
            start: debug_label(&mut labels, &mut node, indexed.index_of(local.start)),
            end: debug_label(&mut labels, &mut node, indexed.index_of(local.end)),
            slot: (rewritten.slot)(local.slot)?,
        });
    }
    let implicit_return = rewritten
        .implicit_return
        .map(|index| debug_label(&mut labels, &mut node, index));

    let mut insns = Vec::with_capacity(rewritten.nodes.len());
    for (insn, placement) in rewritten.nodes {
        let mut target = |index: usize, late: bool| {
            if late {
                labels.late(&mut node, index)
            } else {
                labels.early(&mut node, index)
            }
        };
        insns.push(match insn {
            Insn::Branch { op, target: to } | Insn::BranchW { op, target: to } => {
                let BranchTarget::Internal(to) = *to else {
                    return None;
                };
                let op = match *op {
                    GOTO_W => GOTO,
                    JSR_W => JSR,
                    op => op,
                };
                method_node::Insn::Jump {
                    op,
                    target: target(to, (rewritten.late_branch)(*placement)),
                }
            }
            Insn::TableSwitch {
                default,
                low,
                targets,
            } => method_node::Insn::TableSwitch {
                low: *low,
                high: low + targets.len() as i32 - 1,
                default: target(*default, false),
                labels: targets.iter().map(|&to| target(to, false)).collect(),
            },
            Insn::LookupSwitch { default, pairs } => method_node::Insn::LookupSwitch {
                default: target(*default, false),
                keys: pairs.iter().map(|&(key, _)| key).collect(),
                labels: pairs.iter().map(|&(_, to)| target(to, false)).collect(),
            },
            Insn::Plain { op, operands } => {
                let mut bytes = Vec::with_capacity(1 + operands.len());
                bytes.push(*op);
                bytes.extend_from_slice(operands);
                method_node::Insn::decode(&bytes, pool).ok()?
            }
        });
    }

    // Stand every label at its position, in original-index order and early before late, each
    // followed by the line numbers that start at it.
    let mut standing: BTreeMap<usize, Vec<(usize, bool, LabelId)>> = BTreeMap::new();
    for &(index, label) in &original_labels {
        standing
            .entry(positions.early[index])
            .or_default()
            .push((index, false, label));
    }
    for (&index, &label) in &labels.early {
        if indexed.labels.contains_key(&label) {
            continue;
        }
        standing
            .entry(positions.early[index])
            .or_default()
            .push((index, false, label));
    }
    for (&index, &label) in &labels.late {
        standing
            .entry(positions.late[index])
            .or_default()
            .push((index, true, label));
    }
    let mut nodes = Vec::with_capacity(insns.len() + standing.len());
    let mut insns = insns.into_iter();
    for position in 0..=rewritten.nodes.len() {
        if let Some(mut here) = standing.remove(&position) {
            here.sort_unstable();
            for (_, _, label) in here {
                nodes.push(Node::Label(label));
                for &line in lines_at.get(&label).into_iter().flatten() {
                    nodes.push(Node::Line { line, start: label });
                }
            }
        }
        nodes.extend(insns.next().map(Node::Insn));
    }
    node.nodes = nodes;
    node.try_catch_blocks = try_catch_blocks;
    node.local_variables = local_variables;
    Some(Relabelled {
        node,
        implicit_return,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jvm::classfile::constant_pool_queries::PoolLookup;
    use crate::jvm::classfile::ClassWriter;
    use crate::jvm::method_node::AssembledCode;

    const IFEQ: u8 = 0x99;

    fn call() -> method_node::Insn {
        method_node::Insn::Method {
            op: 0xb8,
            owner: "fixture/Owner".to_string(),
            name: "call".to_string(),
            desc: "()V".to_string(),
            interface: false,
        }
    }

    /// `if (x != 0) call(); call(); return`, guarded up to the second call, whose handler is that
    /// call. Original indices: 0 `iload_0`, 1 `ifeq`, 2 `call`, 3 `goto`, 4 `call`, 5 `return`.
    fn body() -> MethodNode {
        let mut node = MethodNode::new(0x0009, "f", "(I)V");
        let [start, taken, join, end] = [(); 4].map(|_| node.new_label());
        node.nodes = vec![
            Node::Label(start),
            Node::Line { line: 3, start },
            Node::Insn(method_node::Insn::Var { op: 0x15, slot: 0 }),
            Node::Insn(method_node::Insn::Jump {
                op: IFEQ,
                target: taken,
            }),
            Node::Insn(call()),
            Node::Insn(method_node::Insn::Jump {
                op: GOTO,
                target: join,
            }),
            Node::Label(taken),
            Node::Line {
                line: 5,
                start: taken,
            },
            Node::Insn(call()),
            Node::Label(join),
            Node::Insn(method_node::Insn::Op(0xb1)),
            Node::Label(end),
        ];
        node.try_catch_blocks = vec![TryCatchBlock {
            start,
            end: taken,
            handler: taken,
            catch_type: None,
        }];
        node.local_variables = vec![LocalVariable {
            name: "x".to_string(),
            desc: "I".to_string(),
            start,
            end,
            slot: 0,
        }];
        node
    }

    /// A writer whose pool holds every constant `node` names.
    fn writer_for(node: &MethodNode) -> ClassWriter {
        let mut writer = ClassWriter::new("T", "java/lang/Object");
        node.assemble(&mut writer).expect("assemble");
        writer
    }

    fn identity(indexed: &IndexedBody) -> Vec<(Insn, Placement)> {
        indexed
            .insns
            .iter()
            .enumerate()
            .map(|(index, insn)| (insn.clone(), Placement::Original(index)))
            .collect()
    }

    /// Relabel `nodes` over `node` and lay the result out against `writer`'s pool.
    fn laid_out(
        writer: &ClassWriter,
        node: &MethodNode,
        change: impl FnOnce(&IndexedBody) -> (Vec<(Insn, Placement)>, Vec<usize>),
        late_branch: &dyn Fn(Placement) -> bool,
        implicit_return: Option<usize>,
    ) -> (AssembledCode, Option<u16>) {
        let mut pool = PoolLookup::new(&writer.cp, &writer.bootstrap_methods);
        let indexed = IndexedBody::new(node, &mut pool).expect("index");
        let (nodes, stack_targets) = change(&indexed);
        let relabelled = relabel(
            node,
            &indexed,
            &PassOutcome {
                nodes: &nodes,
                late_branch,
                stack_targets: &stack_targets,
                removed_lines: &[],
                removed_handlers: &[],
                removed_locals: &[],
                slot: &|slot| Some(slot + 1),
                implicit_return,
            },
            &pool,
        )
        .expect("relabel");
        let assembled = relabelled.node.assemble(&mut pool).expect("assemble");
        assert!(
            !pool.missed(),
            "the rewrite named a constant the pool lacks"
        );
        let implicit_return = relabelled
            .implicit_return
            .map(|label| assembled.offset_of(label).expect("placed"));
        (assembled, implicit_return)
    }

    #[test]
    fn an_unchanged_body_lays_out_as_it_was() {
        let node = body();
        let writer = writer_for(&node);
        let mut original = writer_for(&node);
        let expected = node.assemble(&mut original).expect("assemble");
        let (assembled, implicit_return) = laid_out(
            &writer,
            &node,
            |indexed| (identity(indexed), Vec::new()),
            &|_| false,
            Some(5),
        );
        assert_eq!(assembled.code, expected.code);
        assert_eq!(assembled.exception_table, expected.exception_table);
        assert_eq!(assembled.line_numbers, expected.line_numbers);
        // Only the slot moved, by the renumbering the passes asked for.
        assert_eq!(assembled.local_variables.len(), 1);
        assert_eq!(assembled.local_variables[0].slot, 1);
        assert_eq!(
            (
                assembled.local_variables[0].start_pc,
                assembled.local_variables[0].length
            ),
            (0, expected.code.len() as u16)
        );
        // iload_0 @0, ifeq @1, call @4, goto @7, call @10, return @13.
        assert_eq!(implicit_return, Some(13));
    }

    /// A `pop` inserted in front of original index 4, where a null-check fold left a value: the
    /// protected range and an ordinary branch still end and land in front of it, while the line,
    /// a late branch and the implicit return stand after it.
    #[test]
    fn tables_move_with_the_labels_of_their_index() {
        let node = body();
        let writer = writer_for(&node);
        let (assembled, implicit_return) = laid_out(
            &writer,
            &node,
            |indexed| {
                let mut nodes = identity(indexed);
                nodes.insert(
                    4,
                    (
                        Insn::Plain {
                            op: 0x57,
                            operands: Vec::new(),
                        },
                        Placement::Before(4),
                    ),
                );
                (nodes, vec![4])
            },
            &|placement| placement == Placement::Original(1),
            Some(5),
        );
        // iload_0 @0, ifeq @1, call @4, goto @7, pop @10, call @11, return @14.
        assert_eq!(assembled.code.len(), 15);
        assert_eq!(&assembled.code[1..4], &[IFEQ, 0x00, 10]);
        assert_eq!(&assembled.code[7..10], &[GOTO, 0x00, 7]);
        assert_eq!(assembled.exception_table, vec![(0, 10, 10, 0)]);
        assert_eq!(assembled.line_numbers, vec![(0, 3), (11, 5)]);
        assert_eq!(implicit_return, Some(14));
    }

    /// Without a value left at its target, an insertion in front of an index moves no table.
    #[test]
    fn an_insertion_without_a_stack_value_leaves_the_tables_in_front() {
        let node = body();
        let writer = writer_for(&node);
        let (assembled, _) = laid_out(
            &writer,
            &node,
            |indexed| {
                let mut nodes = identity(indexed);
                nodes.insert(
                    4,
                    (
                        Insn::Plain {
                            op: 0x59,
                            operands: Vec::new(),
                        },
                        Placement::Before(4),
                    ),
                );
                (nodes, Vec::new())
            },
            &|_| false,
            None,
        );
        assert_eq!(assembled.line_numbers, vec![(0, 3), (10, 5)]);
        assert_eq!(&assembled.code[1..4], &[IFEQ, 0x00, 9]);
    }

    /// A removed instruction's labels stand in front of whatever follows it.
    #[test]
    fn labels_of_a_removed_instruction_move_to_the_next_one() {
        let node = body();
        let writer = writer_for(&node);
        let (assembled, _) = laid_out(
            &writer,
            &node,
            |indexed| {
                let nodes = identity(indexed)
                    .into_iter()
                    .filter(|(_, placement)| *placement != Placement::Original(4))
                    .collect();
                (nodes, Vec::new())
            },
            &|_| false,
            None,
        );
        // iload_0 @0, ifeq @1, call @4, goto @7, return @10.
        assert_eq!(&assembled.code[1..4], &[IFEQ, 0x00, 9]);
        assert_eq!(assembled.exception_table, vec![(0, 10, 10, 0)]);
        assert_eq!(assembled.line_numbers, vec![(0, 3), (10, 5)]);
        assert_eq!(assembled.local_variables[0].length, 11);
    }
}
