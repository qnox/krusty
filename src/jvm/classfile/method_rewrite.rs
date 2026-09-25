//! Bytecode rewrites applied to a finished method when its class is written.
//!
//! kotlinc does not write its temporaries onto the operand stack while generating code; it writes
//! them as locals and lets a bytecode pass fold them (see `bytecode_passes::temporaries`). This is
//! the place krusty does the same. It runs when the class is written, because only then is every
//! table final: several line and local-variable tables are attached after a method is added, and
//! which values are temporaries depends on them. The method is read back into a
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
            let Some(source) = self.methods[index].rewrite_source.take() else {
                continue;
            };
            let Some(rewritten) = self.rewritten(&self.methods[index], &source) else {
                continue;
            };
            self.methods[index].take_rewritten(rewritten);
            crate::trace_compiler!("bytecode", "rewrote {}{}", source.name, source.desc);
        }
    }

    /// `method` after kotlinc's rewrites, or `None` when none applies or the rewritten body could
    /// not be proven to keep its frames.
    pub(super) fn rewritten(
        &self,
        method: &MethodInfo,
        source: &RewriteSource,
    ) -> Option<Rewritten> {
        let bytes = method.code.as_ref()?;
        if bytes.is_empty() || source.builder.bytes != *bytes {
            return None;
        }
        let mut pool = PoolLookup::new(&self.cp, &self.bootstrap_methods);
        let FinishedNode {
            node,
            implicit_return,
        } = self.finished_node(method, source, bytes, &pool)?;
        // The builder's labels and branch fixups name offsets of the emitted bytes, so the node must
        // lay out exactly as emitted.
        if pool.missed() || node.assemble(&mut pool).ok()?.code != *bytes {
            return None;
        }
        let identity = MethodIdentity {
            access: source.access,
            name: &source.name,
            desc: &source.desc,
        };
        self.optimized(method, identity, node, implicit_return)
    }

    /// kotlinc's optimizer passes over `node`, the body `method` currently holds (its code, and the
    /// tables keyed by its offsets), or `None` when none applies or the result could not be proven
    /// to keep its frames. `implicit_return` labels the method's implicit `return`, if it has one.
    pub(super) fn optimized(
        &self,
        method: &MethodInfo,
        source: MethodIdentity<'_>,
        mut node: MethodNode,
        implicit_return: Option<LabelId>,
    ) -> Option<Rewritten> {
        let bytes = method.code.as_ref()?;
        let mut pool = PoolLookup::new(&self.cp, &self.bootstrap_methods);
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
        let assembled = node.assemble(&mut pool).ok()?;
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
