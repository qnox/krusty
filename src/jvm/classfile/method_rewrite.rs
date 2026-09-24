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

use std::collections::BTreeSet;

use super::bytecode_analysis::{ControlGraph, FrameTypes, Handler, VerificationType};
use super::temporaries::{self, Body};
use super::{dead_code, local_slots, negated_jumps, redundant_checkcasts, redundant_gotos};
use super::{stack_maps, stack_peephole};
use super::{ClassWriter, CodeBuilder, LvtEntry, MethodInfo, VerifType};
use crate::jvm::inline::{assemble, disassemble, insn_offsets_at, BranchTarget, Insn};

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

/// Whether every short branch still reaches its target after a rewrite. [`assemble`] encodes these
/// operands as signed 16-bit deltas, so accepting a larger delta would wrap and corrupt the method.
fn short_branches_fit(insns: &[Insn], offsets: &[usize]) -> bool {
    insns.iter().enumerate().all(|(at, insn)| match insn {
        Insn::Branch {
            target: BranchTarget::Internal(target),
            ..
        } => offsets.get(*target).is_some_and(|target_offset| {
            i16::try_from(*target_offset as isize - offsets[at] as isize).is_ok()
        }),
        Insn::Branch { .. } => false,
        _ => true,
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
        let code_len = bytes.len();
        let insns = disassemble(bytes)?;
        let offsets = insn_offsets_at(&insns, 0);
        if offsets.last().copied() != Some(code_len) {
            return None;
        }
        let index_of = |pc: usize| offsets.binary_search(&pc).ok();
        let n = insns.len();
        let handlers: Vec<Handler> = method
            .exceptions
            .iter()
            .map(|&(start, end, handler, _)| {
                Some(Handler {
                    start: index_of(usize::from(start))?,
                    end: index_of(usize::from(end))?,
                    handler: index_of(usize::from(handler))?,
                })
            })
            .collect::<Option<_>>()?;
        let mut arrivals = vec![false; n + 1];
        for insn in &insns {
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
        for handler in &handlers {
            arrivals[handler.start] = true;
            arrivals[handler.end] = true;
            arrivals[handler.handler] = true;
        }
        let mut marks = vec![false; n + 1];
        let mut lines = vec![false; n + 1];
        for &(pc, _) in &method.lnt {
            let at = index_of(usize::from(pc))?;
            marks[at] = true;
            lines[at] = true;
        }
        let mut variable_bounds = vec![false; n + 1];
        // A local-variable entry without a start covers the whole method; one without a length
        // runs to the end.
        let range = |start: Option<u16>, len: Option<u16>| {
            let start = start.map_or(0, usize::from);
            let end = len.map_or(code_len, |len| start + usize::from(len));
            (start, end.min(code_len))
        };
        let mut named = Vec::new();
        for &(_, _, slot, start, len) in &method.lvt {
            let (start, end) = range(start, len);
            if start >= code_len {
                continue;
            }
            let (start, end) = (index_of(start)?, index_of(end)?);
            marks[start] = true;
            marks[end] = true;
            variable_bounds[start] = true;
            variable_bounds[end] = true;
            named.push((start, end, slot));
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
        let original_graph = ControlGraph::build(&insns, &handlers)?;
        // The verifier's types before each original instruction, computed at most once. Rewrite
        // selection must follow the instruction graph itself: emitter-recorded frames are migration
        // input and can be more precise than the state the final bytecode actually proves (for
        // example, a smart-cast fact after the value has been stored in a broader local). Using
        // those frames to remove a `checkcast` can therefore make a valid body unverifiable.
        let flow_types_cell = std::cell::OnceCell::new();
        let flow_types = || {
            flow_types_cell
                .get_or_init(|| FrameTypes::analyze(&insns, &original_graph, &entry, &[], self))
                .as_ref()
        };
        // Recorded frames remain only as a compatibility certificate for accepting reference
        // widenings while the rewrite-frame migration is completed by the next stack stage.
        let original_analysis_cell = std::cell::OnceCell::new();
        let original_analysis = || {
            original_analysis_cell
                .get_or_init(|| {
                    let original_frames = self
                        .compute_frames(&stack_maps::Body {
                            access: source.access,
                            name: &source.name,
                            descriptor: &source.desc,
                            code: bytes,
                            exceptions: &method.exceptions,
                            labels: stack_maps::table_labels(&method.lnt, &method.lvt, code_len),
                        })
                        .ok()?
                        .frames()
                        .iter()
                        .map(|frame| {
                            let locals: Vec<VerifType> = frame
                                .locals
                                .iter()
                                .map(VerificationType::to_verif)
                                .collect();
                            let stack =
                                frame.stack.iter().map(VerificationType::to_verif).collect();
                            (frame.index, expand_slots(&locals), stack)
                        })
                        .collect::<Vec<_>>();
                    let types = FrameTypes::analyze(
                        &insns,
                        &original_graph,
                        &entry,
                        &original_frames,
                        self,
                    )?;
                    Some((types, original_frames))
                })
                .as_ref()
        };
        let redundant_casts = redundant_checkcasts::select(self, &insns, || flow_types());
        // kotlinc's `RedundantNullCheckMethodTransformer`: a `checkNotNull*` of a value its
        // nullability analysis proves non-null goes (see `null_checks`).
        let redundant_null_checks = self.redundant_null_checks(
            &insns,
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
            insns: &insns,
            handlers: &handlers,
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
        if insns.iter().any(|insn| {
            matches!(
                insn,
                Insn::Branch {
                    target: BranchTarget::External(_),
                    ..
                } | Insn::BranchW {
                    target: BranchTarget::External(_),
                    ..
                }
            )
        }) {
            return None;
        }
        let folded = temporaries::eliminate(&body);
        let folded_any = folded.is_some();
        let mut rewrite = folded.unwrap_or_else(|| temporaries::Rewrite {
            nodes: insns
                .iter()
                .enumerate()
                .map(|(index, insn)| (insn.clone(), temporaries::Placement::Original(index)))
                .collect(),
            stack_at_target: Vec::new(),
            late_labels: std::collections::BTreeSet::new(),
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
        for handler in &handlers {
            for at in [handler.start, handler.end, handler.handler] {
                labelled[at] = true;
            }
        }
        let jumps_negated = negated_jumps::negate(&mut rewrite.nodes, &labelled, &|index| {
            branch_labels
                .get(index)
                .copied()
                .flatten()
                .is_some_and(|label| rewrite_late.contains(&label))
        });
        // kotlinc always ends with `DeadCodeEliminationMethodTransformer`: whatever the passes
        // above left unreachable goes, with its line numbers, empty protected ranges and emptied
        // local variables (see `dead_code`).
        let lines_at: Vec<(usize, u16)> = method
            .lnt
            .iter()
            .map(|&(pc, line)| Some((index_of(usize::from(pc))?, line)))
            .collect::<Option<_>>()?;
        let local_ranges: Vec<Option<(usize, usize)>> = method
            .lvt
            .iter()
            .map(|&(_, _, _, start, len)| {
                let (start, end) = range(start, len);
                (start < code_len)
                    .then(|| Some((index_of(start)?, index_of(end)?)))
                    .flatten()
            })
            .collect();
        let dead = dead_code::eliminate(
            &mut rewrite.nodes,
            &dead_code::Flow {
                handlers: &handlers,
                late_branch: &late_branch,
                lines: &lines_at,
                line_after_inserted: &|index| {
                    rewrite
                        .stack_at_target
                        .iter()
                        .any(|(target, _)| *target == index)
                },
                locals: &local_ranges,
            },
        );
        let removed = |table: fn(&dead_code::Elimination) -> &[bool], at: usize| {
            dead.as_ref().is_some_and(|dead| table(dead)[at])
        };
        let mut fixed_slots: BTreeSet<u16> = (0..u16::try_from(entry.len()).ok()?).collect();
        for (at, &(_, desc, slot, _, _)) in method.lvt.iter().enumerate() {
            if removed(|dead| &dead.removed_locals, at) {
                continue;
            }
            fixed_slots.insert(slot);
            if matches!(self.cp.utf8_at(desc), Some("J" | "D")) {
                fixed_slots.insert(slot + 1);
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
        // Every original index `k` now starts at the first rewritten instruction of group `k` or a
        // later one — where a label that stood at `k` lands.
        let mut new_index = vec![rewrite.nodes.len(); n + 1];
        let mut next = 0;
        for (k, slot) in new_index.iter_mut().enumerate().take(n) {
            while next < rewrite.nodes.len() && rewrite.nodes[next].1.group() < k {
                next += 1;
            }
            *slot = next;
        }
        // A late label stands after the instructions a rule inserted in front of its index's group.
        let mut late_index = vec![rewrite.nodes.len(); n + 1];
        let mut next = 0;
        for (k, slot) in late_index.iter_mut().enumerate().take(n) {
            while next < rewrite.nodes.len()
                && (rewrite.nodes[next].1.group() < k
                    || rewrite.nodes[next].1 == temporaries::Placement::Before(k))
            {
                next += 1;
            }
            *slot = next;
        }
        let debug_after_insert: BTreeSet<usize> = rewrite
            .stack_at_target
            .iter()
            .filter_map(|(target, _)| {
                (new_index[*target] != late_index[*target]).then_some(*target)
            })
            .collect();
        let is_late_label = |label: u32| rewrite.late_labels.contains(&label);
        let retarget = |to: usize| new_index[to];
        let retarget_branch = |placement: temporaries::Placement, to: usize| match placement {
            temporaries::Placement::Original(index)
                if branch_labels
                    .get(index)
                    .copied()
                    .flatten()
                    .is_some_and(is_late_label) =>
            {
                late_index[to]
            }
            _ => new_index[to],
        };
        let new_insns: Vec<Insn> = rewrite
            .nodes
            .iter()
            .map(|(insn, placement)| match insn {
                Insn::Branch {
                    op,
                    target: BranchTarget::Internal(to),
                } => Insn::Branch {
                    op: *op,
                    target: BranchTarget::Internal(retarget_branch(*placement, *to)),
                },
                Insn::BranchW {
                    op,
                    target: BranchTarget::Internal(to),
                } => Insn::BranchW {
                    op: *op,
                    target: BranchTarget::Internal(retarget_branch(*placement, *to)),
                },
                Insn::TableSwitch {
                    default,
                    low,
                    targets,
                } => Insn::TableSwitch {
                    default: retarget(*default),
                    low: *low,
                    targets: targets.iter().map(|&to| retarget(to)).collect(),
                },
                Insn::LookupSwitch { default, pairs } => Insn::LookupSwitch {
                    default: retarget(*default),
                    pairs: pairs.iter().map(|&(key, to)| (key, retarget(to))).collect(),
                },
                other => other.clone(),
            })
            .collect();
        let new_offsets = insn_offsets_at(&new_insns, 0);
        let new_len = new_offsets[new_insns.len()];
        if new_len > usize::from(u16::MAX) || !short_branches_fit(&new_insns, &new_offsets) {
            return None;
        }
        // An original offset maps to where the instruction that began there now begins; a removed
        // instruction's offset to whatever follows it, as a label in front of it would.
        let map = |pc: usize| -> usize {
            let k = offsets.partition_point(|&at| at < pc);
            new_offsets[new_index[k.min(n)]]
        };
        let map16 = |pc: u16| map(usize::from(pc)) as u16;
        // A debug boundary and the implicit return at an `ifnull` fold's target describe the
        // original instruction/range boundary after its labels. They therefore belong after the
        // `pop` inserted in front of that original instruction. Other `Before(k)` insertions (the
        // `dup` beside a branch) do not move a table boundary at `k`.
        let map_after_inserted = |pc: usize| -> usize {
            let k = offsets.partition_point(|&at| at < pc).min(n);
            if debug_after_insert.contains(&k) {
                new_offsets[late_index[k]]
            } else {
                map(pc)
            }
        };
        let map_after_inserted16 = |pc: u16| map_after_inserted(usize::from(pc)) as u16;

        let exceptions: Vec<(u16, u16, u16, u16)> = method
            .exceptions
            .iter()
            .enumerate()
            .filter(|&(at, _)| !removed(|dead| &dead.removed_handlers, at))
            .map(|(_, entry)| entry)
            .map(|&(start, end, handler, catch)| (map16(start), map16(end), map16(handler), catch))
            .collect();
        if exceptions.iter().any(|&(start, end, _, _)| start >= end) {
            return None;
        }
        let lnt: Vec<(u16, u16)> = method
            .lnt
            .iter()
            .enumerate()
            .filter(|&(at, _)| !removed(|dead| &dead.removed_lines, at))
            .map(|(_, entry)| entry)
            .map(|&(pc, line)| (map_after_inserted16(pc), line))
            .collect();
        let mut lvt: Vec<LvtEntry> = method
            .lvt
            .iter()
            .enumerate()
            .filter(|&(at, _)| !removed(|dead| &dead.removed_locals, at))
            .map(|(_, entry)| entry)
            .map(|&(name, desc, slot, old_start, old_len)| {
                let start = old_start.map(map_after_inserted16);
                let len = old_len.map(|old_len| {
                    let end =
                        map_after_inserted(old_start.map_or(0, usize::from) + usize::from(old_len));
                    (end - start.map_or(0, usize::from)) as u16
                });
                (name, desc, slot, start, len)
            })
            .collect();
        if let Some(renumbered) = &renumbered {
            for entry in &mut lvt {
                entry.2 = renumbered.slot(entry.2)?;
            }
        }
        if lvt.iter().any(|&(_, _, _, _, len)| len == Some(0)) {
            // Kotlin's complete optimizer removes unused/empty LVT entries. This focused rewrite
            // does not own debug-local deletion, so preserve the original method instead.
            return None;
        }

        // The class carries the frames the rewritten body implies. A body they cannot be computed
        // for is written as emitted.
        let code = assemble(&new_insns);
        self.compute_frames(&stack_maps::Body {
            access: source.access,
            name: &source.name,
            descriptor: &source.desc,
            code: &code,
            exceptions: &exceptions,
            labels: stack_maps::table_labels(&lnt, &lvt, code.len()),
        })
        .ok()?;
        Some(Rewritten {
            code,
            exceptions,
            lnt,
            lvt,
            implicit_void_return_pc: method.implicit_void_return_pc.map(map_after_inserted16),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{is_expression_null_check, short_branches_fit};
    use crate::jvm::inline::{insn_offsets_at, BranchTarget, Insn};

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

    #[test]
    fn a_rewrite_declines_a_short_branch_that_grows_out_of_range() {
        fn body(nops: usize) -> Vec<Insn> {
            let target = nops + 1;
            let mut insns = vec![Insn::Branch {
                op: 0xa7,
                target: BranchTarget::Internal(target),
            }];
            insns.extend((0..nops).map(|_| Insn::Plain {
                op: 0x00,
                operands: Vec::new(),
            }));
            insns.push(Insn::Plain {
                op: 0xb1,
                operands: Vec::new(),
            });
            insns
        }

        let at_limit = body(32_764);
        assert!(short_branches_fit(
            &at_limit,
            &insn_offsets_at(&at_limit, 0)
        ));
        let too_far = body(32_765);
        assert!(!short_branches_fit(&too_far, &insn_offsets_at(&too_far, 0)));
    }
}
