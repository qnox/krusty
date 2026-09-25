//! A finished method read back into a [`MethodNode`], with the labels its builder bound.
//!
//! Reading the class-file body gives one label per offset a jump or a table names. kotlinc's
//! rewrites see the labels its code generator placed, and several can stand at one offset: a
//! null check's target, and a later label bound where that check's path joins, are not the same
//! label to its null-check rules. So each jump the builder linked names the builder's own label,
//! and those labels stand at their offset in the order they were bound, after the label the
//! tables name. A jump the builder did not link itself (in a body spliced in from elsewhere) keeps
//! the offset's label. The method's implicit `return` gets a label of its own, last at its offset,
//! so that where it lands can be read off the rewritten body.

use std::collections::{BTreeMap, HashMap};

use super::super::constant_pool_queries::PoolLookup;
use super::super::{ClassWriter, MethodInfo};
use super::RewriteSource;
use crate::jvm::bytecode::instruction_len;
use crate::jvm::classreader::{ExcEntry, MethodLocal};
use crate::jvm::method_node::{CodeAttribute, Insn, LabelId, MethodNode, Node};

/// A finished method's body and the label of its implicit `return`, if it has one.
pub(super) struct FinishedNode {
    pub node: MethodNode,
    pub implicit_return: Option<LabelId>,
}

impl ClassWriter {
    /// The finished `method` read into a node against the writer's own pool. A local-variable entry
    /// without a start covers the method from its first instruction, one without a length runs to
    /// its end; a range reaching past the code is cut at its end.
    pub(super) fn finished_node(
        &self,
        method: &MethodInfo,
        source: &RewriteSource,
        bytes: &[u8],
        pool: &PoolLookup<'_>,
    ) -> Option<FinishedNode> {
        let code_len = bytes.len();
        let handlers: Vec<ExcEntry> = method
            .exceptions
            .iter()
            .map(|&(start_pc, end_pc, handler_pc, catch_type)| ExcEntry {
                start_pc,
                end_pc,
                handler_pc,
                catch_type,
            })
            .collect();
        let locals: Vec<MethodLocal> = method
            .lvt
            .iter()
            .map(|&(name, desc, slot, start, len)| {
                let start = usize::from(start.unwrap_or(0));
                let end = len.map_or(code_len, |len| start + usize::from(len));
                let (start, end) = (start.min(code_len), end.min(code_len));
                Some(MethodLocal {
                    start_pc: start as u16,
                    length: end.checked_sub(start)? as u16,
                    slot,
                    name: self.cp.utf8_at(name)?.to_string(),
                    descriptor: self.cp.utf8_at(desc)?.to_string(),
                })
            })
            .collect::<Option<_>>()?;
        let code = CodeAttribute {
            max_stack: method.max_stack,
            max_locals: method.max_locals,
            code: bytes,
            handlers: &handlers,
            lines: &method.lnt,
            locals: &locals,
        };
        let mut node =
            MethodNode::read_code(source.access, &source.name, &source.desc, &code, pool).ok()?;
        let mut offsets = Vec::new();
        let mut pc = 0;
        while pc < code_len {
            offsets.push(pc);
            pc += instruction_len(bytes, pc)?;
        }
        let implicit_return = method.implicit_void_return_pc.map(|pc| {
            let at = offsets.partition_point(|&at| at < usize::from(pc));
            (at, node.new_label())
        });
        let builder_labels = builder_labels(&mut node, source, &offsets, code_len);

        // The labels an instruction gets besides the reader's: they stand right after the
        // reader's label there, ahead of its line numbers, as one run of labels does in kotlinc.
        let extra = |index: usize| {
            let builder = offsets
                .get(index)
                .and_then(|offset| builder_labels.get(offset))
                .into_iter()
                .flatten()
                .cloned();
            let implicit = implicit_return
                .filter(|&(at, _)| at == index)
                .map(|(_, label)| Node::Label(label));
            builder.chain(implicit)
        };
        let mut nodes = Vec::with_capacity(node.nodes.len() + builder_labels.len() + 1);
        let mut index = 0;
        let mut placed = false;
        for entry in std::mem::take(&mut node.nodes) {
            match entry {
                Node::Label(_) if !placed => {
                    nodes.push(entry);
                    nodes.extend(extra(index));
                    placed = true;
                }
                Node::Insn(_) => {
                    if !placed {
                        nodes.extend(extra(index));
                    }
                    nodes.push(entry);
                    index += 1;
                    placed = false;
                }
                _ => nodes.push(entry),
            }
        }
        if !placed {
            nodes.extend(extra(index));
        }
        node.nodes = nodes;
        Some(FinishedNode {
            node,
            implicit_return: implicit_return.map(|(_, label)| label),
        })
    }
}

