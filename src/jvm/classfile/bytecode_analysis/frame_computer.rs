//! The `StackMapTable` a method's final instructions imply, computed the way kotlinc's class writer
//! computes it.
//!
//! kotlinc never writes a frame itself. Its `ClassBuilderFactories.BinaryClassWriter` is an ASM
//! `ClassWriter(COMPUTE_MAXS | COMPUTE_FRAMES)` whose `getCommonSuperClass` answers
//! `java/lang/Object` for every pair, so every frame in a kotlinc class is ASM's forward dataflow over
//! the method as finally written: basic blocks split at every label and after every jump, each
//! block's input the merge of the states that reach it, and a frame written at each reachable jump
//! target and handler entry. This is that computation over a decoded body, with ASM's merge rules:
//!
//! - equal types stay; `null` meets a reference (of any dimension) at that reference;
//! - two references of equal array dimension meet at that dimension of `Object`, two arrays of
//!   equal dimension but different primitive elements one dimension lower, and any other pair of
//!   references at the lower dimension of `Object` (a primitive array counting one dimension less);
//! - everything else, including two different uninitialized values, meets at `top`.
//!
//! A handler's input merges, for every block its range covers, both that block's input locals and
//! its output locals, with the caught type alone on the stack. A block nothing reaches is what ASM
//! rewrites into `nop`s ending in `athrow`, framed with no locals and a `Throwable` on the stack.
//!
//! Block boundaries are part of the result (through handler merges), and a label that nothing in the
//! class file records cannot be recovered from it. Callers pass every position they know a label
//! stood at; see [`FrameComputation::compute`].

use super::control_graph::Handler;
use super::frame_types::{method_types, step, FrameState, PoolView, VerificationType};
use crate::jvm::inline::{BranchTarget, Insn};

const OBJECT: &str = "java/lang/Object";
const THROWABLE: &str = "java/lang/Throwable";

/// One exception-table entry in instruction indices, with the internal name of the class it
/// catches (`None` for a catch-all).
#[derive(Clone, Debug)]
pub(crate) struct TypedHandler {
    pub range: Handler,
    pub catch_type: Option<String>,
}

/// A frame as the class file carries it: at an instruction index, locals in frame form (a
/// `long`/`double` is ONE entry, trailing `top`s dropped) and the stack bottom-first.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ComputedFrame {
    pub index: usize,
    pub locals: Vec<VerificationType>,
    pub stack: Vec<VerificationType>,
}

/// Why no frames could be computed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Decline {
    /// A subroutine instruction (`jsr`/`ret`) or a branch outside the body.
    UnsupportedControlFlow,
    /// An instruction the interpreter cannot step, or one that underflows the stack.
    Unsteppable(usize),
    /// Two edges bring stacks of different heights into one block.
    StackHeight(usize),
    /// The method descriptor is malformed.
    Descriptor,
}

/// The inputs of one computation.
pub(crate) struct FrameComputation<'a> {
    pub insns: &'a [Insn],
    pub handlers: &'a [TypedHandler],
    /// `labels[i]`: a label stood at instruction `i` (a branch target, a line number, a local's
    /// bound, a protected-range bound, ...). Length `insns.len() + 1`, or shorter; missing entries
    /// read as `false`.
    pub labels: &'a [bool],
    /// The class being written, which a constructor's `this` becomes after its `super(...)`/`this(...)`
    /// call.
    pub this_class: &'a str,
    pub pool: &'a dyn PoolView,
}

