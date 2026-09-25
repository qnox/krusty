//! Forward verification-type analysis over a decoded JVM method body.
//!
//! JVM transforms need the verifier's local and stack types at arbitrary instructions. A
//! `StackMapTable` frame states them only where the compiler wrote one, and a value rewritten by a
//! loop can have a merge type that no single recorded frame states.
//!
//! So this computes it the way the verifier does: seeded by the method's entry frame, every
//! instruction's effect is applied forward, control-flow joins meet the way a verifier's do, and a
//! recorded frame REPLACES the computed state at its position — the verifier checks the incoming
//! state against the frame and continues from the frame, so the frame is what later code is held
//! to. The result is the state the verifier will hold at every instruction, which is exactly the
//! state any target transform must preserve.

use std::collections::HashMap;

use super::ControlGraph;
use crate::jvm::classfile::VerifType;
use crate::jvm::inline::Insn;

const OBJECT_INTERNAL_NAME: &str = "java/lang/Object";

/// What the analysis needs to read out of the constant pool the body's operands index.
pub(crate) trait PoolView {
    /// The internal name of the `CONSTANT_Class` at `index`.
    fn class_name(&self, index: u16) -> Option<&str>;
    /// The descriptor of the field reference at `index`.
    fn field_descriptor(&self, index: u16) -> Option<&str>;
    /// The descriptor of the method reference (or `invokedynamic` call site) at `index`.
    fn method_descriptor(&self, index: u16) -> Option<&str>;
    /// The verification type an `ldc` of the constant at `index` pushes.
    fn loadable_constant(&self, index: u16) -> Option<VerifType>;
}

impl PoolView for crate::jvm::classfile::ClassWriter {
    fn class_name(&self, index: u16) -> Option<&str> {
        self.class_name_at(index)
    }

    fn field_descriptor(&self, index: u16) -> Option<&str> {
        self.fieldref_descriptor_at(index)
    }

    fn method_descriptor(&self, index: u16) -> Option<&str> {
        self.methodref_parts(index)
            .map(|(_, _, descriptor)| descriptor)
            .or_else(|| self.invokedynamic_descriptor_at(index))
    }

    fn loadable_constant(&self, index: u16) -> Option<VerifType> {
        self.loadable_constant_type_at(index)
    }
}

/// A verification type as the analysis tracks it. Distinct from [`VerifType`] in one respect: an
/// object created by `new` and not yet constructed is `Uninitialized`, which a frame cannot name in
/// this compiler's representation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum VerificationType {
    Top,
    Integer,
    Float,
    Long,
    Double,
    Null,
    UninitializedThis,
    /// The `new` at this instruction index produced it, and no `<init>` has run on it yet.
    Uninitialized(usize),
    /// An internal class name, or an array descriptor. Shared: every instruction the analysis
    /// steps copies the whole frame, and a copy must not reallocate each name in it.
    Reference(std::rc::Rc<str>),
}

impl VerificationType {
    pub(crate) fn from_verif(v: &VerifType, pool: &dyn PoolView) -> VerificationType {
        match v {
            VerifType::Top => VerificationType::Top,
            VerifType::Integer => VerificationType::Integer,
            VerifType::Float => VerificationType::Float,
            VerifType::Long => VerificationType::Long,
            VerifType::Double => VerificationType::Double,
            VerifType::Null => VerificationType::Null,
            VerifType::UninitializedThis => VerificationType::UninitializedThis,
            VerifType::ObjectName(name) => VerificationType::Reference(name.as_str().into()),
            VerifType::Object(index) => match pool.class_name(*index) {
                Some(name) => VerificationType::Reference(name.into()),
                None => VerificationType::Top,
            },
        }
    }

    /// The frame representation. An uninitialized object has none and reads as `Top`.
    pub(crate) fn to_verif(&self) -> VerifType {
        match self {
            VerificationType::Top | VerificationType::Uninitialized(_) => VerifType::Top,
            VerificationType::Integer => VerifType::Integer,
            VerificationType::Float => VerifType::Float,
            VerificationType::Long => VerifType::Long,
            VerificationType::Double => VerifType::Double,
            VerificationType::Null => VerifType::Null,
            VerificationType::UninitializedThis => VerifType::UninitializedThis,
            VerificationType::Reference(name) => VerifType::ObjectName(name.to_string()),
        }
    }

    pub(crate) fn is_wide(&self) -> bool {
        matches!(self, VerificationType::Long | VerificationType::Double)
    }

    /// What two edges into one point agree on. Two references meet at `Object`, which either is
    /// assignable to; `null` meets a reference at that reference; anything else that differs is
    /// unusable past the join.
    pub(crate) fn join(&self, other: &VerificationType) -> VerificationType {
        use VerificationType::*;
        match (self, other) {
            (a, b) if a == b => a.clone(),
            (Null, Reference(r)) | (Reference(r), Null) => Reference(r.clone()),
            (Reference(_), Reference(_)) => Reference(OBJECT_INTERNAL_NAME.into()),
            _ => Top,
        }
    }
}

/// The verifier's state at one program point: locals by SLOT (a `long`/`double` is its type then
/// `Top`), and the operand stack bottom-first, one entry per value.
#[derive(Clone, Debug, Eq, PartialEq, Default)]
pub(crate) struct FrameState {
    pub locals: Vec<VerificationType>,
    pub stack: Vec<VerificationType>,
}

impl FrameState {
    fn from_verif(locals: &[VerifType], stack: &[VerifType], pool: &dyn PoolView) -> FrameState {
        FrameState {
            locals: locals
                .iter()
                .map(|v| VerificationType::from_verif(v, pool))
                .collect(),
            stack: stack
                .iter()
                .map(|v| VerificationType::from_verif(v, pool))
                .collect(),
        }
    }

    pub(crate) fn local(&self, slot: u16) -> &VerificationType {
        self.locals
            .get(usize::from(slot))
            .unwrap_or(&VerificationType::Top)
    }

