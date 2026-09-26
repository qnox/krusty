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
//! A rewrite edits no frame: the class carries the frames the rewritten body implies, computed when
//! the class is written (see [`super::stack_maps`]). The rewritten body is only kept if those frames
//! can be computed; otherwise the method is written as emitted, less the local variables whose range
//! holds no instruction, which kotlinc's `prepareForEmitting` drops from every method it writes (a
//! rewritten body loses them in its final dead-code step). A constant the rewritten
//! body names that the pool lacks (a `Ref` element's descriptor, an unboxing call) is interned and
//! the body laid out again. Where each of the method's constants lands in the pool, and which of
//! its emitted constants no longer belong there, is settled once the class is serialized (see
//! [`super::pool_layout`]), from the [`RelaidMethod`]s the rewrites report.

mod finished_node;

use super::constant_pool_queries::{PoolLookup, Wanted};
use super::pool_layout::RelaidMethod;
use super::stack_maps;
use super::{ClassWriter, CodeBuilder, LvtEntry, MethodInfo, VerifType};
use crate::jvm::bytecode_passes::pipeline::{self, Outcome, PassContext};
use crate::jvm::inline::Insn;
use crate::jvm::method_node::{ConstantSink, LabelId, MethodNode};
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
    /// The pool's size once the method was added: the entries past the previous method's are the
    /// ones its emission and addition interned.
    pub pool_end: Option<u16>,
}

impl RewriteSource {
    pub(super) fn new(access: u16, name: &str, desc: &str, builder: &CodeBuilder) -> Box<Self> {
        Box::new(RewriteSource {
            access,
            name: name.to_string(),
            desc: desc.to_string(),
            builder: builder.clone(),
            decided: None,
            pool_end: None,
        })
    }
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

    /// Drop every local variable whose range holds no instruction, as kotlinc's
    /// `prepareForEmitting` does for every method it writes. A rewritten body has had them dropped
    /// by its final dead-code step; this is that step for a method written as emitted.
    fn drop_empty_locals(&mut self) {
        let code_len = self.code.as_ref().map_or(0, Vec::len);
        self.lvt.retain(|&(_, _, _, start, len)| {
            let start = usize::from(start.unwrap_or(0));
            let end = len.map_or(code_len, |len| start + usize::from(len));
            start < end.min(code_len)
        });
    }
}

impl ClassWriter {
    /// Apply kotlinc's bytecode rewrites to every method, now that each one's tables are final, and
    /// report the methods rewritten with the pool entries each one interned.
    pub(super) fn rewrite_methods(&mut self) -> Vec<RelaidMethod> {
        let mut relaid = Vec::new();
        let mut previous_end = 0;
        for index in 0..self.methods.len() {
            let Some(mut source) = self.methods[index].rewrite_source.take() else {
                self.methods[index].drop_empty_locals();
                continue;
            };
            let added_after = previous_end;
            previous_end = source.pool_end.unwrap_or(previous_end);
            let decided = source
                .decided
                .take()
                .filter(|decided| decided.inputs == RewriteInputs::of(&self.methods[index]));
            let interned_after = self.cp.slot_count();
            let rewritten = match decided {
                Some(decided) => decided.outcome,
                None => self.rewrite_interning(index, &source).0,
            };
            let Some(rewritten) = rewritten else {
                self.methods[index].drop_empty_locals();
                continue;
            };
            self.methods[index].take_rewritten(rewritten);
            relaid.push(RelaidMethod {
                index,
                added: added_after + 1..previous_end.max(added_after) + 1,
                interned: interned_after + 1..self.cp.slot_count() + 1,
            });
            crate::trace_compiler!("bytecode", "rewrote {}{}", source.name, source.desc);
        }
        relaid
    }

