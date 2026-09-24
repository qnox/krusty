//! Bytecode rewrites applied to a finished method when its class is written.
//!
//! kotlinc does not write its temporaries onto the operand stack while generating code; it writes
//! them as locals and lets a bytecode pass fold them (see [`super::temporaries`]). This is the
//! place krusty does the same. It runs when the class is written, because only then is every table
//! final: several line and local-variable tables are attached after a method is added, and which
//! values are temporaries depends on them. The method's instructions are decoded, rewritten and
//! re-assembled, and every table keyed by a byte offset — labels (and so frames and exception
//! ranges), line numbers, local ranges, the implicit return — moves with the instruction it
//! described.
//!
//! A rewrite adds no instruction operand, and edits no frame: the class carries the frames the
//! rewritten body implies, computed when the class is written (see [`super::stack_maps`]). The
//! rewritten body is only kept if those frames can be computed; otherwise the method is written
//! exactly as emitted.

mod node_bridge;

use std::collections::BTreeSet;

use super::bytecode_analysis::{ControlGraph, FrameTypes};
use super::constant_pool_queries::PoolLookup;
use super::temporaries::{self, Body, Placement};
use super::{dead_code, local_slots, negated_jumps, redundant_checkcasts, redundant_gotos};
use super::{stack_maps, stack_peephole};
use super::{ClassWriter, CodeBuilder, LvtEntry, MethodInfo, VerifType};
use crate::jvm::classreader::{ExcEntry, MethodLocal};
use crate::jvm::inline::{assemble, insn_offsets_at, BranchTarget, Insn};
use crate::jvm::method_node::{CodeAttribute, MethodNode};
use node_bridge::IndexedBody;

fn is_expression_null_check(owner: &str, name: &str, descriptor: &str) -> bool {
    owner == "kotlin/jvm/internal/Intrinsics"
        && descriptor == "(Ljava/lang/Object;Ljava/lang/String;)V"
        && matches!(
            name,
            "checkNotNullExpressionValue" | "checkExpressionValueIsNotNull"
        )
}

