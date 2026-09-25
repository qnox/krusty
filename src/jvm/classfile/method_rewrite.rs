//! Bytecode rewrites applied to a finished method when its class is written.
//!
//! kotlinc does not write its temporaries onto the operand stack while generating code; it writes
//! them as locals and lets a bytecode pass fold them (see `bytecode_passes::temporaries`). This is
//! the place krusty does the same. The rewrite is first decided when a method is added, so frame
//! classes are interned in kotlinc's order, and that outcome is reused when the class is written if
//! every table the decision read is unchanged. Methods whose line or local-variable tables are
//! attached later are decided again from those final tables. The method is read back into a
//! [`MethodNode`](crate::jvm::method_node::MethodNode) (see [`finished_node`]), kotlinc's passes run over it, and it is laid out again: every table
//! keyed by a byte offset — exception ranges, line numbers, local ranges, the implicit return —
//! hangs off a label and moves with it.
//!
//! A rewrite adds no instruction operand, and edits no frame: the class carries the frames the
//! rewritten body implies, computed when the class is written (see [`super::stack_maps`]). The
//! rewritten body is only kept if those frames can be computed; otherwise the method is written
//! exactly as emitted.

mod finished_node;

use std::collections::BTreeSet;

use super::bytecode_analysis::{ControlGraph, FrameTypes, Handler, VerificationType};
use super::constant_pool_queries::PoolLookup;
use super::stack_maps;
use super::{ClassWriter, CodeBuilder, LvtEntry, MethodInfo, VerifType};
use crate::jvm::bytecode_passes::redundant_checkcasts::{self, StackTops};
use crate::jvm::bytecode_passes::{
    dead_code, local_slots, negated_jumps, redundant_gotos, redundant_null_checks, stack_peephole,
    temporaries,
};
use crate::jvm::inline::{disassemble, insn_offsets_at, Insn};
use crate::jvm::method_node::{LabelId, MethodNode};
use finished_node::FinishedNode;

/// What a method keeps so it can be rewritten when its class is written: its builder, for the
/// labels its branches name.
#[derive(Clone)]
pub(super) struct RewriteSource {
    pub access: u16,
    pub name: String,
    pub desc: String,
    pub builder: CodeBuilder,
    /// The rewrite already decided when the method was added (see [`ClassWriter::remembered_rewrite`]).
    pub decided: Option<Decided>,
}

/// A rewrite decided from a method's tables, reused when the class is written if they still hold.
#[derive(Clone)]
pub(super) struct Decided {
    inputs: RewriteInputs,
    outcome: Option<Rewritten>,
}

/// Everything of a method the rewrite reads besides its source and the constant pool.
#[derive(Clone, PartialEq)]
struct RewriteInputs {
    code: Option<Vec<u8>>,
    exceptions: Vec<(u16, u16, u16, u16)>,
    lnt: Vec<(u16, u16)>,
    lvt: Vec<LvtEntry>,
    implicit_void_return_pc: Option<u16>,
    max_stack: u16,
    max_locals: u16,
}

impl RewriteInputs {
    fn of(method: &MethodInfo) -> Self {
        RewriteInputs {
            code: method.code.clone(),
            exceptions: method.exceptions.clone(),
            lnt: method.lnt.clone(),
            lvt: method.lvt.clone(),
            implicit_void_return_pc: method.implicit_void_return_pc,
            max_stack: method.max_stack,
            max_locals: method.max_locals,
        }
    }
}

/// The method a body belongs to, as the verifier's entry state and the frame computation need it.
#[derive(Clone, Copy)]
pub(super) struct MethodIdentity<'a> {
    pub access: u16,
    pub name: &'a str,
    pub desc: &'a str,
}

/// A rewritten method's `Code` and every table that moved with it. Its frames and maxima are
/// computed from it when the class is written.
#[derive(Clone)]
pub(super) struct Rewritten {
    pub(super) code: Vec<u8>,
    pub(super) exceptions: Vec<(u16, u16, u16, u16)>,
    pub(super) lnt: Vec<(u16, u16)>,
    pub(super) lvt: Vec<LvtEntry>,
    implicit_void_return_pc: Option<u16>,
}

/// `locals` as one entry per slot: a `long`/`double` followed by the `top` of its second word.
fn expand_slots(locals: &[VerifType]) -> Vec<VerifType> {
    let mut slots = Vec::with_capacity(locals.len());
    for local in locals {
        slots.push(local.clone());
        if matches!(local, VerifType::Long | VerifType::Double) {
            slots.push(VerifType::Top);
        }
    }
    slots
}