    /// The rewrite of the method at `index`, remembered for when the class is written. The
    /// rewrite reads the method's tables, which can still change until then, and the constant
    /// pool, which only grows: a constant it found keeps its index, so an outcome that found every
    /// constant it looked for holds while the tables do.
    pub(super) fn remembered_rewrite(&mut self, index: usize) -> Option<Rewritten> {
        let mut source = self.methods[index].rewrite_source.take()?;
        let (outcome, complete) = self.rewrite_interning(index, &source);
        if complete {
            source.decided = Some(Decided {
                inputs: RewriteInputs::of(&self.methods[index]),
                outcome: outcome.clone(),
            });
        }
        self.methods[index].rewrite_source = Some(source);
        outcome
    }

    /// The rewrite of the method at `index` from `source`, and whether it found every constant it
    /// looked for. The constants a first layout wanted are interned and the method rewritten once
    /// more against the grown pool; if that rewrite is then declined, the entries are left for
    /// [`super::pool_layout`] to drop.
    fn rewrite_interning(
        &mut self,
        index: usize,
        source: &RewriteSource,
    ) -> (Option<Rewritten>, bool) {
        let mut pool = PoolLookup::new(&self.cp, &self.bootstrap_methods);
        let outcome = self.rewritten_with(&self.methods[index], source, &mut pool);
        let wanted = match pool.into_wanted() {
            Some(wanted) if wanted.is_empty() => return (outcome, true),
            Some(wanted) => wanted,
            None => {
                crate::trace_compiler!(
                    "bytecode",
                    "{}.{}{} names a call site the pool lacks",
                    self.internal_name,
                    source.name,
                    source.desc
                );
                return (outcome, false);
            }
        };
        crate::trace_compiler!(
            "bytecode",
            "{}.{}{} interns {} constants",
            self.internal_name,
            source.name,
            source.desc,
            wanted.len()
        );
        self.intern_wanted(&wanted);
        let mut pool = PoolLookup::new(&self.cp, &self.bootstrap_methods);
        let outcome = self.rewritten_with(&self.methods[index], source, &mut pool);
        if outcome.is_none() {
            crate::trace_compiler!(
                "bytecode",
                "{}.{}{} is written as emitted after interning",
                self.internal_name,
                source.name,
                source.desc
            );
        }
        (outcome, !pool.missed())
    }

    fn intern_wanted(&mut self, wanted: &[Wanted]) {
        for constant in wanted {
            match constant {
                Wanted::Class(name) => {
                    self.class(name);
                }
                Wanted::Field(owner, name, desc) => {
                    self.field(owner, name, desc);
                }
                Wanted::Method(owner, name, desc, interface) => {
                    self.method(owner, name, desc, *interface);
                }
                Wanted::Constant(constant) => {
                    self.constant(constant);
                }
                Wanted::Utf8(text) => {
                    self.cp.utf8(text);
                }
            }
        }
    }

    /// `method` after kotlinc's rewrites, or `None` when none applies or the rewritten body could
    /// not be proven to keep its frames. Every constant it names is looked up in `pool`.
    fn rewritten_with(
        &self,
        method: &MethodInfo,
        source: &RewriteSource,
        pool: &mut PoolLookup<'_>,
    ) -> Option<Rewritten> {
        let bytes = method.code.as_ref()?;
        let identity = MethodIdentity {
            access: source.access,
            name: &source.name,
            desc: &source.desc,
        };
        if bytes.is_empty() || source.builder.bytes != *bytes {
            self.trace_as_emitted(identity, format_args!("its body is not the builder's"));
            return None;
        }
        let Some(FinishedNode {
            node,
            implicit_return,
        }) = self.finished_node(method, source, bytes, pool)
        else {
            self.trace_as_emitted(identity, format_args!("its body does not read back"));
            return None;
        };
        // The builder's labels and branch fixups name offsets of the emitted bytes, so the node must
        // lay out exactly as emitted.
        if pool.missed() {
            return None;
        }
        if node.assemble(pool).ok().map(|laid| laid.code).as_ref() != Some(bytes) {
            self.trace_as_emitted(
                identity,
                format_args!("its body does not lay out as emitted"),
            );
            return None;
        }
        self.optimized(method, identity, node, implicit_return, pool)
    }