impl FrameComputation<'_> {
    /// The frames ASM would write for this body, in instruction order.
    pub(crate) fn compute(
        &self,
        access: u16,
        name: &str,
        descriptor: &str,
    ) -> Result<Vec<ComputedFrame>, Decline> {
        let entry = entry_locals(access, name, descriptor, self.this_class)?;
        let n = self.insns.len();
        let blocks = self.blocks()?;
        let block_of = {
            let mut block_of = vec![0usize; n];
            for (b, block) in blocks.iter().enumerate() {
                for slot in &mut block_of[block.start..block.end] {
                    *slot = b;
                }
            }
            block_of
        };
        let start_of = |index: usize| -> Option<usize> {
            let b = *block_of.get(index)?;
            (blocks[b].start == index).then_some(b)
        };
        // Exception edges per block: every block whose start lies in a protected range.
        let mut exceptional: Vec<Vec<(usize, VerificationType)>> = vec![Vec::new(); blocks.len()];
        let mut jump_target = vec![false; blocks.len()];
        for handler in self.handlers {
            let Some(to) = start_of(handler.range.handler) else {
                return Err(Decline::UnsupportedControlFlow);
            };
            jump_target[to] = true;
            let caught = VerificationType::Reference(
                handler
                    .catch_type
                    .as_deref()
                    .unwrap_or(THROWABLE)
                    .to_string(),
            );
            for (b, block) in blocks.iter().enumerate() {
                if block.start >= handler.range.start && block.start < handler.range.end {
                    exceptional[b].push((to, caught.clone()));
                }
            }
        }
        for block in &blocks {
            for &to in &block.targets {
                jump_target[start_of(to).ok_or(Decline::UnsupportedControlFlow)?] = true;
            }
        }

        let mut input: Vec<Option<FrameState>> = vec![None; blocks.len()];
        input[0] = Some(FrameState {
            locals: entry.clone(),
            stack: Vec::new(),
        });
        let mut pending: Vec<usize> = vec![0];
        let mut queued = vec![false; blocks.len()];
        queued[0] = true;
        while let Some(b) = pending.pop() {
            queued[b] = false;
            let block = &blocks[b];
            let entry_state = input[b].clone().expect("a queued block has an input");
            let mut state = entry_state.clone();
            for index in block.start..block.end {
                state = step(self.insns, index, &state, self.pool, Some(self.this_class))
                    .ok_or(Decline::Unsteppable(index))?;
            }
            let mut successors: Vec<(usize, FrameState)> = Vec::new();
            if block.falls_through && block.end < n {
                successors.push((block_of[block.end], state.clone()));
            }
            for &to in &block.targets {
                successors.push((block_of[to], state.clone()));
            }
            for (to, caught) in &exceptional[b] {
                // ASM merges the block's output locals, then its input locals.
                let mut locals = state.locals.clone();
                merge_into(&mut locals, &entry_state.locals);
                successors.push((
                    *to,
                    FrameState {
                        locals,
                        stack: vec![caught.clone()],
                    },
                ));
            }
            for (to, arriving) in successors {
                let changed = match &mut input[to] {
                    slot @ None => {
                        *slot = Some(arriving);
                        true
                    }
                    Some(existing) => {
                        if existing.stack.len() != arriving.stack.len() {
                            return Err(Decline::StackHeight(blocks[to].start));
                        }
                        let a = merge_into(&mut existing.locals, &arriving.locals);
                        let s = merge_values(&mut existing.stack, &arriving.stack);
                        a | s
                    }
                };
                if changed && !queued[to] {
                    queued[to] = true;
                    pending.push(to);
                }
            }
        }

        let mut frames = Vec::new();
        for (b, block) in blocks.iter().enumerate() {
            match &input[b] {
                Some(state) if jump_target[b] => frames.push(ComputedFrame {
                    index: block.start,
                    locals: frame_locals(&state.locals),
                    stack: state.stack.clone(),
                }),
                Some(_) => {}
                // Unreachable: ASM rewrites the block to `nop`s and an `athrow`, and frames it.
                None if block.start < block.end => frames.push(ComputedFrame {
                    index: block.start,
                    locals: Vec::new(),
                    stack: vec![VerificationType::Reference(THROWABLE.to_string())],
                }),
                None => {}
            }
        }
        Ok(frames)
    }

    /// Basic blocks in instruction order, split where ASM splits them: at every label, and after
    /// every jump, switch, return and `athrow`.
    fn blocks(&self) -> Result<Vec<Block>, Decline> {
        let n = self.insns.len();
        let mut starts = vec![false; n + 1];
        starts[0] = true;
        for (index, &label) in self.labels.iter().enumerate().take(n + 1) {
            starts[index] |= label;
        }
        for handler in self.handlers {
            for at in [
                handler.range.start,
                handler.range.end,
                handler.range.handler,
            ] {
                *starts.get_mut(at).ok_or(Decline::UnsupportedControlFlow)? = true;
            }
        }
        let mut targets_of: Vec<Vec<usize>> = vec![Vec::new(); n];
        let mut ends_flow = vec![false; n];
        for (index, insn) in self.insns.iter().enumerate() {
            let internal = |target: &BranchTarget| match target {
                BranchTarget::Internal(to) if *to < n => Ok(*to),
                _ => Err(Decline::UnsupportedControlFlow),
            };
            match insn {
                Insn::Branch { op, target } | Insn::BranchW { op, target } => {
                    if matches!(op, 0xa8 | 0xc9) {
                        return Err(Decline::UnsupportedControlFlow);
                    }
                    targets_of[index].push(internal(target)?);
                    ends_flow[index] = matches!(op, 0xa7 | 0xc8);
                    starts[index + 1] = true;
                }
                Insn::TableSwitch {
                    default, targets, ..
                } => {
                    for &to in std::iter::once(default).chain(targets) {
                        targets_of[index].push(internal(&BranchTarget::Internal(to))?);
                    }
                    ends_flow[index] = true;
                    starts[index + 1] = true;
                }
                Insn::LookupSwitch { default, pairs } => {
                    for &to in std::iter::once(default).chain(pairs.iter().map(|(_, to)| to)) {
                        targets_of[index].push(internal(&BranchTarget::Internal(to))?);
                    }
                    ends_flow[index] = true;
                    starts[index + 1] = true;
                }
                Insn::Plain { op, operands } => {
                    let ret = *op == 0xa9 || (*op == 0xc4 && operands.first() == Some(&0xa9));
                    if ret {
                        return Err(Decline::UnsupportedControlFlow);
                    }
                    if matches!(op, 0xac..=0xb1 | 0xbf) {
                        ends_flow[index] = true;
                        starts[index + 1] = true;
                    }
                }
            }
            for &to in &targets_of[index] {
                starts[to] = true;
            }
        }
        let mut blocks = Vec::new();
        let mut start = 0;
        for (index, &starts_block) in starts.iter().enumerate().take(n + 1).skip(1) {
            if starts_block || index == n {
                let last = index - 1;
                let mut targets = Vec::new();
                // Only the block's last instruction can branch: every branch ends its block.
                for &to in &targets_of[last] {
                    if !targets.contains(&to) {
                        targets.push(to);
                    }
                }
                blocks.push(Block {
                    start,
                    end: index,
                    targets,
                    falls_through: !ends_flow[last],
                });
                start = index;
            }
        }
        Ok(blocks)
    }
}