/// Slots a load, store or `iinc` touches, with their width.
pub(super) fn var_slot(insn: &Insn) -> Option<(u16, u16)> {
    let Insn::Plain { op, operands } = insn else {
        return None;
    };
    let wide_kind = |op: u8| u16::from(matches!(op, 0x16 | 0x18 | 0x37 | 0x39)) + 1;
    Some(match *op {
        0x15..=0x19 | 0x36..=0x3a => (u16::from(*operands.first()?), wide_kind(*op)),
        0x1a..=0x2d => {
            let n = *op - 0x1a;
            (u16::from(n % 4), if matches!(n / 4, 1 | 3) { 2 } else { 1 })
        }
        0x3b..=0x4e => {
            let n = *op - 0x3b;
            (u16::from(n % 4), if matches!(n / 4, 1 | 3) { 2 } else { 1 })
        }
        0x84 => (u16::from(*operands.first()?), 1),
        0xc4 => {
            let inner = *operands.first()?;
            let slot = u16::from_be_bytes([*operands.get(1)?, *operands.get(2)?]);
            (slot, if inner == 0x84 { 1 } else { wide_kind(inner) })
        }
        _ => return None,
    })
}

impl MethodInfo {
    /// Hold `rewritten` in place of the body and tables the method held.
    pub(super) fn take_rewritten(&mut self, rewritten: Rewritten) {
        self.code = Some(rewritten.code);
        self.exceptions = rewritten.exceptions;
        self.lnt = rewritten.lnt;
        self.lvt = rewritten.lvt;
        self.implicit_void_return_pc = rewritten.implicit_void_return_pc;
    }
}

impl ClassWriter {
    /// Apply kotlinc's bytecode rewrites to every method, now that each one's tables are final.
    pub(super) fn rewrite_methods(&mut self) {
        for index in 0..self.methods.len() {
            let Some(mut source) = self.methods[index].rewrite_source.take() else {
                continue;
            };
            let decided = source
                .decided
                .take()
                .filter(|decided| decided.inputs == RewriteInputs::of(&self.methods[index]));
            let rewritten = match decided {
                Some(decided) => decided.outcome,
                None => self.rewritten(&self.methods[index], &source),
            };
            let Some(rewritten) = rewritten else {
                continue;
            };
            self.methods[index].take_rewritten(rewritten);
            crate::trace_compiler!("bytecode", "rewrote {}{}", source.name, source.desc);
        }
    }

    /// The rewrite of the method at `index`, remembered for when the class is written. The
    /// rewrite reads the method's tables, which can still change until then, and the constant
    /// pool, which only grows: a constant it found keeps its index, so an outcome that found every
    /// constant it looked for holds while the tables do. One that missed a constant is decided
    /// again, when a later method may have added it.
    pub(super) fn remembered_rewrite(&mut self, index: usize) -> Option<Rewritten> {
        let method = &self.methods[index];
        let source = method.rewrite_source.as_deref()?;
        let mut pool = PoolLookup::new(&self.cp, &self.bootstrap_methods);
        let outcome = self.rewritten_with(method, source, &mut pool);
        if !pool.missed() {
            let inputs = RewriteInputs::of(method);
            if let Some(source) = self.methods[index].rewrite_source.as_deref_mut() {
                source.decided = Some(Decided {
                    inputs,
                    outcome: outcome.clone(),
                });
            }
        }
        outcome
    }

    /// `method` after kotlinc's rewrites, or `None` when none applies or the rewritten body could
    /// not be proven to keep its frames.
    pub(super) fn rewritten(
        &self,
        method: &MethodInfo,
        source: &RewriteSource,
    ) -> Option<Rewritten> {
        self.rewritten_with(
            method,
            source,
            &mut PoolLookup::new(&self.cp, &self.bootstrap_methods),
        )
    }

    fn rewritten_with(
        &self,
        method: &MethodInfo,
        source: &RewriteSource,
        pool: &mut PoolLookup<'_>,
    ) -> Option<Rewritten> {
        let bytes = method.code.as_ref()?;
        if bytes.is_empty() || source.builder.bytes != *bytes {
            return None;
        }
        let FinishedNode {
            node,
            implicit_return,
        } = self.finished_node(method, source, bytes, pool)?;
        // The builder's labels and branch fixups name offsets of the emitted bytes, so the node must
        // lay out exactly as emitted.
        if pool.missed() || node.assemble(pool).ok()?.code != *bytes {
            return None;
        }
        let identity = MethodIdentity {
            access: source.access,
            name: &source.name,
            desc: &source.desc,
        };
        self.optimized(method, identity, node, implicit_return, pool)
    }