    fn set_local(&mut self, slot: usize, value: VerificationType) {
        let needed = slot + 1 + usize::from(value.is_wide());
        if self.locals.len() < needed {
            self.locals.resize(needed, VerificationType::Top);
        }
        // Writing the second word of a `long`/`double` destroys it (JVMS §4.10.1.9, `store`).
        if slot > 0 && self.locals[slot - 1].is_wide() {
            self.locals[slot - 1] = VerificationType::Top;
        }
        let wide = value.is_wide();
        self.locals[slot] = value;
        if wide {
            self.locals[slot + 1] = VerificationType::Top;
        }
    }

    fn pop(&mut self) -> Option<VerificationType> {
        self.stack.pop()
    }

    fn push(&mut self, value: VerificationType) {
        self.stack.push(value);
    }

    /// Meet with an edge arriving in `other`; `true` when this changed. A stack of a different
    /// height, or one whose entries do not meet, is not a state at all.
    fn join_with(&mut self, other: &FrameState) -> Option<bool> {
        if self.stack.len() != other.stack.len() {
            return None;
        }
        let mut changed = false;
        if other.locals.len() > self.locals.len() {
            self.locals
                .resize(other.locals.len(), VerificationType::Top);
        }
        for (index, mine) in self.locals.iter_mut().enumerate() {
            let theirs = other.locals.get(index).unwrap_or(&VerificationType::Top);
            let met = mine.join(theirs);
            changed |= met != *mine;
            *mine = met;
        }
        // A stack entry the edges disagree on meets at `top` like a local: it is unusable past
        // the join, and whether that matters is decided where it is consumed.
        for (mine, theirs) in self.stack.iter_mut().zip(&other.stack) {
            let met = mine.join(theirs);
            changed |= met != *mine;
            *mine = met;
        }
        Some(changed)
    }
}

/// The stack entry a field descriptor names.
fn descriptor_type(descriptor: &str) -> Option<VerificationType> {
    Some(match descriptor.as_bytes().first()? {
        b'B' | b'C' | b'I' | b'S' | b'Z' => VerificationType::Integer,
        b'J' => VerificationType::Long,
        b'F' => VerificationType::Float,
        b'D' => VerificationType::Double,
        b'L' => VerificationType::Reference(descriptor.get(1..descriptor.len() - 1)?.into()),
        b'[' => VerificationType::Reference(descriptor.into()),
        _ => return None,
    })
}

/// A method descriptor's parameters (one entry each) and its result (`None` for `void`).
pub(super) fn method_types(
    descriptor: &str,
) -> Option<(Vec<VerificationType>, Option<VerificationType>)> {
    // Parameters are read type by type up to the `)` that follows one: a class name may itself
    // contain `(` or `)` (a backticked Kotlin name), so the first `)` is not necessarily the end.
    let bytes = descriptor.as_bytes();
    if bytes.first() != Some(&b'(') {
        return None;
    }
    let mut out = Vec::new();
    let mut at = 1;
    while *bytes.get(at)? != b')' {
        let start = at;
        while bytes.get(at) == Some(&b'[') {
            at += 1;
        }
        if bytes.get(at) == Some(&b'L') {
            at += descriptor.get(at..)?.find(';')?;
        }
        at += 1;
        out.push(descriptor_type(descriptor.get(start..at)?)?);
    }
    let ret = descriptor.get(at + 1..)?;
    let ret = match ret {
        "V" => None,
        other => Some(descriptor_type(other)?),
    };
    Some((out, ret))
}

/// The element a reference array holds.
fn element_type(array: &VerificationType) -> VerificationType {
    match array {
        VerificationType::Null => VerificationType::Null,
        VerificationType::Reference(descriptor) => match descriptor.strip_prefix('[') {
            Some(element) => descriptor_type(element).unwrap_or(VerificationType::Top),
            None => VerificationType::Top,
        },
        _ => VerificationType::Top,
    }
}

/// The two-byte operand at the start of `operands`.
fn u2(operands: &[u8]) -> Option<u16> {
    Some(u16::from_be_bytes([*operands.first()?, *operands.get(1)?]))
}

/// Pop one value group for the `dup2` family: a single category-2 value, else two category-1
/// values. Returned bottom-first.
fn pop_group(state: &mut FrameState) -> Option<Vec<VerificationType>> {
    let top = state.pop()?;
    if top.is_wide() {
        Some(vec![top])
    } else {
        let under = state.pop()?;
        Some(vec![under, top])
    }
}

/// The verifier's state before every instruction of one method body.
pub(crate) struct FrameTypes {
    before: Vec<Option<FrameState>>,
}

