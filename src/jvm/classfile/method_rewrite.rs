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
//! A rewrite adds no instruction operand. It clears frame slots to `top`, and pushes onto a jump
//! target's frame the type a null check left on the stack; rebuilding the stack map interns that
//! type's class if no frame named it before, after every constant emission interned — where
//! kotlinc's writer also adds the classes its computed frames name.
//!
//! The rewritten body is only kept if the forward frame analysis still accepts it and every edge
//! into a recorded frame agrees with that frame; otherwise the method is written exactly as
//! emitted.

use std::collections::BTreeSet;

use super::bytecode_analysis::{ControlGraph, FrameTypes, Handler, VerificationType};
use super::redundant_gotos;
use super::temporaries::{self, Body};
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

/// What a method keeps so it can be rewritten when its class is written: its builder (for the
/// frames and the labels they are bound to) and the entry frame its stack map compresses against.
#[derive(Clone)]
pub(super) struct RewriteSource {
    pub access: u16,
    pub name: String,
    pub desc: String,
    pub builder: CodeBuilder,
    pub baseline: Option<Vec<VerifType>>,
}

/// A rewritten method's `Code` and every table that moved with it.
struct Rewritten {
    code: Vec<u8>,
    max_stack: u16,
    max_locals: u16,
    exceptions: Vec<(u16, u16, u16, u16)>,
    lnt: Vec<(u16, u16)>,
    lvt: Vec<LvtEntry>,
    implicit_void_return_pc: Option<u16>,
    /// The builder with its labels moved and its frames edited, to rebuild the stack map from.
    frames: CodeBuilder,
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

/// The inverse of [`expand_slots`], with the trailing `top`s a frame omits dropped.
fn compress_slots(slots: &[VerifType]) -> Vec<VerifType> {
    let mut locals = Vec::with_capacity(slots.len());
    let mut slot = 0;
    while slot < slots.len() {
        let local = slots[slot].clone();
        let wide = matches!(local, VerifType::Long | VerifType::Double);
        locals.push(local);
        slot += if wide { 2 } else { 1 };
    }
    while locals.last() == Some(&VerifType::Top) {
        locals.pop();
    }
    locals
}

fn words(value: &VerificationType) -> usize {
    match value {
        VerificationType::Long | VerificationType::Double => 2,
        _ => 1,
    }
}

/// Slots a load, store or `iinc` touches, with their width.
fn var_slot(insn: &Insn) -> Option<(u16, u16)> {
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

fn is_store(insn: &Insn) -> bool {
    match insn {
        Insn::Plain {
            op: 0x36..=0x4e, ..
        } => true,
        Insn::Plain { op: 0xc4, operands } => {
            matches!(operands.first(), Some(0x36..=0x3a))
        }
        _ => false,
    }
}

/// The original instruction indices store `store` (to `slot`) reaches: every instruction a path
/// from it arrives at before another store to `slot`. A recorded frame there typed the slot with
/// this store's value, which a rewrite that eliminated the store no longer provides.
fn reached_by(insns: &[Insn], graph: &ControlGraph, store: usize, slot: u16) -> Vec<bool> {
    let mut reached = vec![false; insns.len() + 1];
    let mut pending: Vec<usize> = graph
        .normal_successors(store)
        .iter()
        .chain(graph.exceptional_successors(store))
        .copied()
        .collect();
    while let Some(index) = pending.pop() {
        if index > insns.len() || reached[index] {
            continue;
        }
        reached[index] = true;
        if index == insns.len() {
            continue;
        }
        let kills =
            is_store(&insns[index]) && var_slot(&insns[index]).map(|(s, _)| s) == Some(slot);
        // An exception raised before the killing store still carries the value into a handler.
        pending.extend(graph.exceptional_successors(index));
        if !kills {
            pending.extend(graph.normal_successors(index));
        }
    }
    reached
}

/// Two stack entries at one rewritten offset that are provably the same verifier value. `null`
/// may meet a reference at that reference. Distinct non-null references are deliberately refused:
/// without the class hierarchy, widening them to `Object` could make a later narrow receiver or
/// argument fail verification.
fn join_stack_entry(a: &VerifType, b: &VerifType, cp: &super::ConstPool) -> Option<VerifType> {
    let reference = |v: &VerifType| matches!(v, VerifType::Object(_) | VerifType::ObjectName(_));
    if super::verif_eq(a, b, cp) {
        Some(a.clone())
    } else if *a == VerifType::Null && reference(b) {
        Some(b.clone())
    } else if *b == VerifType::Null && reference(a) {
        Some(a.clone())
    } else {
        None
    }
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
    /// Give every frame bound at one offset the same stack, joined entry by entry; `None` when
    /// their heights differ or an entry has no join.
    fn unify_stacks_at_shared_offsets(&self, frames: &mut CodeBuilder) -> Option<()> {
        let pcs: Vec<Option<usize>> = frames
            .frames
            .iter()
            .map(|(label, _, _)| {
                frames
                    .labels
                    .get(*label as usize)
                    .copied()
                    .filter(|&pc| pc != usize::MAX)
            })
            .collect();
        for (first, pc) in pcs.iter().enumerate() {
            let Some(pc) = *pc else {
                continue;
            };
            let sharing: Vec<usize> = (first..pcs.len()).filter(|&n| pcs[n] == Some(pc)).collect();
            if sharing.len() < 2 || pcs[..first].contains(&Some(pc)) {
                continue;
            }
            let mut stack = frames.frames[first].2.clone();
            for &n in &sharing[1..] {
                let other = &frames.frames[n].2;
                if other.len() != stack.len() {
                    return None;
                }
                for (entry, theirs) in stack.iter_mut().zip(other) {
                    *entry = join_stack_entry(entry, theirs, &self.cp)?;
                }
            }
            for &n in &sharing {
                frames.frames[n].2.clone_from(&stack);
            }
        }
        Some(())
    }

    /// Apply kotlinc's bytecode rewrites to every method, now that each one's tables are final.
    pub(super) fn rewrite_methods(&mut self) {
        for index in 0..self.methods.len() {
            let Some(source) = self.methods[index].rewrite_source.take() else {
                continue;
            };
            let Some(rewritten) = self.rewritten(&self.methods[index], &source) else {
                continue;
            };
            let stackmap = if rewritten.frames.has_frames() {
                rewritten
                    .frames
                    .build_stackmap(source.baseline.as_deref(), &mut self.cp)
            } else {
                None
            };
            let method = &mut self.methods[index];
            method.code = Some(rewritten.code);
            method.max_stack = rewritten.max_stack;
            method.max_locals = rewritten.max_locals;
            method.exceptions = rewritten.exceptions;
            method.lnt = rewritten.lnt;
            method.lvt = rewritten.lvt;
            method.implicit_void_return_pc = rewritten.implicit_void_return_pc;
            method.stackmap = stackmap;
            crate::trace_compiler!("bytecode", "rewrote {}{}", source.name, source.desc);
        }
    }

    /// `method` after kotlinc's rewrites, or `None` when none applies or the rewritten body could
    /// not be proven to keep its frames.
    fn rewritten(&self, method: &MethodInfo, source: &RewriteSource) -> Option<Rewritten> {
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
            eliminated: Vec::new(),
            stack_at_target: Vec::new(),
            late_labels: std::collections::BTreeSet::new(),
        });
        let protected_starts: Vec<usize> = handlers.iter().map(|handler| handler.start).collect();
        let rewrite_late = rewrite.late_labels.clone();
        let gotos_changed = redundant_gotos::remove(
            &mut rewrite.nodes,
            &redundant_gotos::Tables {
                lines: &lines,
                variable_bounds: &variable_bounds,
                protected_starts: &protected_starts,
                late_branch: &|index| {
                    branch_labels
                        .get(index)
                        .copied()
                        .flatten()
                        .is_some_and(|label| rewrite_late.contains(&label))
                },
            },
        );
        if !folded_any && !gotos_changed {
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

        // A null check that now keeps its value on the stack leaves it there at the jump target:
        // that target's frame gains it, typed as the checked local was at each check.
        let original_graph = ControlGraph::build(&insns, &handlers)?;
        let mut pushed: Vec<(usize, VerifType)> = Vec::new();
        if !rewrite.stack_at_target.is_empty() {
            let original_frames = self
                .merged_frames(&source.builder)
                .into_iter()
                .map(|(at, locals, stack)| Some((index_of(at)?, expand_slots(&locals), stack)))
                .collect::<Option<Vec<_>>>()?;
            let types =
                FrameTypes::analyze(&insns, &original_graph, &entry, &original_frames, self)?;
            for (target, loads) in &rewrite.stack_at_target {
                let mut value: Option<VerificationType> = None;
                for &load in loads {
                    let (slot, _) = var_slot(&insns[load])?;
                    let local = types.before(load)?.local(slot).clone();
                    value = Some(match value {
                        Some(value) => value.join(&local),
                        None => local,
                    });
                }
                let value = value?;
                if !matches!(
                    value,
                    VerificationType::Reference(_) | VerificationType::Null
                ) {
                    return None;
                }
                pushed.push((offsets[*target], value.to_verif()));
            }
        }

        // An eliminated store's slot is `top` in every recorded frame its value reached: those frames
        // typed the slot with a value the rewritten body no longer stores.
        let reach: Vec<(u16, Vec<bool>)> = rewrite
            .eliminated
            .iter()
            .map(|&(store, slot)| (slot, reached_by(&insns, &original_graph, store, slot)))
            .collect();
        let mut frames = source.builder.clone();
        let mut received = vec![false; pushed.len()];
        for (label, locals, stack) in &mut frames.frames {
            let Some(&pc) = source.builder.labels.get(*label as usize) else {
                continue;
            };
            for (n, (at, value)) in pushed.iter().enumerate() {
                if *at == pc && !is_late_label(*label) {
                    stack.push(value.clone());
                    received[n] = true;
                }
            }
            let Some(index) = index_of(pc) else {
                continue;
            };
            let mut slots = expand_slots(locals);
            let mut edited = false;
            for (slot, reached) in &reach {
                let slot = usize::from(*slot);
                if reached[index] && slot < slots.len() && slots[slot] != VerifType::Top {
                    slots[slot] = VerifType::Top;
                    edited = true;
                }
            }
            if edited {
                *locals = compress_slots(&slots);
            }
        }
        // A target with no recorded frame has nothing to carry the value: keep the method as it was.
        if received.contains(&false) {
            return None;
        }
        frames.bytes = assemble(&new_insns);
        frames.fixups.clear();
        frames.switch_fixups.clear();
        for (label, pc) in frames.labels.iter_mut().enumerate() {
            if *pc == usize::MAX {
                continue;
            }
            *pc = if is_late_label(label as u32) {
                let k = offsets.partition_point(|&at| at < *pc).min(n);
                new_offsets[late_index[k]]
            } else {
                map(*pc)
            };
        }
        // A removed reload can leave its jump target and the join after it at one offset, each with
        // its own frame: the target's carries the checked value's type, the join's whatever every
        // path into it agreed on. Both describe the one point now, so each stack entry is what they
        // all accept.
        if !pushed.is_empty() {
            self.unify_stacks_at_shared_offsets(&mut frames)?;
        }
        let exceptions: Vec<(u16, u16, u16, u16)> = method
            .exceptions
            .iter()
            .map(|&(start, end, handler, catch)| (map16(start), map16(end), map16(handler), catch))
            .collect();
        if exceptions.iter().any(|&(start, end, _, _)| start >= end) {
            return None;
        }
        let lnt: Vec<(u16, u16)> = method
            .lnt
            .iter()
            .map(|&(pc, line)| (map_after_inserted16(pc), line))
            .collect();
        let lvt: Vec<LvtEntry> = method
            .lvt
            .iter()
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
        if lvt.iter().any(|&(_, _, _, _, len)| len == Some(0)) {
            // Kotlin's complete optimizer removes unused/empty LVT entries. This focused rewrite
            // does not own debug-local deletion, so preserve the original method instead.
            return None;
        }

        let new_handlers: Vec<Handler> = exceptions
            .iter()
            .map(|&(start, end, handler, _)| {
                let at = |pc: u16| new_offsets.binary_search(&usize::from(pc)).ok();
                Some(Handler {
                    start: at(start)?,
                    end: at(end)?,
                    handler: at(handler)?,
                })
            })
            .collect::<Option<_>>()?;
        let new_graph = ControlGraph::build(&new_insns, &new_handlers)?;
        // kotlinc's writer puts a frame only where a jump, a switch or a handler arrives. A label a
        // rewrite left reached only by falling through (its `goto` removed, its jumps threaded on)
        // loses its frame; one left in dead code keeps it, since the verifier still checks it.
        let mut targeted = vec![false; new_insns.len() + 1];
        for insn in &new_insns {
            match insn {
                Insn::Branch {
                    target: BranchTarget::Internal(to),
                    ..
                }
                | Insn::BranchW {
                    target: BranchTarget::Internal(to),
                    ..
                } => targeted[*to] = true,
                Insn::TableSwitch {
                    default, targets, ..
                } => {
                    for &to in std::iter::once(default).chain(targets) {
                        targeted[to] = true;
                    }
                }
                Insn::LookupSwitch { default, pairs } => {
                    for &to in std::iter::once(default).chain(pairs.iter().map(|(_, to)| to)) {
                        targeted[to] = true;
                    }
                }
                _ => {}
            }
        }
        for handler in &new_handlers {
            targeted[handler.handler] = true;
        }
        let ends_flow = |insn: &Insn| match insn {
            Insn::Branch { op, .. } | Insn::BranchW { op, .. } => matches!(*op, 0xa7 | 0xc8),
            Insn::Plain { op, .. } => matches!(*op, 0xac..=0xb1 | 0xbf),
            Insn::TableSwitch { .. } | Insn::LookupSwitch { .. } => true,
        };
        let labels = frames.labels.clone();
        frames.frames.retain(|(label, _, _)| {
            let Some(&pc) = labels.get(*label as usize) else {
                return true;
            };
            match new_offsets.binary_search(&pc) {
                Ok(at) if at < new_insns.len() => {
                    targeted[at] || (at > 0 && ends_flow(&new_insns[at - 1]))
                }
                _ => true,
            }
        });
        let merged = self
            .merged_frames(&frames)
            .into_iter()
            .filter(|(at, _, _)| *at < new_len)
            .map(|(at, locals, stack)| {
                Some((
                    new_offsets.binary_search(&at).ok()?,
                    expand_slots(&locals),
                    stack,
                ))
            })
            .collect::<Option<Vec<_>>>()?;
        let types = FrameTypes::analyze(&new_insns, &new_graph, &entry, &merged, self)?;
        if !types.frames_hold(&new_insns, &new_graph, &merged, self) {
            return None;
        }
        let mut max_stack = 0usize;
        for index in 0..=new_insns.len() {
            if let Some(state) = types.before(index) {
                max_stack = max_stack.max(state.stack.iter().map(words).sum());
            }
        }
        let mut max_locals = entry.len();
        for insn in &new_insns {
            if let Some((slot, width)) = var_slot(insn) {
                max_locals = max_locals.max(usize::from(slot) + usize::from(width));
            }
        }
        for &(_, desc, slot, _, _) in &lvt {
            let width = match self.cp.utf8_at(desc) {
                Some("J" | "D") => 2,
                _ => 1,
            };
            max_locals = max_locals.max(usize::from(slot) + width);
        }
        for (_, locals, _) in &frames.frames {
            max_locals = max_locals.max(expand_slots(locals).len());
        }
        Some(Rewritten {
            code: frames.bytes.clone(),
            max_stack: u16::try_from(max_stack).ok()?,
            max_locals: u16::try_from(max_locals).ok()?,
            exceptions,
            lnt,
            lvt,
            implicit_void_return_pc: method.implicit_void_return_pc.map(map_after_inserted16),
            frames,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{is_expression_null_check, join_stack_entry, short_branches_fit};
    use crate::jvm::classfile::{ConstPool, VerifType};
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

    #[test]
    fn shared_offsets_do_not_widen_distinct_references_without_a_hierarchy() {
        let pool = ConstPool::default();
        assert!(join_stack_entry(
            &VerifType::ObjectName("java/lang/String".to_owned()),
            &VerifType::ObjectName("java/lang/CharSequence".to_owned()),
            &pool,
        )
        .is_none());
        assert!(matches!(
            join_stack_entry(
                &VerifType::Null,
                &VerifType::ObjectName("java/lang/CharSequence".to_owned()),
                &pool,
            ),
            Some(VerifType::ObjectName(name)) if name == "java/lang/CharSequence"
        ));
    }
}