/// What a method keeps so it can be rewritten when its class is written: its builder, for the
/// labels its branches name.
#[derive(Clone)]
pub(super) struct RewriteSource {
    pub access: u16,
    pub name: String,
    pub desc: String,
    pub builder: CodeBuilder,
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
            let method = &mut self.methods[index];
            method.code = Some(rewritten.code);
            method.exceptions = rewritten.exceptions;
            method.lnt = rewritten.lnt;
            method.lvt = rewritten.lvt;
            method.implicit_void_return_pc = rewritten.implicit_void_return_pc;
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
        let node = self.finished_node(method, source, bytes, &pool)?;
        let indexed = IndexedBody::new(&node, &mut pool).ok()?;
        let insns = &indexed.insns;
        // The builder's labels and branch fixups name offsets of the emitted bytes, so the indexed
        // body must lay out exactly as emitted.
        if pool.missed() || assemble(insns) != *bytes {
            return None;
        }
        let offsets = insn_offsets_at(insns, 0);
        let index_of = |pc: usize| offsets.binary_search(&pc).ok();
        let n = insns.len();
        let handlers = &indexed.handlers;
        let mut arrivals = vec![false; n + 1];
        for insn in insns {
            match insn {
                Insn::Branch {
                    target: BranchTarget::Internal(to),
                    ..
                }
                | Insn::BranchW {
                    target: BranchTarget::Internal(to),
                    ..
                } => arrivals[*to] = true,
                Insn::TableSwitch {
                    default, targets, ..
                } => {
                    for &to in std::iter::once(default).chain(targets) {
                        arrivals[to] = true;
                    }
                }
                Insn::LookupSwitch { default, pairs } => {
                    for &to in std::iter::once(default).chain(pairs.iter().map(|(_, to)| to)) {
                        arrivals[to] = true;
                    }
                }
                _ => {}
            }
        }
        for handler in handlers {
            arrivals[handler.start] = true;
            arrivals[handler.end] = true;
            arrivals[handler.handler] = true;
        }
        let mut marks = vec![false; n + 1];
        let mut lines = vec![false; n + 1];
        for &(at, _) in &indexed.lines {
            marks[at] = true;
            lines[at] = true;
        }
        let mut variable_bounds = vec![false; n + 1];
        let mut named = Vec::new();
        for (range, local) in indexed.locals.iter().zip(&node.local_variables) {
            let Some((start, end)) = *range else {
                continue;
            };
            marks[start] = true;
            marks[end] = true;
            variable_bounds[start] = true;
            variable_bounds[end] = true;
            named.push((start, end, local.slot));
        }
        // Entry state: `this` (instance methods) and the parameters, one entry per slot.
        let mut entry = Vec::new();
        if source.access & 0x0008 == 0 {
            entry.push(if source.name == "<init>" {
                VerifType::UninitializedThis
            } else {
                VerifType::ObjectName(self.internal_name.clone())
            });
        }
        if !Self::append_param_verif_types(&source.desc, &mut entry) {
            return None;
        }
        let entry = expand_slots(&entry);
        let original_graph = ControlGraph::build(insns, handlers)?;
        // The verifier's types before each original instruction, computed at most once. Seed the
        // walk with the frames the original bytecode itself implies: in particular, a typed catch
        // handler enters with its declared exception class, not the generic `Throwable` used by an
        // untyped exceptional edge. These are computed frames, never emitter-recorded semantic
        // guesses; the rewritten body is computed and validated independently below.
        let original_frames_cell = std::cell::OnceCell::new();
        let original_frames = || {
            original_frames_cell
                .get_or_init(|| {
                    let body = stack_maps::Body {
                        access: source.access,
                        name: &source.name,
                        descriptor: &source.desc,
                        code: bytes,
                        exceptions: &method.exceptions,
                        labels: stack_maps::table_labels(&method.lnt, &method.lvt, bytes.len()),
                    };
                    self.compute_frames(&body)
                        .ok()
                        .map(|computed| stack_maps::verif_frames(computed.frames()))
                })
                .as_deref()
        };
        let flow_types_cell = std::cell::OnceCell::new();
        let flow_types = || {
            flow_types_cell
                .get_or_init(|| {
                    FrameTypes::analyze(insns, &original_graph, &entry, original_frames()?, self)
                })
                .as_ref()
        };
        let redundant_casts = redundant_checkcasts::select(self, insns, flow_types);
        // kotlinc's `RedundantNullCheckMethodTransformer`: a `checkNotNull*` of a value its
        // nullability analysis proves non-null goes (see `null_checks`).
        let redundant_null_checks = self.redundant_null_checks(
            insns,
            &original_graph,
            &arrivals,
            usize::from(method.max_locals),
        );
        // Which label each branch jumps to, and the labels bound at each index in the order they
        // stand: kotlinc's rules see labels, and several can share one offset.
        let mut branch_labels: Vec<Option<u32>> = vec![None; n];
        for &(operand, label) in &source.builder.fixups {
            if label.builder != source.builder.id {
                continue;
            }
            if let Some(at) = operand.checked_sub(1).and_then(index_of) {
                branch_labels[at] = Some(label.index);
            }
        }
        let mut labels_at: Vec<Vec<u32>> = vec![Vec::new(); n + 1];
        {
            let mut bound: Vec<(u32, usize, u32)> = Vec::new();
            for (label, &pc) in source.builder.labels.iter().enumerate() {
                let label = label as u32;
                if pc == usize::MAX || source.builder.is_dead_bound(label) {
                    continue;
                }
                if let Some(at) = index_of(pc) {
                    bound.push((source.builder.bind_sequence(label as usize), at, label));
                }
            }
            bound.sort_unstable();
            for (_, at, label) in bound {
                labels_at[at].push(label);
            }
        }
        let body = Body {
            insns,
            handlers,
            arrivals: &arrivals,
            marks: &marks,
            named: &named,
            redundant_casts: &redundant_casts,
            redundant_null_checks: &redundant_null_checks,
            branch_labels: &branch_labels,
            labels_at: &labels_at,
            one_word_static: &|field| {
                self.fieldref_descriptor_at(field)
                    .is_some_and(|descriptor| !matches!(descriptor, "J" | "D"))
            },
            string_constant: &|index| {
                self.loadable_constant_type_at(index)
                    == Some(VerifType::ObjectName("java/lang/String".to_string()))
            },
            expression_null_check: &|method| {
                self.methodref_parts(method)
                    .is_some_and(|(owner, name, descriptor)| {
                        is_expression_null_check(owner, name, descriptor)
                    })
            },
        };
        let folded = temporaries::eliminate(&body);
        let folded_any = folded.is_some();
        let mut rewrite = folded.unwrap_or_else(|| temporaries::Rewrite {
            nodes: insns
                .iter()
                .enumerate()
                .map(|(index, insn)| (insn.clone(), Placement::Original(index)))
                .collect(),
            stack_at_target: Vec::new(),
            late_labels: BTreeSet::new(),
        });
        let protected_starts: Vec<usize> = handlers.iter().map(|handler| handler.start).collect();
        let rewrite_late = rewrite.late_labels.clone();
        let no_late_branch = |_: usize| false;
        let peephole_tables = redundant_gotos::Tables {
            lines: &lines,
            variable_bounds: &variable_bounds,
            protected_starts: &protected_starts,
            // The peephole only uses the tables for NOP retention.
            late_branch: &no_late_branch,
        };
        // kotlinc's stack peephole runs after the temporaries pass and before the `goto` cleanup.
        let handler_entries: Vec<usize> = handlers.iter().map(|handler| handler.handler).collect();
        let peephole = stack_peephole::optimize(
            &mut rewrite.nodes,
            &peephole_tables,
            &handler_entries,
            &stack_peephole::Pool {
                unit_instance: &|field| {
                    matches!(
                        self.cp.fieldref_parts(field),
                        Some(("kotlin/Unit", "INSTANCE", "Lkotlin/Unit;"))
                    )
                },
                compare_int: &|method| {
                    matches!(
                        self.methodref_parts(method),
                        Some(("kotlin/jvm/internal/Intrinsics", "compare", "(II)I"))
                    )
                },
            },
        );
        for &(from, to) in &peephole.moved_branches {
            branch_labels[to] = branch_labels[from].take();
        }
        let late_branch = |index: usize| {
            branch_labels
                .get(index)
                .copied()
                .flatten()
                .is_some_and(|label| rewrite_late.contains(&label))
        };
        let tables = redundant_gotos::Tables {
            lines: &lines,
            variable_bounds: &variable_bounds,
            protected_starts: &protected_starts,
            late_branch: &late_branch,
        };
        let gotos_changed = redundant_gotos::remove(&mut rewrite.nodes, &tables);
        let mut labelled: Vec<bool> = lines
            .iter()
            .zip(&variable_bounds)
            .map(|(&line, &bound)| line || bound)
            .collect();
        for handler in handlers {
            for at in [handler.start, handler.end, handler.handler] {
                labelled[at] = true;
            }
        }
        let jumps_negated = negated_jumps::negate(&mut rewrite.nodes, &labelled, &late_branch);
        // kotlinc always ends with `DeadCodeEliminationMethodTransformer`: whatever the passes
        // above left unreachable goes, with its line numbers, empty protected ranges and emptied
        // local variables (see `dead_code`).
        let stack_targets: Vec<usize> = rewrite
            .stack_at_target
            .iter()
            .map(|(target, _)| *target)
            .collect();
        let dead = dead_code::eliminate(
            &mut rewrite.nodes,
            &dead_code::Flow {
                handlers,
                late_branch: &late_branch,
                lines: &indexed.lines,
                line_after_inserted: &|index| stack_targets.contains(&index),
                locals: &indexed.locals,
            },
        );
        let removed_locals = dead
            .as_ref()
            .map_or(&[][..], |dead| &dead.removed_locals[..]);
        let mut fixed_slots: BTreeSet<u16> = (0..u16::try_from(entry.len()).ok()?).collect();
        for (at, local) in node.local_variables.iter().enumerate() {
            if removed_locals.get(at).copied().unwrap_or(false) {
                continue;
            }
            fixed_slots.insert(local.slot);
            if matches!(local.desc.as_str(), "J" | "D") {
                fixed_slots.insert(local.slot + 1);
            }
        }
        let renumbered = local_slots::compact(&mut rewrite.nodes, &fixed_slots);
        if !folded_any
            && !peephole.changed
            && !gotos_changed
            && !jumps_negated
            && dead.is_none()
            && renumbered.is_none()
        {
            return None;
        }