struct Block {
    start: usize,
    end: usize,
    targets: Vec<usize>,
    falls_through: bool,
}

/// The locals on entry, one entry per slot: `this` (uninitialized in a constructor) and the
/// parameters.
fn entry_locals(
    access: u16,
    name: &str,
    descriptor: &str,
    this_class: &str,
) -> Result<Vec<VerificationType>, Decline> {
    let mut locals = Vec::new();
    if access & 0x0008 == 0 {
        locals.push(if name == "<init>" {
            VerificationType::UninitializedThis
        } else {
            VerificationType::Reference(this_class.to_string())
        });
    }
    let (params, _) = method_types(descriptor).ok_or(Decline::Descriptor)?;
    for param in params {
        let wide = param.is_wide();
        locals.push(param);
        if wide {
            locals.push(VerificationType::Top);
        }
    }
    Ok(locals)
}

/// The method's entry frame in frame form: the baseline its first `StackMapTable` entry is relative
/// to.
pub(crate) fn entry_frame(
    access: u16,
    name: &str,
    descriptor: &str,
    this_class: &str,
) -> Result<Vec<VerificationType>, Decline> {
    entry_locals(access, name, descriptor, this_class).map(|slots| frame_locals(&slots))
}

/// Slot-indexed locals in the form a frame carries them.
pub(crate) fn frame_locals(slots: &[VerificationType]) -> Vec<VerificationType> {
    let mut locals = Vec::with_capacity(slots.len());
    let mut slot = 0;
    while slot < slots.len() {
        let value = slots[slot].clone();
        slot += if value.is_wide() { 2 } else { 1 };
        locals.push(value);
    }
    while locals.last() == Some(&VerificationType::Top) {
        locals.pop();
    }
    locals
}