    /// kotlinc's optimizer passes (see [`pipeline`]) over `node`, the body `method` currently holds
    /// (its code, and the tables keyed by its offsets), or `None` when none applies or the result
    /// could not be proven to keep its frames. `implicit_return` labels the method's implicit
    /// `return`, if it has one. The optimized body looks its constants up in `pool`, which records
    /// whether one was missing. This is the class-file boundary around the passes: it supplies the
    /// method's entry state, and lays the result out again with its local-variable table re-keyed
    /// and its frames proven.
    pub(super) fn optimized(
        &self,
        method: &MethodInfo,
        source: MethodIdentity<'_>,
        mut node: MethodNode,
        implicit_return: Option<LabelId>,
        pool: &mut PoolLookup<'_>,
    ) -> Option<Rewritten> {
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
            self.trace_as_emitted(
                source,
                format_args!("its descriptor names a parameter type the verifier lacks"),
            );
            return None;
        }
        let entry = expand_slots(&entry);
        let context = PassContext {
            owner: &self.internal_name,
            value_classes: &*self.value_classes,
            parameter_slots: u16::try_from(entry.len()).ok()?,
        };
        let removed_locals = match pipeline::optimize(&mut node, &context) {
            Outcome::Changed { removed_locals } => removed_locals,
            Outcome::Unchanged => return None,
            Outcome::Declined => {
                self.trace_as_emitted(source, format_args!("a pass declined the body"));
                return None;
            }
        };
        // A short branch that no longer reaches its target, or a body grown past the JVM's limit,
        // fails to lay out; the method is then written as emitted.
        // A layout that wanted a constant is only laid out to learn every constant it wants.
        let assembled = match node.assemble(pool) {
            Ok(assembled) => assembled,
            Err(error) => {
                self.trace_as_emitted(
                    source,
                    format_args!("the optimized body fails to lay out: {error:?}"),
                );
                return None;
            }
        };
        if assembled
            .exception_table
            .iter()
            .any(|&(start, end, _, _)| start >= end)
        {
            self.trace_as_emitted(source, format_args!("a protected range lays out empty"));
            return None;
        }
        // Each kept local keeps its pool entries, and a bound it left open stays open: a missing
        // start is the method's start and a missing length runs to its end.
        let kept_locals = method
            .lvt
            .iter()
            .zip(&removed_locals)
            .filter(|&(_, &removed)| !removed)
            .map(|(entry, _)| entry);
        // A local whose type a rewrite changed (a `Ref` become its element) names its new
        // descriptor, which the pool must hold like every constant the body uses.
        let lvt: Vec<LvtEntry> = kept_locals
            .zip(&assembled.local_variables)
            .map(|(&(name, desc, _, old_start, old_len), local)| {
                let start = old_start.map(|_| local.start_pc);
                let end = local.start_pc + local.length;
                let len = old_len.map(|_| end - start.unwrap_or(0));
                let desc = if self.cp.utf8_at(desc) == Some(local.desc.as_str()) {
                    desc
                } else {
                    pool.descriptor(&local.desc)
                };
                (name, desc, local.slot, start, len)
            })
            .collect();
        if pool.missed() {
            return None;
        }
        let implicit_void_return_pc = match implicit_return {
            Some(label) => match assembled.offset_of(label) {
                Some(pc) => Some(pc),
                None => {
                    self.trace_as_emitted(source, format_args!("its implicit return is gone"));
                    return None;
                }
            },
            None => None,
        };

