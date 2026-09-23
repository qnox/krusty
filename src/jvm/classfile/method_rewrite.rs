//! Bytecode rewrites applied to a finished method when its class is written.
//!
//! kotlinc does not write its temporaries onto the operand stack while generating code; it writes
//! them as locals and lets a bytecode pass fold them (see [`crate::jvm::temporaries`]). This is the
//! place krusty does the same. It runs when the class is written, because only then is every table
//! final: several line and local-variable tables are attached after a method is added, and which
//! values are temporaries depends on them. The method's instructions are decoded, rewritten and
//! re-assembled, and every table keyed by a byte offset — labels (and so frames and exception
//! ranges), line numbers, local ranges, the implicit return — moves with the instruction it
//! described.
//!
//! A rewrite adds no constant and only clears frame slots to `top`, so rebuilding the stack map
//! interns nothing new: the constant pool is exactly what emission made it.
//!
//! The rewritten body is only kept if the forward frame analysis still accepts it; otherwise the
//! method is written exactly as emitted.

use super::{ClassWriter, CodeBuilder, LvtEntry, MethodInfo, VerifType};
use crate::jvm::inline::{assemble, disassemble, insn_offsets_at, BranchTarget, Insn};
use crate::jvm::suspend::cps::{ControlGraph, FrameTypes, Handler, VerificationType};
use crate::jvm::temporaries::{self, Body};

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
        for &(pc, _) in &method.lnt {
            marks[index_of(usize::from(pc))?] = true;
        }
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
            named.push((start, end, slot));
        }
        let body = Body {
            insns: &insns,
            handlers: &handlers,
            arrivals: &arrivals,
            marks: &marks,
            named: &named,
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
                    .is_some_and(|(owner, name, _)| {
                        owner == "kotlin/jvm/internal/Intrinsics"
                            && matches!(
                                name,
                                "checkNotNullExpressionValue" | "checkExpressionValueIsNotNull"
                            )
                    })
            },
        };
        let rewrite = temporaries::eliminate(&body)?;
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
        let retarget = |to: usize| new_index[to];
        let new_insns: Vec<Insn> = rewrite
            .nodes
            .iter()
            .map(|(insn, _)| match insn {
                Insn::Branch {
                    op,
                    target: BranchTarget::Internal(to),
                } => Insn::Branch {
                    op: *op,
                    target: BranchTarget::Internal(retarget(*to)),
                },
                Insn::BranchW {
                    op,
                    target: BranchTarget::Internal(to),
                } => Insn::BranchW {
                    op: *op,
                    target: BranchTarget::Internal(retarget(*to)),
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
        // An original offset maps to where the instruction that began there now begins; a removed
        // instruction's offset to whatever follows it, as a label in front of it would.
        let map = |pc: usize| -> usize {
            let k = offsets.partition_point(|&at| at < pc);
            new_offsets[new_index[k.min(n)]]
        };
        let map16 = |pc: u16| map(usize::from(pc)) as u16;

        // An eliminated store's slot is `top` in every recorded frame its value reached: those frames
        // typed the slot with a value the rewritten body no longer stores.
        let original_graph = ControlGraph::build(&insns, &handlers)?;
        let reach: Vec<(u16, Vec<bool>)> = rewrite
            .eliminated
            .iter()
            .map(|&(store, slot)| (slot, reached_by(&insns, &original_graph, store, slot)))
            .collect();
        let mut frames = source.builder.clone();
        for (label, locals, _) in &mut frames.frames {
            let Some(&pc) = source.builder.labels.get(*label as usize) else {
                continue;
            };
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
        frames.bytes = assemble(&new_insns);
        frames.fixups.clear();
        frames.switch_fixups.clear();
        for label in &mut frames.labels {
            if *label != usize::MAX {
                *label = map(*label);
            }
        }
        let exceptions: Vec<(u16, u16, u16, u16)> = method
            .exceptions
            .iter()
            .map(|&(start, end, handler, catch)| (map16(start), map16(end), map16(handler), catch))
            .collect();
        let lnt: Vec<(u16, u16)> = method
            .lnt
            .iter()
            .map(|&(pc, line)| (map16(pc), line))
            .collect();
        let lvt: Vec<LvtEntry> = method
            .lvt
            .iter()
            .map(|&(name, desc, slot, old_start, old_len)| {
                let start = old_start.map(map16);
                let len = old_len.map(|old_len| {
                    let end = map(old_start.map_or(0, usize::from) + usize::from(old_len));
                    (end - start.map_or(0, usize::from)) as u16
                });
                (name, desc, slot, start, len)
            })
            .collect();

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
        let mut max_stack = 0usize;
        for index in 0..=new_insns.len() {
            if let Some(state) = types.before(index) {
                max_stack = max_stack.max(state.stack.iter().map(words).sum());
            }
        }
        let mut max_locals = entry.len();
        for insn in &new_insns {
            if let Some((slot, width)) = var_slot(insn) {
                max_locals = max_locals.max(usize::from(slot + width));
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
            implicit_void_return_pc: method.implicit_void_return_pc.map(map16),
            frames,
        })
    }
}