/// Merge slot-indexed `arriving` locals into `existing`; `true` when anything changed.
fn merge_into(existing: &mut Vec<VerificationType>, arriving: &[VerificationType]) -> bool {
    let len = existing.len().max(arriving.len());
    let mut changed = false;
    if existing.len() < len {
        existing.resize(len, VerificationType::Top);
        changed = true;
    }
    for (index, slot) in existing.iter_mut().enumerate() {
        let theirs = arriving.get(index).unwrap_or(&VerificationType::Top);
        changed |= merge_one(slot, theirs);
    }
    changed
}

fn merge_values(existing: &mut [VerificationType], arriving: &[VerificationType]) -> bool {
    let mut changed = false;
    for (mine, theirs) in existing.iter_mut().zip(arriving) {
        changed |= merge_one(mine, theirs);
    }
    changed
}

/// ASM's `Frame.merge` of one type into another, with `java/lang/Object` as every pair's common
/// superclass. `true` when `dst` changed.
fn merge_one(dst: &mut VerificationType, src: &VerificationType) -> bool {
    use VerificationType::*;
    if dst == src {
        return false;
    }
    let merged = match (&*dst, src) {
        (Null, Reference(_)) => src.clone(),
        (Reference(_), Null) => return false,
        (Reference(a), Reference(b)) => Reference(merge_references(a, b)),
        _ => Top,
    };
    if merged == *dst {
        return false;
    }
    *dst = merged;
    true
}

/// Two different reference types' merge under ASM's array rules.
fn merge_references(a: &str, b: &str) -> String {
    let (a_dim, a_ref) = array_shape(a);
    let (b_dim, b_ref) = array_shape(b);
    let dim = if a_dim == b_dim && a_ref == b_ref {
        if a_ref {
            a_dim
        } else {
            // Equal dimensions of different primitive elements: e.g. `[I` and `[J` meet at `Object`.
            a_dim - 1
        }
    } else {
        // A primitive array counts one dimension less: its elements are not references.
        let a_eff = if a_ref { a_dim } else { a_dim - 1 };
        let b_eff = if b_ref { b_dim } else { b_dim - 1 };
        a_eff.min(b_eff)
    };
    if dim == 0 {
        OBJECT.to_string()
    } else {
        format!("{}L{OBJECT};", "[".repeat(dim))
    }
}