    /// kotlinc's optimizer passes over `node`, the body `method` currently holds (its code, and the
    /// tables keyed by its offsets), or `None` when none applies or the result could not be proven
    /// to keep its frames. `implicit_return` labels the method's implicit `return`, if it has one.
    /// The optimized body looks its constants up in `pool`, which records whether one was missing.
    pub(super) fn optimized(
        &self,
        method: &MethodInfo,
        source: MethodIdentity<'_>,
        mut node: MethodNode,
        implicit_return: Option<LabelId>,
        pool: &mut PoolLookup<'_>,
    ) -> Option<Rewritten> {
        let bytes = method.code.as_ref()?;
        // Entry state: `this` (instance methods) and the parameters, one entry per slot.
        let mut entry = Vec::new();
        if source.access & 0x0008 == 0 {
            entry.push(if source.name == "<init>" {
                VerifType::UninitializedThis
            } else {
                VerifType::ObjectName(self.internal_name.clone())
            });
        }
        if !Self::append_param_verif_types(source.desc, &mut entry) {
            return None;
        }
        let entry = expand_slots(&entry);
        // The verifier's view of the method as emitted, by instruction number: what the
        // redundant-cast pass asks about the value each cast sees.
        let insns = disassemble(bytes)?;
        let offsets = insn_offsets_at(&insns, 0);
        let index_of = |pc: u16| offsets.binary_search(&usize::from(pc)).ok();
        let handlers = method
            .exceptions
            .iter()
            .map(|&(start, end, handler, _)| {
                Some(Handler {
                    start: index_of(start)?,
                    end: index_of(end)?,
                    handler: index_of(handler)?,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        let graph = ControlGraph::build(&insns, &handlers)?;
        // Seed the walk with the frames the emitted bytecode itself implies: in particular, a typed
        // catch handler enters with its declared exception class, not the generic `Throwable` used
        // by an untyped exceptional edge. These are computed frames, never emitter-recorded
        // semantic guesses; the rewritten body is computed and validated independently below.
        let flow_types_cell = std::cell::OnceCell::new();
        let flow_types = || {
            flow_types_cell
                .get_or_init(|| {
                    let body = stack_maps::Body {
                        access: source.access,
                        name: source.name,
                        descriptor: source.desc,
                        code: bytes,
                        exceptions: &method.exceptions,
                        labels: stack_maps::table_labels(&method.lnt, &method.lvt, bytes.len()),
                    };
                    let frames = self
                        .compute_frames(&body)
                        .ok()
                        .map(|computed| stack_maps::verif_frames(computed.frames()))?;
                    FrameTypes::analyze(&insns, &graph, &entry, &frames, self)
                })
                .as_ref()
        };
        // kotlinc's `RedundantNullCheckMethodTransformer` and `RedundantCheckCastEliminationMethodTransformer`
        // both judge the method as emitted; what they select goes before the temporaries pass.
        let removed: BTreeSet<usize> = redundant_checkcasts::select(&node, flow_types)
            .into_iter()
            .chain(redundant_null_checks::select(&node))
            .collect();
        let mut position = 0;
        node.nodes.retain(|_| {
            position += 1;
            !removed.contains(&(position - 1))
        });
        let temporaries = temporaries::eliminate(&mut node);
        let folded_any = !removed.is_empty() || temporaries.is_some();
        let pinned = temporaries.map(|done| done.pinned).unwrap_or_default();
        // kotlinc's stack peephole runs after the temporaries pass, then its `goto` cleanup and
        // its `NegatedJumpsMethodTransformer`, the last of its rewrites (see `stack_peephole`,
        // `redundant_gotos`, `negated_jumps`).
        let peephole_changed = stack_peephole::optimize(&mut node);
        let gotos_changed = redundant_gotos::remove(&mut node, &pinned);
        let jumps_negated = negated_jumps::negate(&mut node, &pinned);
        // kotlinc always ends with `DeadCodeEliminationMethodTransformer`: whatever the passes
        // above left unreachable goes, with its line numbers, empty protected ranges and emptied
        // local variables, and the slots left unused close up (see `dead_code`, `local_slots`).
        let dead = dead_code::eliminate(&mut node);
        let removed_locals = dead
            .as_ref()
            .map_or(&[][..], |dead| &dead.removed_locals[..]);
        let parameter_slots: BTreeSet<u16> = (0..u16::try_from(entry.len()).ok()?).collect();
        let renumbered = local_slots::compact(&mut node, &parameter_slots);
        if !folded_any
            && !peephole_changed
            && !gotos_changed
            && !jumps_negated
            && dead.is_none()
            && !renumbered
        {
            return None;
        }
        // A short branch that no longer reaches its target, or a body grown past the JVM's limit,
        // fails to lay out; the method is then written as emitted.
        let assembled = node.assemble(pool).ok()?;
        if pool.missed()
            || assembled
                .exception_table
                .iter()
                .any(|&(start, end, _, _)| start >= end)
        {
            return None;
        }
        // Each kept local keeps its pool entries, and a bound it left open stays open: a missing
        // start is the method's start and a missing length runs to its end.
        let kept_locals = method
            .lvt
            .iter()
            .enumerate()
            .filter(|&(at, _)| !removed_locals.get(at).copied().unwrap_or(false))
            .map(|(_, entry)| entry);
        let lvt: Vec<LvtEntry> = kept_locals
            .zip(&assembled.local_variables)
            .map(|(&(name, desc, _, old_start, old_len), local)| {
                let start = old_start.map(|_| local.start_pc);
                let end = local.start_pc + local.length;
                let len = old_len.map(|_| end - start.unwrap_or(0));
                (name, desc, local.slot, start, len)
            })
            .collect();
        if lvt.iter().any(|&(_, _, _, _, len)| len == Some(0)) {
            // Kotlin's complete optimizer removes unused/empty LVT entries. This focused rewrite
            // does not own debug-local deletion, so preserve the original method instead.
            return None;
        }
        let implicit_void_return_pc = match implicit_return {
            Some(label) => Some(assembled.offset_of(label)?),
            None => None,
        };

        // The class carries the frames the rewritten body implies. A body they cannot be computed
        // for is written as emitted.
        self.compute_frames(&stack_maps::Body {
            access: source.access,
            name: source.name,
            descriptor: source.desc,
            code: &assembled.code,
            exceptions: &assembled.exception_table,
            labels: stack_maps::table_labels(&assembled.line_numbers, &lvt, assembled.code.len()),
        })
        .ok()?;
        Some(Rewritten {
            code: assembled.code,
            exceptions: assembled.exception_table,
            lnt: assembled.line_numbers,
            lvt,
            implicit_void_return_pc,
        })
    }
}

/// The class-file analysis answers the redundant-cast pass: `null`, or exactly the cast's class,
/// on top of the verifier's stack.
impl StackTops for FrameTypes {
    fn is_exactly(&self, index: usize, class: &str) -> bool {
        match self.before(index).and_then(|state| state.stack.last()) {
            Some(VerificationType::Null) => true,
            Some(VerificationType::Reference(name)) => **name == *class,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jvm::classfile::{ACC_PUBLIC, ACC_STATIC};

    /// `static int f() { int t = 1; return t; }` with `t` a temporary the rewrite folds.
    fn writer_with_temporary() -> ClassWriter {
        let mut writer = ClassWriter::new("T", "java/lang/Object");
        let mut code = CodeBuilder::new(0);
        code.push_int(1, &mut writer);
        code.istore(0);
        code.iload(0);
        code.ireturn();
        code.link();
        writer.add_method(ACC_PUBLIC | ACC_STATIC, "f", "()I", &code);
        writer
    }

    #[test]
    fn a_rewrite_decided_when_the_method_is_added_is_the_one_written() {
        let mut writer = writer_with_temporary();
        let fresh = {
            let method = &writer.methods[0];
            let source = method.rewrite_source.as_deref().expect("rewrite source");
            writer
                .rewritten(method, source)
                .expect("the temporary folds")
        };
        // Make the remembered result observably different from a fresh rewrite. The line is valid
        // but is not part of the source method, so it can reach the method only through reuse.
        let remembered_line = vec![(0, 7)];
        let source = writer.methods[0]
            .rewrite_source
            .as_deref_mut()
            .expect("rewrite source");
        let remembered = source
            .decided
            .as_mut()
            .and_then(|decided| decided.outcome.as_mut())
            .expect("remembered rewrite");
        assert_eq!(remembered.code, fresh.code);
        remembered.lnt.clone_from(&remembered_line);
        writer.rewrite_methods();
        assert_eq!(writer.methods[0].code.as_deref(), Some(&fresh.code[..]));
        assert_eq!(writer.methods[0].lnt, remembered_line);
    }

    #[test]
    fn a_table_attached_after_the_method_is_added_is_rewritten_again() {
        let mut writer = writer_with_temporary();
        writer.methods[0].lnt = vec![(0, 7)];
        let method = &writer.methods[0];
        let source = method.rewrite_source.as_deref().expect("rewrite source");
        let fresh = writer
            .rewritten(method, source)
            .expect("the temporary folds");
        assert_eq!(fresh.lnt, vec![(0, 7)]);
        writer.rewrite_methods();
        assert_eq!(writer.methods[0].lnt, vec![(0, 7)]);
        assert_eq!(writer.methods[0].code.as_deref(), Some(&fresh.code[..]));
    }
}