        // Back to a node: each original index's labels stand where its instructions landed, so
        // every table moves with them when the node is laid out again.
        let late_label = |label: u32| rewrite.late_labels.contains(&label);
        let relabelled = node_bridge::relabel(
            &node,
            &indexed,
            &node_bridge::PassOutcome {
                nodes: &rewrite.nodes,
                late_branch: &|placement| match placement {
                    Placement::Original(index) => branch_labels
                        .get(index)
                        .copied()
                        .flatten()
                        .is_some_and(late_label),
                    Placement::Before(_) | Placement::After(_) => false,
                },
                stack_targets: &stack_targets,
                removed_lines: dead
                    .as_ref()
                    .map_or(&[][..], |dead| &dead.removed_lines[..]),
                removed_handlers: dead
                    .as_ref()
                    .map_or(&[][..], |dead| &dead.removed_handlers[..]),
                removed_locals,
                slot: &|slot| match &renumbered {
                    Some(renumbered) => renumbered.slot(slot),
                    None => Some(slot),
                },
                implicit_return: method
                    .implicit_void_return_pc
                    .map(|pc| offsets.partition_point(|&at| at < usize::from(pc)).min(n)),
            },
            &pool,
        )?;
        // A short branch that no longer reaches its target, or a body grown past the JVM's limit,
        // fails to lay out; the method is then written as emitted.
        let assembled = relabelled.node.assemble(&mut pool).ok()?;
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
        let implicit_void_return_pc = match relabelled.implicit_return {
            Some(label) => Some(assembled.offset_of(label)?),
            None => None,
        };