/// The array dimension of an internal name or array descriptor, and whether its innermost element
/// is a reference. A plain class name (dimension zero) is a reference whatever it is called, so a
/// class named `I` is not mistaken for `int`; a primitive array always has dimension one or more.
fn array_shape(name: &str) -> (usize, bool) {
    let dim = name.bytes().take_while(|&b| b == b'[').count();
    (dim, dim == 0 || name.as_bytes().get(dim) == Some(&b'L'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jvm::classfile::VerifType;

    /// Field `#1` is a `String`, `#2` an `Integer`; class `#3` is `java/lang/Exception`.
    struct Pool;

    impl PoolView for Pool {
        fn class_name(&self, index: u16) -> Option<&str> {
            (index == 3).then_some("java/lang/Exception")
        }
        fn field_descriptor(&self, index: u16) -> Option<&str> {
            match index {
                1 => Some("Ljava/lang/String;"),
                2 => Some("Ljava/lang/Integer;"),
                _ => None,
            }
        }
        fn method_descriptor(&self, _: u16) -> Option<&str> {
            None
        }
        fn loadable_constant(&self, _: u16) -> Option<VerifType> {
            None
        }
    }

    fn plain(op: u8) -> Insn {
        Insn::Plain {
            op,
            operands: Vec::new(),
        }
    }

    fn getstatic(field: u8) -> Insn {
        Insn::Plain {
            op: 0xb2,
            operands: vec![0, field],
        }
    }

    fn branch(op: u8, to: usize) -> Insn {
        Insn::Branch {
            op,
            target: BranchTarget::Internal(to),
        }
    }

    fn compute(insns: &[Insn], handlers: &[TypedHandler], descriptor: &str) -> Vec<ComputedFrame> {
        FrameComputation {
            insns,
            handlers,
            labels: &[],
            this_class: "p/K",
            pool: &Pool,
        }
        .compute(0x0008, "f", descriptor)
        .expect("frames compute")
    }

    fn reference(name: &str) -> VerificationType {
        VerificationType::Reference(name.to_string())
    }

    #[test]
    fn a_join_of_two_references_holds_object_and_frames_only_jump_targets() {
        use VerificationType::*;
        // static f(Z)Ljava/lang/Object; = if (b) STRING else INTEGER
        let insns = [
            plain(0x1a),     // 0 iload_0
            branch(0x99, 4), // 1 ifeq 4
            getstatic(1),    // 2 getstatic String
            branch(0xa7, 5), // 3 goto 5
            getstatic(2),    // 4 getstatic Integer
            plain(0xb0),     // 5 areturn
        ];
        let frames = compute(&insns, &[], "(Z)Ljava/lang/Object;");
        assert_eq!(
            frames,
            vec![
                ComputedFrame {
                    index: 4,
                    locals: vec![Integer],
                    stack: vec![],
                },
                ComputedFrame {
                    index: 5,
                    locals: vec![Integer],
                    stack: vec![reference(OBJECT)],
                },
            ]
        );
    }

    #[test]
    fn a_handler_merges_the_input_and_output_locals_of_each_covered_block() {
        // static f()V: x = STRING; try { x = INTEGER } catch (e: Exception) {} return
        let insns = [
            getstatic(1),    // 0 getstatic String
            plain(0x4b),     // 1 astore_0
            getstatic(2),    // 2 getstatic Integer   (protected)
            plain(0x4b),     // 3 astore_0            (protected)
            branch(0xa7, 6), // 4 goto 6
            plain(0x4c),     // 5 astore_1            (handler)
            plain(0xb1),     // 6 return
        ];
        let handlers = [TypedHandler {
            range: Handler {
                start: 2,
                end: 4,
                handler: 5,
            },
            catch_type: Some("java/lang/Exception".to_string()),
        }];
        let frames = compute(&insns, &handlers, "()V");
        assert_eq!(frames[0].index, 5);
        assert_eq!(frames[0].locals, vec![reference(OBJECT)]);
        assert_eq!(frames[0].stack, vec![reference("java/lang/Exception")]);
        // The join after the handler: the handler stored its exception into slot 1.
        assert_eq!(frames[1].index, 6);
        assert_eq!(frames[1].locals, vec![reference(OBJECT)]);
        assert!(frames[1].stack.is_empty());
    }

    #[test]
    fn an_unreachable_block_is_framed_as_asm_rewrites_it() {
        // static f()V: return; nop; return
        let insns = [plain(0xb1), plain(0x00), plain(0xb1)];
        let frames = compute(&insns, &[], "()V");
        assert_eq!(
            frames,
            vec![ComputedFrame {
                index: 1,
                locals: vec![],
                stack: vec![reference(THROWABLE)],
            }]
        );
    }

    #[test]
    fn references_meet_at_object_of_their_common_dimension() {
        assert_eq!(
            merge_references("java/lang/String", "java/lang/Integer"),
            OBJECT
        );
        assert_eq!(
            merge_references("[Ljava/lang/String;", "[Ljava/lang/Integer;"),
            "[Ljava/lang/Object;"
        );
        assert_eq!(merge_references("[I", "[J"), OBJECT);
        assert_eq!(merge_references("[[I", "[[J"), "[Ljava/lang/Object;");
        assert_eq!(
            merge_references("[Ljava/lang/String;", "java/lang/String"),
            OBJECT
        );
        assert_eq!(
            merge_references("[[Ljava/lang/String;", "[[I"),
            "[Ljava/lang/Object;"
        );
    }

    #[test]
    fn null_meets_a_reference_at_the_reference_and_anything_else_at_top() {
        let mut dst = VerificationType::Null;
        assert!(merge_one(
            &mut dst,
            &VerificationType::Reference("[I".to_string())
        ));
        assert_eq!(dst, VerificationType::Reference("[I".to_string()));
        assert!(!merge_one(&mut dst, &VerificationType::Null));
        let mut dst = VerificationType::Integer;
        assert!(merge_one(&mut dst, &VerificationType::Float));
        assert_eq!(dst, VerificationType::Top);
        let mut dst = VerificationType::Uninitialized(3);
        assert!(merge_one(&mut dst, &VerificationType::Uninitialized(7)));
        assert_eq!(dst, VerificationType::Top);
    }

    #[test]
    fn frame_locals_fold_wide_values_and_drop_trailing_tops() {
        use VerificationType::*;
        assert_eq!(
            frame_locals(&[Integer, Long, Top, Top, Double, Top, Top, Top]),
            vec![Integer, Long, Top, Double]
        );
    }
}