impl FrameTypes {
    /// `entry` is the slot-indexed locals on entry; `frames` are the recorded frames as
    /// `(instruction index, slot-indexed locals, stack)`, ONE per index — the frame the class file
    /// will carry there (`ClassWriter::merged_frames`), not the per-label frames several labels
    /// bound at one offset registered. `None` when an instruction cannot be modelled, two frames
    /// share an index, or the body does not verify under this model — the caller declines then.
    pub(crate) fn analyze(
        insns: &[Insn],
        graph: &ControlGraph,
        entry: &[VerifType],
        frames: &[(usize, Vec<VerifType>, Vec<VerifType>)],
        pool: &dyn PoolView,
    ) -> Option<FrameTypes> {
        if graph.exit() != insns.len() {
            return None;
        }
        let mut recorded: HashMap<usize, FrameState> = HashMap::with_capacity(frames.len());
        for (index, locals, stack) in frames {
            // One frame per position, and it has to be the one the class file carries. Several
            // labels can be bound at ONE bytecode offset, and the verifier holds their MERGE there
            // — the common prefix of their locals. Collecting them into a map instead let the last
            // one win, which can be MORE precise than the merge: the analysis then types a slot the
            // verifier has as `top`, and the spill planned from it loads an unset local ("Bad local
            // variable type") while the join frame claims a type the fall-through edge never had
            // ("Inconsistent stackmap frames"). Merging is the caller's job, so two frames at one
            // index is a bug — decline rather than pick one.
            let frame = FrameState::from_verif(locals, stack, pool);
            if recorded.insert(*index, frame).is_some() {
                crate::trace_compiler!(
                    "bytecode",
                    "frame analysis: two frames recorded at {index}; they were not merged"
                );
                return None;
            }
        }
        let mut before: Vec<Option<FrameState>> = vec![None; insns.len() + 1];
        before[0] = Some(match recorded.get(&0) {
            Some(frame) => frame.clone(),
            None => FrameState::from_verif(entry, &[], pool),
        });
        let order = graph.reverse_post_order();
        // Propagate `state` along an edge into `to`. A recorded frame is what the verifier holds
        // there whatever arrives, so it is taken once and never widened; anywhere else the edges
        // meet.
        let propagate =
            |before: &mut Vec<Option<FrameState>>, to: usize, state: &FrameState| -> Option<bool> {
                if to >= insns.len() {
                    return Some(false);
                }
                if let Some(frame) = recorded.get(&to) {
                    return Some(match &before[to] {
                        Some(_) => false,
                        None => {
                            before[to] = Some(frame.clone());
                            true
                        }
                    });
                }
                match &mut before[to] {
                    Some(current) => current.join_with(state),
                    slot @ None => {
                        *slot = Some(state.clone());
                        Some(true)
                    }
                }
            };
        // Each pass visits the instructions in reverse post-order, and one is stepped only when
        // the state before it changed since it was last stepped. Stepping is a function of that
        // state, and propagating a state already propagated changes nothing (the meet is
        // idempotent and absorbing, and a recorded frame is taken once), so skipping an unchanged
        // instruction skips only no-ops: the passes, every state they reach and the fixpoint are
        // those of a sweep that re-steps every reachable instruction until a pass changes nothing.
        let mut pending = vec![false; insns.len() + 1];
        pending[0] = true;
        loop {
            let mut changed = false;
            for &index in &order {
                if index >= insns.len() || !std::mem::take(&mut pending[index]) {
                    continue;
                }
                let Some(state) = before[index].as_ref() else {
                    continue;
                };
                let Some(after) = step(insns, index, state, pool, None) else {
                    crate::trace_compiler!(
                        "bytecode",
                        "frame analysis: cannot step {:?} at {index} from {state:?}",
                        insns[index]
                    );
                    return None;
                };
                // A handler is entered with locals as they stood BEFORE this instruction, which a
                // self-edge below may join into; keep them only when a handler covers it.
                let entry_locals =
                    (!graph.exceptional_successors(index).is_empty()).then(|| state.locals.clone());
                for &to in graph.normal_successors(index) {
                    let Some(did) = propagate(&mut before, to, &after) else {
                        crate::trace_compiler!(
                            "bytecode",
                            "frame analysis: edge {index}->{to} does not meet: {after:?} into {:?} (insns {:?})",
                            before[to],
                            &insns[index.saturating_sub(4)..=index]
                        );
                        return None;
                    };
                    pending[to] |= did;
                    changed |= did;
                }
                let Some(entry_locals) = entry_locals else {
                    continue;
                };
                // A handler is entered with the exception alone on the stack, and locals as they
                // stood at whichever point of the instruction threw — before its own store, or
                // after it.
                let mut thrown = FrameState {
                    locals: entry_locals,
                    stack: vec![VerificationType::Reference("java/lang/Throwable".into())],
                };
                let after_store = FrameState {
                    locals: after.locals.clone(),
                    stack: thrown.stack.clone(),
                };
                thrown.join_with(&after_store)?;
                for &handler in graph.exceptional_successors(index) {
                    let Some(did) = propagate(&mut before, handler, &thrown) else {
                        crate::trace_compiler!(
                            "bytecode",
                            "frame analysis: handler edge {index}->{handler} does not meet: {thrown:?} into {:?}",
                            before[handler]
                        );
                        return None;
                    };
                    pending[handler] |= did;
                    changed |= did;
                }
            }
            if !changed {
                break;
            }
        }
        Some(FrameTypes { before })
    }

    /// The state immediately BEFORE `index` executes; `None` where the instruction is unreachable.
    pub(crate) fn before(&self, index: usize) -> Option<&FrameState> {
        self.before.get(index)?.as_ref()
    }
}