        // The class carries the frames the rewritten body implies. A body they cannot be computed
        // for is written as emitted.
        self.compute_frames(&stack_maps::Body {
            access: source.access,
            name: &source.name,
            descriptor: &source.desc,
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

    /// The finished `method` read into a node against the writer's own pool. A local-variable
    /// entry without a start covers the method from its first instruction, one without a length
    /// runs to its end; a range reaching past the code is cut at its end.
    fn finished_node(
        &self,
        method: &MethodInfo,
        source: &RewriteSource,
        bytes: &[u8],
        pool: &PoolLookup<'_>,
    ) -> Option<MethodNode> {
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
        MethodNode::read_code(source.access, &source.name, &source.desc, &code, pool).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::is_expression_null_check;

    #[test]
    fn expression_null_check_identity_includes_its_descriptor() {
        assert!(is_expression_null_check(
            "kotlin/jvm/internal/Intrinsics",
            "checkNotNullExpressionValue",
            "(Ljava/lang/Object;Ljava/lang/String;)V",
        ));
        assert!(!is_expression_null_check(
            "kotlin/jvm/internal/Intrinsics",
            "checkNotNullExpressionValue",
            "(Ljava/lang/Object;)V",
        ));
        assert!(!is_expression_null_check(
            "fixture/Intrinsics",
            "checkNotNullExpressionValue",
            "(Ljava/lang/Object;Ljava/lang/String;)V",
        ));
    }
}