/// Retarget each jump the builder linked to a label of its own, one per builder label, and return
/// those labels by the offset they stand at, in the order the builder bound them.
fn builder_labels(
    node: &mut MethodNode,
    source: &RewriteSource,
    offsets: &[usize],
    code_len: usize,
) -> BTreeMap<usize, Vec<Node>> {
    let builder = &source.builder;
    let mut linked: HashMap<usize, u32> = HashMap::new();
    for &(operand, label) in &builder.fixups {
        if label.builder != builder.id {
            continue;
        }
        if let Some(at) = operand.checked_sub(1) {
            linked.insert(at, label.index);
        }
    }
    // Where each label the reader placed stands.
    let mut label_offsets: HashMap<LabelId, usize> = HashMap::new();
    let mut index = 0;
    for entry in &node.nodes {
        match entry {
            Node::Label(label) => {
                label_offsets.insert(*label, offsets.get(index).copied().unwrap_or(code_len));
            }
            Node::Insn(_) => index += 1,
            Node::Line { .. } => {}
        }
    }
    let mut own: BTreeMap<u32, LabelId> = BTreeMap::new();
    let mut fresh = Vec::new();
    let mut index = 0;
    for position in 0..node.nodes.len() {
        let Node::Insn(insn) = &node.nodes[position] else {
            continue;
        };
        let pc = offsets[index];
        index += 1;
        let Insn::Jump { target, .. } = insn else {
            continue;
        };
        let Some(&label) = linked.get(&pc) else {
            continue;
        };
        let bound = builder.labels[label as usize];
        if Some(&bound) != label_offsets.get(target) {
            continue;
        }
        let retargeted = *own.entry(label).or_insert_with(|| {
            fresh.push(label);
            node.new_label()
        });
        if let Node::Insn(Insn::Jump { target, .. }) = &mut node.nodes[position] {
            *target = retargeted;
        }
    }
    fresh.sort_by_key(|&label| (builder.bind_sequence(label as usize), label));
    let mut standing: BTreeMap<usize, Vec<Node>> = BTreeMap::new();
    for label in fresh {
        standing
            .entry(builder.labels[label as usize])
            .or_default()
            .push(Node::Label(own[&label]));
    }
    standing
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jvm::classfile::{ClassWriter, CodeBuilder, ACC_PUBLIC, ACC_STATIC};

    #[test]
    fn finished_body_restores_same_offset_branch_labels_in_bind_order() {
        let mut writer = ClassWriter::new("T", "java/lang/Object");
        let mut code = CodeBuilder::new(0);
        let null_target = code.new_label();
        let join = code.new_label();
        code.aconst_null();
        code.ifnull(null_target);
        code.goto(join);
        code.bind(null_target);
        code.bind(join);
        code.implicit_ret_void();
        code.link();
        writer.add_method(ACC_PUBLIC | ACC_STATIC, "f", "()V", &code);

        let method = &writer.methods[0];
        let source = method.rewrite_source.as_deref().expect("rewrite source");
        let bytes = method.code.as_deref().expect("method body");
        let pool = PoolLookup::new(&writer.cp, &writer.bootstrap_methods);
        let finished = writer
            .finished_node(method, source, bytes, &pool)
            .expect("finished node");

        let return_at = finished
            .node
            .nodes
            .iter()
            .position(|node| *node == Node::Insn(Insn::Op(0xb1)))
            .expect("return");
        let labels: Vec<LabelId> = finished.node.nodes[..return_at]
            .iter()
            .rev()
            .take_while(|node| matches!(node, Node::Label(_)))
            .filter_map(|node| match node {
                Node::Label(label) => Some(*label),
                _ => None,
            })
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        // The reader's offset label comes first. The two distinct branch labels follow in bind
        // order, and the implicit return's provenance label is last.
        assert_eq!(labels.len(), 4);
        let targets: Vec<LabelId> = finished
            .node
            .instructions()
            .filter_map(|insn| match insn {
                Insn::Jump { target, .. } => Some(*target),
                _ => None,
            })
            .collect();
        assert_eq!(targets, labels[1..3]);
        assert_eq!(finished.implicit_return, Some(labels[3]));
    }
}