/// The state after `insn` (at `index`) runs from `state`. `None` for an opcode this does not
/// model, or one the state cannot legally run.
pub(super) fn step(
    insns: &[Insn],
    index: usize,
    state: &FrameState,
    pool: &dyn PoolView,
    this_class: Option<&str>,
) -> Option<FrameState> {
    use VerificationType::*;
    let mut s = state.clone();
    let (op, operands): (u8, &[u8]) = match insns.get(index)? {
        Insn::Plain { op, operands } => (*op, operands.as_slice()),
        Insn::Branch { op, .. } | Insn::BranchW { op, .. } => (*op, &[]),
        Insn::TableSwitch { .. } | Insn::LookupSwitch { .. } => {
            s.pop()?;
            return Some(s);
        }
    };
    let class_at = |operands: &[u8]| -> Option<VerificationType> {
        Some(Reference(pool.class_name(u2(operands)?)?.into()))
    };
    let invoke = |s: &mut FrameState, descriptor: &str, receiver: bool| -> Option<()> {
        let (params, ret) = method_types(descriptor)?;
        for _ in params {
            s.pop()?;
        }
        if receiver {
            s.pop()?;
        }
        if let Some(ret) = ret {
            s.push(ret);
        }
        Some(())
    };
    match op {
        0x00 => {}                                    // nop
        0x01 => s.push(Null),                         // aconst_null
        0x02..=0x08 | 0x10 | 0x11 => s.push(Integer), // iconst_*, bipush, sipush
        0x09 | 0x0a => s.push(Long),                  // lconst_*
        0x0b..=0x0d => s.push(Float),                 // fconst_*
        0x0e | 0x0f => s.push(Double),                // dconst_*
        0x12 => s.push(VerificationType::from_verif(
            &pool.loadable_constant(u16::from(*operands.first()?))?,
            pool,
        )), // ldc
        0x13 | 0x14 => s.push(VerificationType::from_verif(
            &pool.loadable_constant(u2(operands)?)?,
            pool,
        )), // ldc_w, ldc2_w
        0x15 | 0x1a..=0x1d => s.push(Integer),        // iload(_n)
        0x16 | 0x1e..=0x21 => s.push(Long),           // lload(_n)
        0x17 | 0x22..=0x25 => s.push(Float),          // fload(_n)
        0x18 | 0x26..=0x29 => s.push(Double),         // dload(_n)
        // A load of a slot the state holds as `top`, and an `iinc` of one that is not an `int`,
        // are not checked: they push or keep `top`, which the ORIGINAL class would already have
        // failed verification on. Being lenient here cannot make a verifiable body look wrong.
        0x19 => {
            let slot = u16::from(*operands.first()?);
            s.push(s.local(slot).clone());
        } // aload
        0x2a..=0x2d => {
            let slot = u16::from(op - 0x2a);
            s.push(s.local(slot).clone());
        } // aload_n
        0x2e..=0x35 => {
            s.pop()?;
            let array = s.pop()?;
            s.push(match op {
                0x2e | 0x33..=0x35 => Integer, // iaload, baload, caload, saload
                0x2f => Long,                  // laload
                0x30 => Float,                 // faload
                0x31 => Double,                // daload
                _ => element_type(&array),     // aaload
            });
        }
        0x36..=0x3a => {
            let value = s.pop()?;
            let slot = usize::from(*operands.first()?);
            s.set_local(slot, stored_type(op - 0x36, value));
        } // istore..astore
        0x3b..=0x4e => {
            let value = s.pop()?;
            let slot = usize::from((op - 0x3b) % 4);
            s.set_local(slot, stored_type((op - 0x3b) / 4, value));
        } // istore_n..astore_n
        0x4f..=0x56 => {
            s.pop()?;
            s.pop()?;
            s.pop()?;
        } // array stores
        0x57 => {
            s.pop()?;
        } // pop
        0x58 => {
            if !s.pop()?.is_wide() {
                s.pop()?;
            }
        } // pop2
        0x59 => {
            let top = s.stack.last()?.clone();
            s.push(top);
        } // dup
        0x5a => {
            let a = s.pop()?;
            let b = s.pop()?;
            s.push(a.clone());
            s.push(b);
            s.push(a);
        } // dup_x1
        0x5b => {
            let a = s.pop()?;
            let under = pop_group(&mut s)?;
            s.push(a.clone());
            s.stack.extend(under);
            s.push(a);
        } // dup_x2
        0x5c => {
            let group = pop_group(&mut s)?;
            s.stack.extend(group.iter().cloned());
            s.stack.extend(group);
        } // dup2
        0x5d => {
            let group = pop_group(&mut s)?;
            let under = s.pop()?;
            s.stack.extend(group.iter().cloned());
            s.push(under);
            s.stack.extend(group);
        } // dup2_x1
        0x5e => {
            let group = pop_group(&mut s)?;
            let under = pop_group(&mut s)?;
            s.stack.extend(group.iter().cloned());
            s.stack.extend(under);
            s.stack.extend(group);
        } // dup2_x2
        0x5f => {
            let a = s.pop()?;
            let b = s.pop()?;
            s.push(a);
            s.push(b);
        } // swap
        0x60..=0x73 => {
            s.pop()?;
            s.pop()?;
            s.push(arithmetic_type((op - 0x60) % 4));
        } // iadd..drem
        0x74..=0x77 => {
            s.pop()?;
            s.push(arithmetic_type(op - 0x74));
        } // ineg..dneg
        0x78..=0x7d => {
            s.pop()?;
            s.pop()?;
            s.push(if op % 2 == 0 { Integer } else { Long });
        } // ishl, lshl, ishr, lshr, iushr, lushr
        0x7e..=0x83 => {
            s.pop()?;
            s.pop()?;
            s.push(if op % 2 == 0 { Integer } else { Long });
        } // iand, land, ior, lor, ixor, lxor
        0x84 => {} // iinc
        0x85..=0x93 => {
            s.pop()?;
            s.push(match op {
                0x85 | 0x8c | 0x8f => Long,   // i2l, f2l, d2l
                0x86 | 0x89 | 0x90 => Float,  // i2f, l2f, d2f
                0x87 | 0x8a | 0x8d => Double, // i2d, l2d, f2d
                _ => Integer,                 // l2i, f2i, d2i, i2b, i2c, i2s
            });
        }
        0x94..=0x98 => {
            s.pop()?;
            s.pop()?;
            s.push(Integer);
        } // lcmp, fcmpl, fcmpg, dcmpl, dcmpg
        0x99..=0x9e | 0xc6 | 0xc7 => {
            s.pop()?;
        } // ifeq..ifle, ifnull, ifnonnull
        0x9f..=0xa6 => {
            s.pop()?;
            s.pop()?;
        } // if_icmp*, if_acmp*
        0xa7 | 0xc8 => {}                  // goto, goto_w
        0xa8 | 0xa9 | 0xc9 => return None, // jsr, ret, jsr_w
        0xac..=0xb0 => {
            s.pop()?;
        } // ireturn..areturn
        0xb1 => {}                         // return
        0xb2 => s.push(descriptor_type(pool.field_descriptor(u2(operands)?)?)?), // getstatic
        0xb3 => {
            s.pop()?;
        } // putstatic
        0xb4 => {
            s.pop()?;
            s.push(descriptor_type(pool.field_descriptor(u2(operands)?)?)?);
        } // getfield
        0xb5 => {
            s.pop()?;
            s.pop()?;
        } // putfield
        0xb6 | 0xb9 => {
            let descriptor = pool.method_descriptor(u2(operands)?)?;
            invoke(&mut s, descriptor, true)?;
        } // invokevirtual, invokeinterface
        0xb7 => {
            let descriptor = pool.method_descriptor(u2(operands)?)?;
            let (params, ret) = method_types(descriptor)?;
            for _ in params {
                s.pop()?;
            }
            let receiver = s.pop()?;
            // `<init>` on an uninitialized object makes every copy of it — on the stack and in
            // locals — the constructed class. Any other target is an ordinary call. A constructor's
            // own `this` becomes the class being written when the caller names it; otherwise
            // `Object` is as far as it needs to be right (no suspend function is a constructor).
            if let Uninitialized(created) = receiver {
                let constructed = Reference(constructed_class(insns, created, pool)?);
                for value in s.locals.iter_mut().chain(s.stack.iter_mut()) {
                    if *value == receiver {
                        *value = constructed.clone();
                    }
                }
            } else if receiver == UninitializedThis {
                let constructed = Reference(this_class.unwrap_or(OBJECT_INTERNAL_NAME).into());
                for value in s.locals.iter_mut().chain(s.stack.iter_mut()) {
                    if *value == UninitializedThis {
                        *value = constructed.clone();
                    }
                }
            }
            if let Some(ret) = ret {
                s.push(ret);
            }
        } // invokespecial
        0xb8 => {
            let descriptor = pool.method_descriptor(u2(operands)?)?;
            invoke(&mut s, descriptor, false)?;
        } // invokestatic
        0xba => {
            let descriptor = pool.method_descriptor(u2(operands)?)?;
            invoke(&mut s, descriptor, false)?;
        } // invokedynamic
        0xbb => {
            let _ = class_at(operands)?;
            s.push(Uninitialized(index));
        } // new
        0xbc => {
            s.pop()?;
            let element = match *operands.first()? {
                4 => "[Z",
                5 => "[C",
                6 => "[F",
                7 => "[D",
                8 => "[B",
                9 => "[S",
                10 => "[I",
                11 => "[J",
                _ => return None,
            };
            s.push(Reference(element.into()));
        } // newarray
        0xbd => {
            s.pop()?;
            let class = pool.class_name(u2(operands)?)?;
            s.push(Reference(
                match class.starts_with('[') {
                    true => format!("[{class}"),
                    false => format!("[L{class};"),
                }
                .into(),
            ));
        } // anewarray
        0xbe => {
            s.pop()?;
            s.push(Integer);
        } // arraylength
        0xbf => {
            s.pop()?;
        } // athrow
        0xc0 => {
            s.pop()?;
            s.push(class_at(operands)?);
        } // checkcast
        0xc1 => {
            s.pop()?;
            s.push(Integer);
        } // instanceof
        0xc2 | 0xc3 => {
            s.pop()?;
        } // monitorenter, monitorexit
        0xc4 => {
            let inner = *operands.first()?;
            let slot = usize::from(u2(operands.get(1..)?)?);
            match inner {
                0x15 => s.push(Integer),
                0x16 => s.push(Long),
                0x17 => s.push(Float),
                0x18 => s.push(Double),
                0x19 => s.push(s.local(slot as u16).clone()),
                0x36..=0x3a => {
                    let value = s.pop()?;
                    s.set_local(slot, stored_type(inner - 0x36, value));
                }
                0x84 => {}
                _ => return None,
            }
        } // wide
        0xc5 => {
            let dimensions = usize::from(*operands.get(2)?);
            for _ in 0..dimensions {
                s.pop()?;
            }
            s.push(class_at(operands)?);
        } // multianewarray
        // `impdep1`: a coroutine marker, stack-neutral like the `nop`s that replace it.
        0xfe => {}
        // `impdep2`: a codegen `InlineMarker` call. `mark(I)V` takes the id pushed before it.
        crate::jvm::bytecode::CODEGEN_MARKER_OP => {
            if crate::jvm::bytecode::CodegenMarker::from_operand(*operands.first()?)?
                == crate::jvm::bytecode::CodegenMarker::Mark
            {
                s.pop()?;
            }
        }
        _ => return None,
    }
    Some(s)
}