        // The class carries the frames the rewritten body implies. A body they cannot be computed
        // for is written as emitted.
        if let Err(decline) = self.compute_frames(&stack_maps::Body {
            access: source.access,
            name: source.name,
            descriptor: source.desc,
            code: &assembled.code,
            exceptions: &assembled.exception_table,
            labels: stack_maps::table_labels(&assembled.line_numbers, &lvt, assembled.code.len()),
        }) {
            self.trace_as_emitted(source, format_args!("no frames: {decline:?}"));
            return None;
        }
        Some(Rewritten {
            code: assembled.code,
            exceptions: assembled.exception_table,
            lnt: assembled.line_numbers,
            lvt,
            implicit_void_return_pc,
        })
    }

    /// Trace why the rewrite of `source` keeps the method as emitted.
    fn trace_as_emitted(&self, source: MethodIdentity<'_>, why: std::fmt::Arguments<'_>) {
        crate::trace_compiler!(
            "bytecode",
            "{}.{}{} is written as emitted: {why}",
            self.internal_name,
            source.name,
            source.desc
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jvm::classfile::{ACC_PUBLIC, ACC_STATIC};

    impl ClassWriter {
        fn rewritten(&self, method: &MethodInfo, source: &RewriteSource) -> Option<Rewritten> {
            let mut pool = PoolLookup::new(&self.cp, &self.bootstrap_methods);
            self.rewritten_with(method, source, &mut pool)
        }
    }

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

    /// `static int t4(boolean x) { if (x) { int unused = 5; } return 1; }`, with `unused`'s range
    /// opening after its store and closing at the block's end: an empty range.
    fn writer_with_empty_local() -> ClassWriter {
        let mut writer = ClassWriter::new("T", "java/lang/Object");
        let mut code = CodeBuilder::new(1);
        let closed = code.new_label();
        code.iload(0);
        code.ifeq(closed);
        code.push_int(5, &mut writer);
        code.istore(1);
        let opened = u16::try_from(code.bytes.len()).expect("a short body");
        code.bind(closed);
        code.push_int(1, &mut writer);
        code.ireturn();
        code.link();
        code.add_local_entry(0, None, 0, "x", "Z");
        code.add_local_entry(opened, Some(0), 1, "unused", "I");
        writer.add_method(ACC_PUBLIC | ACC_STATIC, "t4", "(Z)I", &code);
        writer
    }

    fn local_names(writer: &ClassWriter, method: &MethodInfo) -> Vec<String> {
        method
            .lvt
            .iter()
            .map(|&(name, ..)| writer.cp.utf8_at(name).expect("a name").to_string())
            .collect()
    }

    #[test]
    fn a_local_with_an_empty_range_goes_when_the_method_is_rewritten() {
        // The store opens a named local, so it is no temporary and stays; the entry goes with
        // kotlinc's final dead-code step instead of keeping the method as emitted.
        let mut writer = writer_with_empty_local();
        let emitted = writer.methods[0].code.clone().expect("a body");
        assert_eq!(local_names(&writer, &writer.methods[0]), ["x", "unused"]);
        writer.rewrite_methods();
        let method = &writer.methods[0];
        assert_eq!(emitted, [0x1a, 0x99, 0x00, 0x05, 0x08, 0x3c, 0x04, 0xac]);
        assert_eq!(method.code.as_deref(), Some(&emitted[..]));
        assert_eq!(local_names(&writer, method), ["x"]);
        let &(_, _, slot, start, len) = &method.lvt[0];
        assert_eq!((slot, start, len), (0, Some(0), None));
    }

    #[test]
    fn a_method_written_as_emitted_drops_its_empty_locals() {
        // A method no rewrite reads (a coroutine transform's, for one) is written as it holds, but
        // its empty locals go as kotlinc's `prepareForEmitting` drops them from every method.
        let mut writer = writer_with_empty_local();
        writer.methods[0].rewrite_source = None;
        writer.rewrite_methods();
        let method = &writer.methods[0];
        assert_eq!(
            method.code.as_deref(),
            Some(&[0x1a, 0x99, 0x00, 0x05, 0x08, 0x3c, 0x04, 0xac][..])
        );
        assert_eq!(local_names(&writer, method), ["x"]);
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