/// The class the `new` at `created` instantiates.
fn constructed_class(
    insns: &[Insn],
    created: usize,
    pool: &dyn PoolView,
) -> Option<std::rc::Rc<str>> {
    let Insn::Plain { op: 0xbb, operands } = insns.get(created)? else {
        return None;
    };
    Some(pool.class_name(u2(operands)?)?.into())
}

/// The type a store of kind `kind` (0 `int`, 1 `long`, 2 `float`, 3 `double`, 4 reference) leaves
/// in its slot: the value itself for a reference, the opcode's own type otherwise.
fn stored_type(kind: u8, value: VerificationType) -> VerificationType {
    match kind {
        0 => VerificationType::Integer,
        1 => VerificationType::Long,
        2 => VerificationType::Float,
        3 => VerificationType::Double,
        _ => value,
    }
}

/// The result type of an arithmetic opcode family member (0 `int`, 1 `long`, 2 `float`, 3
/// `double`).
fn arithmetic_type(kind: u8) -> VerificationType {
    match kind {
        0 => VerificationType::Integer,
        1 => VerificationType::Long,
        2 => VerificationType::Float,
        _ => VerificationType::Double,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jvm::classfile::bytecode_analysis::Handler;
    use crate::jvm::inline::BranchTarget;

    /// A pool with named entries, enough for the instructions the tests use.
    #[derive(Default)]
    struct FakePool {
        classes: HashMap<u16, &'static str>,
        fields: HashMap<u16, &'static str>,
        methods: HashMap<u16, &'static str>,
        constants: HashMap<u16, VerifType>,
    }

    impl PoolView for FakePool {
        fn class_name(&self, index: u16) -> Option<&str> {
            self.classes.get(&index).copied()
        }
        fn field_descriptor(&self, index: u16) -> Option<&str> {
            self.fields.get(&index).copied()
        }
        fn method_descriptor(&self, index: u16) -> Option<&str> {
            self.methods.get(&index).copied()
        }
        fn loadable_constant(&self, index: u16) -> Option<VerifType> {
            self.constants.get(&index).cloned()
        }
    }

    #[test]
    fn a_descriptor_naming_a_class_with_parentheses_reads_to_its_own_close() {
        let (params, ret) = method_types("(L();I[J)L();").expect("descriptor reads");
        assert_eq!(
            params,
            vec![
                VerificationType::Reference("()".into()),
                VerificationType::Integer,
                VerificationType::Reference("[J".into()),
            ]
        );
        assert_eq!(ret, Some(VerificationType::Reference("()".into())));
    }

    fn plain(op: u8) -> Insn {
        Insn::Plain {
            op,
            operands: Vec::new(),
        }
    }

    fn with(op: u8, operands: &[u8]) -> Insn {
        Insn::Plain {
            op,
            operands: operands.to_vec(),
        }
    }

    fn branch(op: u8, target: usize) -> Insn {
        Insn::Branch {
            op,
            target: BranchTarget::Internal(target),
        }
    }

    fn object(name: &str) -> VerifType {
        VerifType::ObjectName(name.to_string())
    }

    fn reference(name: &str) -> VerificationType {
        VerificationType::Reference(name.into())
    }

    fn analyze(
        insns: &[Insn],
        entry: &[VerifType],
        frames: &[(usize, Vec<VerifType>, Vec<VerifType>)],
        pool: &FakePool,
    ) -> FrameTypes {
        let graph = ControlGraph::build(insns, &[]).expect("graph");
        FrameTypes::analyze(insns, &graph, entry, frames, pool).expect("types")
    }

    /// A pool that counts method-descriptor reads: one per `invokestatic` the analysis steps.
    struct CountingPool {
        inner: FakePool,
        method_reads: std::cell::Cell<usize>,
    }

    impl PoolView for CountingPool {
        fn class_name(&self, index: u16) -> Option<&str> {
            self.inner.class_name(index)
        }
        fn field_descriptor(&self, index: u16) -> Option<&str> {
            self.inner.field_descriptor(index)
        }
        fn method_descriptor(&self, index: u16) -> Option<&str> {
            self.method_reads.set(self.method_reads.get() + 1);
            self.inner.method_descriptor(index)
        }
        fn loadable_constant(&self, index: u16) -> Option<VerifType> {
            self.inner.loadable_constant(index)
        }
    }

    /// An instruction is stepped again only when the state before it changed. A loop whose
    /// back edge changes nothing is walked once; one whose back edge widens its header re-steps the
    /// loop once, and the pass that confirms the fixpoint steps nothing.
    #[test]
    fn an_instruction_is_stepped_again_only_when_its_incoming_state_changed() {
        let mut inner = FakePool::default();
        inner.methods.insert(7, "(Ljava/lang/Object;)I");
        inner.constants.insert(3, object("java/lang/String"));
        let pool = CountingPool {
            inner,
            method_reads: std::cell::Cell::new(0),
        };
        let run = |insns: &[Insn]| {
            pool.method_reads.set(0);
            let graph = ControlGraph::build(insns, &[]).expect("graph");
            let types =
                FrameTypes::analyze(insns, &graph, &[object("Main")], &[], &pool).expect("types");
            (types, pool.method_reads.get())
        };

        // aload_0 ; invokestatic #7 ; ifne 0 ; return — the back edge arrives with the state
        // the loop was entered with.
        let steady = [
            plain(0x2a),
            with(0xb8, &[0, 7]),
            branch(0x9a, 0),
            plain(0xb1),
        ];
        let (_, steady_reads) = run(&steady);
        assert_eq!(steady_reads, 1, "an unchanged loop is walked once");

        // aconst_null ; astore_1 ; aload_1 ; invokestatic #7 ; ifeq 8 ; ldc #3 ; astore_1 ;
        // goto 2 ; return — the back edge widens slot 1 from `null` to `String` at the header.
        let widening = [
            plain(0x01),
            plain(0x4c),
            plain(0x2b),
            with(0xb8, &[0, 7]),
            branch(0x99, 8),
            with(0x12, &[3]),
            plain(0x4c),
            branch(0xa7, 2),
            plain(0xb1),
        ];
        let (types, widening_reads) = run(&widening);
        assert_eq!(widening_reads, 2, "one widening re-steps the loop once");
        let header = types.before(2).expect("reachable");
        assert_eq!(header.local(1), &reference("java/lang/String"));
        assert_eq!(
            types.before(8).expect("reachable").local(1),
            &reference("java/lang/String")
        );
    }

    /// Two frames at ONE index means the caller handed over per-label frames instead of the merged
    /// frame the class file carries. Picking one of them would let the analysis hold a type the
    /// verifier does not, so the analysis declines and the machine bails.
    #[test]
    fn two_frames_at_one_index_are_declined_rather_than_picked_between() {
        let pool = FakePool::default();
        // aload_0 ; return
        let insns = [plain(0x2a), plain(0xb1)];
        let graph = ControlGraph::build(&insns, &[]).expect("graph");
        let frames = [
            (1, vec![object("Main"), VerifType::Integer], Vec::new()),
            (1, vec![object("Main"), VerifType::Float], Vec::new()),
        ];
        assert!(
            FrameTypes::analyze(&insns, &graph, &[object("Main")], &frames, &pool).is_none(),
            "an unmerged pair must decline"
        );
        // The same body with ONE frame at that index analyses.
        assert!(
            FrameTypes::analyze(&insns, &graph, &[object("Main")], &frames[..1], &pool).is_some()
        );
    }

    #[test]
    fn a_store_types_its_slot_from_what_produced_the_value() {
        let mut pool = FakePool::default();
        pool.methods.insert(7, "()Ljava/lang/String;");
        pool.classes.insert(9, "java/lang/Integer");
        // invokestatic #7 ; astore_1 ; aconst_null ; checkcast #9 ; astore_2 ; return
        let insns = [
            with(0xb8, &[0, 7]),
            plain(0x4c),
            plain(0x01),
            with(0xc0, &[0, 9]),
            plain(0x4d),
            plain(0xb1),
        ];
        let types = analyze(&insns, &[object("Main")], &[], &pool);
        let at_return = types.before(5).expect("reachable");
        assert_eq!(at_return.local(0), &reference("Main"));
        assert_eq!(at_return.local(1), &reference("java/lang/String"));
        assert_eq!(at_return.local(2), &reference("java/lang/Integer"));
        assert!(at_return.stack.is_empty());
    }

    #[test]
    fn the_operand_stack_is_tracked_under_a_call() {
        let mut pool = FakePool::default();
        pool.fields.insert(3, "I");
        // aload_0 ; getfield #3 ; aload_0 ; return   — stack under the last aload is [int]
        let insns = [plain(0x2a), with(0xb4, &[0, 3]), plain(0x2a), plain(0xb1)];
        let types = analyze(&insns, &[object("Main")], &[], &pool);
        assert_eq!(
            types.before(3).expect("reachable").stack,
            [VerificationType::Integer, reference("Main")]
        );
    }

    /// The loop rewrites slot 1 with a different reference on the back edge; with no frame at the
    /// head, the head's state is what the two edges agree on.
    #[test]
    fn a_reference_rewritten_around_a_loop_meets_at_object() {
        let mut pool = FakePool::default();
        pool.methods.insert(5, "()Ljava/lang/Integer;");
        // 0: aconst_null ; 1: astore_1
        // 2: iload_2 ; 3: ifeq 7
        // 4: invokestatic #5 ; 5: astore_1 ; 6: goto 2
        // 7: return
        let insns = [
            plain(0x01),
            plain(0x4c),
            plain(0x1c),
            branch(0x99, 7),
            with(0xb8, &[0, 5]),
            plain(0x4c),
            branch(0xa7, 2),
            plain(0xb1),
        ];
        let entry = [object("Main"), VerifType::Top, VerifType::Integer];
        let types = analyze(&insns, &entry, &[], &pool);
        // Entering the loop the first time the slot holds null; the back edge brings an Integer;
        // null meets a reference at that reference.
        assert_eq!(
            types.before(2).expect("head").local(1),
            &reference("java/lang/Integer")
        );
        assert_eq!(
            types.before(7).expect("exit").local(1),
            &reference("java/lang/Integer")
        );
    }

    #[test]
    fn two_references_meet_at_object_and_two_primitives_meet_at_top() {
        let mut pool = FakePool::default();
        pool.methods.insert(5, "()Ljava/lang/Integer;");
        pool.methods.insert(6, "()Ljava/lang/String;");
        // 0: iload_2 ; 1: ifeq 5
        // 2: invokestatic #5 ; 3: astore_1 ; 4: goto 7
        // 5: invokestatic #6 ; 6: astore_1
        // 7: return
        let insns = [
            plain(0x1c),
            branch(0x99, 5),
            with(0xb8, &[0, 5]),
            plain(0x4c),
            branch(0xa7, 7),
            with(0xb8, &[0, 6]),
            plain(0x4c),
            plain(0xb1),
        ];
        let entry = [object("Main"), VerifType::Top, VerifType::Integer];
        let types = analyze(&insns, &entry, &[], &pool);
        assert_eq!(
            types.before(7).expect("join").local(1),
            &reference("java/lang/Object")
        );

        // iload_2 ; ifeq 4 ; iconst_0 ; istore_1 ; goto 6 ; fconst_0 ; fstore_1 ; return
        let insns = [
            plain(0x1c),
            branch(0x99, 5),
            plain(0x03),
            plain(0x3c),
            branch(0xa7, 7),
            plain(0x0b),
            plain(0x44),
            plain(0xb1),
        ];
        let types = analyze(&insns, &entry, &[], &pool);
        assert_eq!(
            types.before(7).expect("join").local(1),
            &VerificationType::Top
        );
    }

    /// A recorded frame is the state the verifier holds at its position, whatever arrives.
    #[test]
    fn a_recorded_frame_replaces_the_computed_state_at_its_position() {
        let mut pool = FakePool::default();
        pool.methods.insert(5, "()Ljava/lang/Integer;");
        // 0: invokestatic #5 ; 1: astore_1 ; 2: nop ; 3: return
        let insns = [with(0xb8, &[0, 5]), plain(0x4c), plain(0x00), plain(0xb1)];
        let entry = [object("Main")];
        let frames = [(2, vec![object("Main"), object("java/lang/Number")], vec![])];
        let types = analyze(&insns, &entry, &frames, &pool);
        assert_eq!(
            types.before(1).expect("before").local(1),
            &VerificationType::Top
        );
        assert_eq!(
            types.before(3).expect("after").local(1),
            &reference("java/lang/Number")
        );
    }

    #[test]
    fn a_long_occupies_two_slots_and_a_store_into_its_second_word_destroys_it() {
        let pool = FakePool::default();
        // lconst_0 ; lstore_1 ; iconst_0 ; istore_2 ; return
        let insns = [
            plain(0x09),
            plain(0x40),
            plain(0x03),
            plain(0x3d),
            plain(0xb1),
        ];
        let types = analyze(&insns, &[object("Main")], &[], &pool);
        let after_long = types.before(2).expect("reachable");
        assert_eq!(after_long.local(1), &VerificationType::Long);
        assert_eq!(after_long.local(2), &VerificationType::Top);
        let after_int = types.before(4).expect("reachable");
        assert_eq!(after_int.local(1), &VerificationType::Top);
        assert_eq!(after_int.local(2), &VerificationType::Integer);
    }

    #[test]
    fn a_handler_is_entered_with_the_exception_alone_on_the_stack() {
        let mut pool = FakePool::default();
        pool.methods.insert(5, "()Ljava/lang/Integer;");
        // 0: invokestatic #5 ; 1: astore_1 ; 2: return ; 3: astore_2 ; 4: return
        let insns = [
            with(0xb8, &[0, 5]),
            plain(0x4c),
            plain(0xb1),
            plain(0x4d),
            plain(0xb1),
        ];
        let handlers = [Handler {
            start: 0,
            end: 2,
            handler: 3,
        }];
        let graph = ControlGraph::build(&insns, &handlers).expect("graph");
        let types =
            FrameTypes::analyze(&insns, &graph, &[object("Main")], &[], &pool).expect("types");
        let at_handler = types.before(3).expect("reachable");
        assert_eq!(at_handler.stack, [reference("java/lang/Throwable")]);
        // Slot 1 is stored inside the range, so the throw may precede it: unusable in the handler.
        assert_eq!(at_handler.local(1), &VerificationType::Top);
        assert_eq!(
            types.before(4).expect("reachable").local(2),
            &reference("java/lang/Throwable")
        );
    }

    #[test]
    fn an_object_is_uninitialized_until_its_constructor_runs() {
        let mut pool = FakePool::default();
        pool.classes.insert(4, "java/lang/StringBuilder");
        pool.methods.insert(6, "()V");
        // 0: new #4 ; 1: dup ; 2: invokespecial #6 ; 3: astore_1 ; 4: return
        let insns = [
            with(0xbb, &[0, 4]),
            plain(0x59),
            with(0xb7, &[0, 6]),
            plain(0x4c),
            plain(0xb1),
        ];
        let types = analyze(&insns, &[object("Main")], &[], &pool);
        assert_eq!(
            types.before(2).expect("reachable").stack,
            [
                VerificationType::Uninitialized(0),
                VerificationType::Uninitialized(0)
            ]
        );
        assert_eq!(
            types.before(3).expect("reachable").stack,
            [reference("java/lang/StringBuilder")]
        );
    }

    #[test]
    fn dup2_x2_moves_a_long_under_another_long() {
        let pool = FakePool::default();
        // lconst_0 ; lconst_1 ; dup2_x2 ; return   — [L0, L1] -> [L1, L0, L1]
        let insns = [plain(0x09), plain(0x0a), plain(0x5e), plain(0xb1)];
        let types = analyze(&insns, &[], &[], &pool);
        assert_eq!(
            types.before(3).expect("reachable").stack,
            [
                VerificationType::Long,
                VerificationType::Long,
                VerificationType::Long
            ]
        );
        // dup_x2 with a long under the top: iconst_0 ; lconst_0 ; swap is illegal, so build
        // [L, I] and dup_x2 → [I, L, I]
        let insns = [plain(0x09), plain(0x03), plain(0x5b), plain(0xb1)];
        let types = analyze(&insns, &[], &[], &pool);
        assert_eq!(
            types.before(3).expect("reachable").stack,
            [
                VerificationType::Integer,
                VerificationType::Long,
                VerificationType::Integer
            ]
        );
    }

    #[test]
    fn a_wide_store_and_load_name_the_same_slot_as_their_compact_forms() {
        let pool = FakePool::default();
        let wide = |inner: u8, slot: u16| with(0xc4, &[inner, (slot >> 8) as u8, slot as u8]);
        // aconst_null ; wide astore 300 ; wide aload 300 ; wide istore 301 is illegal; use return
        let insns = [plain(0x01), wide(0x3a, 300), wide(0x19, 300), plain(0xb1)];
        let types = analyze(&insns, &[], &[], &pool);
        assert_eq!(
            types.before(2).expect("reachable").local(300),
            &VerificationType::Null
        );
        assert_eq!(
            types.before(3).expect("reachable").stack,
            [VerificationType::Null]
        );
    }

    #[test]
    fn a_constructor_call_retypes_the_uninitialized_copy_held_in_a_local() {
        let mut pool = FakePool::default();
        pool.classes.insert(4, "java/lang/StringBuilder");
        pool.methods.insert(6, "()V");
        // new #4 ; dup ; astore_1 ; invokespecial #6 ; return
        let insns = [
            with(0xbb, &[0, 4]),
            plain(0x59),
            plain(0x4c),
            with(0xb7, &[0, 6]),
            plain(0xb1),
        ];
        let types = analyze(&insns, &[object("Main")], &[], &pool);
        assert_eq!(
            types.before(3).expect("reachable").local(1),
            &VerificationType::Uninitialized(0)
        );
        assert_eq!(
            types.before(4).expect("reachable").local(1),
            &reference("java/lang/StringBuilder")
        );
    }

    /// The shape `fold` depends on: the loop head carries a recorded frame claiming `Object` for
    /// the accumulator, and that — not the computed meet of the entry and back edges — is what the
    /// body is held to.
    #[test]
    fn a_recorded_frame_at_a_loop_head_is_what_the_body_is_held_to() {
        let mut pool = FakePool::default();
        pool.methods.insert(5, "()Ljava/lang/Integer;");
        // 0: invokestatic #5 ; 1: astore_1
        // 2: iload_2 ; 3: ifeq 7        <- frame: slot 1 = Object
        // 4: invokestatic #5 ; 5: astore_1 ; 6: goto 2
        // 7: return
        let insns = [
            with(0xb8, &[0, 5]),
            plain(0x4c),
            plain(0x1c),
            branch(0x99, 7),
            with(0xb8, &[0, 5]),
            plain(0x4c),
            branch(0xa7, 2),
            plain(0xb1),
        ];
        let entry = [object("Main"), VerifType::Top, VerifType::Integer];
        let frames = [(
            2,
            vec![
                object("Main"),
                object("java/lang/Object"),
                VerifType::Integer,
            ],
            vec![],
        )];
        let types = analyze(&insns, &entry, &frames, &pool);
        assert_eq!(
            types.before(4).expect("body").local(1),
            &reference("java/lang/Object")
        );
        assert_eq!(
            types.before(7).expect("exit").local(1),
            &reference("java/lang/Object")
        );
    }

    #[test]
    fn unreachable_code_has_no_state() {
        let pool = FakePool::default();
        // goto 2 ; nop ; return
        let insns = [branch(0xa7, 2), plain(0x00), plain(0xb1)];
        let types = analyze(&insns, &[], &[], &pool);
        assert!(types.before(1).is_none());
        assert!(types.before(2).is_some());
    }

    #[test]
    fn an_array_load_yields_the_element_type() {
        let mut pool = FakePool::default();
        pool.classes.insert(2, "java/lang/String");
        // iconst_1 ; anewarray #2 ; iconst_0 ; aaload ; astore_1 ; return
        let insns = [
            plain(0x04),
            with(0xbd, &[0, 2]),
            plain(0x03),
            plain(0x32),
            plain(0x4c),
            plain(0xb1),
        ];
        let types = analyze(&insns, &[object("Main")], &[], &pool);
        assert_eq!(
            types.before(2).expect("reachable").stack,
            [reference("[Ljava/lang/String;")]
        );
        assert_eq!(
            types.before(5).expect("reachable").local(1),
            &reference("java/lang/String")
        );
    }
}
