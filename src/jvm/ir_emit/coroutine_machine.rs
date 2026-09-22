//! The coroutine state machine for a suspend function whose only suspension is inside a body the
//! emitter splices.
//!
//! The IR machine cannot build one for these: at the point it runs, the suspension is inside a
//! lambda it treats as a closure boundary, and the locals that must be spilled do not exist until
//! the inline body has been spliced. See `docs/JVM_INLINE_BEFORE_CPS.md`.
//!
//! So the machine is built during emission, in two passes over the same body. The first emits it
//! with no machine at all, which is the post-splice bytecode the spill set has to be read from; the
//! second emits the real method around the answers.

use std::collections::HashMap;

use crate::jvm::classfile::VerifType;
use crate::jvm::ir_emit::{ir_ty_to_jvm, slot_words};
use crate::types::Ty;

/// Where the machine keeps its own state. Reserved above the parameters and below the body's own
/// locals, so a frame built from the emitter's slot view describes them without being told to.
#[derive(Clone, Copy, Debug)]
pub(super) struct MachineSlots {
    pub(super) result: u16,
    pub(super) continuation: u16,
    pub(super) suspended: u16,
}

impl MachineSlots {
    fn owns(&self, slot: u16) -> bool {
        slot == self.result || slot == self.continuation || slot == self.suspended
    }
}

/// One suspension's spill plan: the locals that must survive it.
#[derive(Clone, Debug, Default)]
pub(super) struct SuspensionPlan {
    /// `(slot, type)` of every local live across the suspension, ascending by slot. The slots a
    /// stack prefix is saved into are part of this: once saved they are locals like any other.
    ///
    /// A slot the verifier holds as `null` — `Ty::Null` — owns no field: its value is a constant,
    /// the resume rematerializes it with `aconst_null`, and the join claims `null` for it. Storing
    /// it in an `Object` field would restore it as `Object`, which is wider than what the code
    /// after the join was verified against.
    pub(super) spills: Vec<(u16, Ty)>,
    /// The operand-stack values the dependency holds UNDER this suspension, bottom-first, each with
    /// the slot it is saved into. `areturn` discards the stack, so they are stored before the call
    /// and pushed back on both paths out of it.
    pub(super) prefix: Vec<(u16, Ty)>,
}

/// What the discovery pass learned about one suspend function.
#[derive(Clone, Debug, Default)]
pub(super) struct MachinePlan {
    /// One entry per suspension, in the order the markers were found.
    pub(super) suspensions: Vec<SuspensionPlan>,
    /// One past the highest slot the body itself uses; the machine's own locals go above it.
    pub(super) body_locals: u16,
}

impl MachinePlan {
    /// The continuation field holding spill `index`, by representation kind.
    pub(super) fn spill_field(kind: char, index: usize) -> String {
        format!("{kind}${index}")
    }
}

/// Plans discovered per function, keyed by `FunId`.
pub(super) type MachinePlans = HashMap<u32, MachinePlan>;

/// The JVM representation kind a spilled local is stored under, matching the continuation field
/// families the reference compiler uses (`I$0`, `L$0`, …).
pub(super) fn spill_kind(ty: Ty) -> char {
    match crate::jvm::ir_emit::ir_ty_to_jvm(&ty) {
        Ty::Int | Ty::Short | Ty::Byte | Ty::Char | Ty::Boolean => 'I',
        Ty::Long => 'J',
        Ty::Float => 'F',
        Ty::Double => 'D',
        _ => 'L',
    }
}

/// The Kotlin type a verification type stands for, as far as a spill needs to know.
pub(super) fn verif_spill_type(v: &VerifType) -> Option<Ty> {
    Some(match v {
        VerifType::Integer => Ty::Int,
        VerifType::Long => Ty::Long,
        VerifType::Float => Ty::Float,
        VerifType::Double => Ty::Double,
        VerifType::ObjectName(name) => Ty::obj(name.as_str()),
        VerifType::Null => Ty::Null,
        // `Top` is an unassigned slot and `UninitializedThis` cannot be spilled.
        _ => return None,
    })
}

/// Read one function's spill plan off the bytecode the discovery pass emitted.
///
/// The markers say where each suspension landed — a position no offset recorded before the splice
/// could have predicted. Around each one, the locals live across it are the spill set, and their
/// types are what the verifier holds there: computed forward over the spliced body from `entry`,
/// the method's own entry locals, and the frames it carries (see [`FrameTypes`]).
///
/// `None` when the body cannot be analyzed or a value that must survive cannot be typed: the
/// machine then declines and the caller bails exactly as it did before.
pub(super) fn discover(
    code: &crate::jvm::classfile::CodeBuilder,
    machine: Option<MachineSlots>,
    entry: &[VerifType],
    cw: &crate::jvm::classfile::ClassWriter,
    expected: usize,
) -> Option<MachinePlan> {
    use crate::jvm::classfile::CoroutineMarker;
    use crate::jvm::suspend::cps::{ControlGraph, FrameTypes, Handler, LocalLiveness};

    let markers: Vec<(usize, u16)> = code
        .marker_positions()?
        .into_iter()
        .filter(|(_, kind, _)| *kind == CoroutineMarker::Suspension)
        .map(|(at, _, ordinal)| (at, ordinal))
        .collect();
    crate::trace_compiler!(
        "suspend",
        "discover: {} marker(s) ordinals={:?}",
        markers.len(),
        markers
            .iter()
            .map(|(_, ordinal)| *ordinal)
            .collect::<Vec<_>>()
    );
    if markers.is_empty() {
        return None;
    }
    // One marker per suspension, each with its own ordinal. An inline function that invokes its
    // lambda at two sites receives one body per site, each marked with its own ordinals; were the
    // same body spliced twice, one ordinal would be marked twice and the plan would describe one
    // site with the other's locals. That no longer happens by construction, and this check is the
    // safety net that keeps a machine resuming at the wrong position from ever reaching a class
    // file. A missing ordinal is the mirror case — the state would have no position, and its
    // spills would never be emitted.
    let mut marked: Vec<bool> = vec![false; expected];
    for &(_, ordinal) in &markers {
        let Some(seen) = marked.get_mut(ordinal as usize) else {
            crate::trace_compiler!("suspend", "discover: ordinal {ordinal} is not a state");
            return None;
        };
        if std::mem::replace(seen, true) {
            crate::trace_compiler!(
                "suspend",
                "discover: ordinal {ordinal} marked more than once"
            );
            return None;
        }
    }
    if let Some(missing) = marked.iter().position(|seen| !seen) {
        crate::trace_compiler!("suspend", "discover: state {missing} was never emitted");
        return None;
    }
    // The builder's own bytes hold a placeholder for every branch not yet linked; the control
    // flow read here has to be the method's.
    let Some(bytes) = code.resolved_bytes() else {
        crate::trace_compiler!("suspend", "discover: a branch destination is unbound");
        return None;
    };
    let Some(insns) = crate::jvm::inline::disassemble(&bytes) else {
        crate::trace_compiler!("suspend", "discover: disassemble failed");
        return None;
    };
    let offsets = crate::jvm::inline::insn_offsets_at(&insns, 0);
    let index_of = |offset: usize| offsets.iter().position(|&at| at == offset);
    let Some(regions) = code
        .resolved_exceptions()
        .iter()
        .map(|&(start, end, handler, _)| {
            Some(Handler {
                start: index_of(start as usize)?,
                end: index_of(end as usize)?,
                handler: index_of(handler as usize)?,
            })
        })
        .collect::<Option<Vec<_>>>()
    else {
        crate::trace_compiler!(
            "suspend",
            "discover: an exception-table boundary is not an instruction offset"
        );
        return None;
    };
    let Some(graph) = ControlGraph::build(&insns, &regions) else {
        crate::trace_compiler!("suspend", "discover: control graph declined");
        return None;
    };
    let Some(liveness) = LocalLiveness::analyze(&insns, &graph) else {
        crate::trace_compiler!("suspend", "discover: liveness declined");
        return None;
    };
    // The frames this body carries, by instruction index and slot: the splice relocated the
    // dependency's own, and this emitter recorded one at every label it bound.
    //
    // MERGED by offset, because that is what the class file will carry: several labels can be bound
    // at one offset — a loop's `end` and the following statement's `start` — and the verifier holds
    // the merge of their frames there, not any one of them. Reading them per label handed this
    // analysis whichever was registered last, which can name a local the merge drops; the spill
    // planned from it then loads a slot the verifier has as `top`.
    let Some(frames) = cw
        .merged_frames(code)
        .into_iter()
        .map(|(at, locals, stack)| Some((index_of(at)?, expand_slots(&locals), stack)))
        .collect::<Option<Vec<(usize, Vec<VerifType>, Vec<VerifType>)>>>()
    else {
        crate::trace_compiler!(
            "suspend",
            "discover: a frame is not at an instruction offset"
        );
        return None;
    };
    let Some(types) = FrameTypes::analyze(&insns, &graph, entry, &frames, cw) else {
        crate::trace_compiler!("suspend", "discover: frame analysis declined");
        return None;
    };

    let mut suspensions = vec![SuspensionPlan::default(); expected];
    for (at, ordinal) in markers {
        let index = index_of(at)?;
        let Some(state) = types.before(index) else {
            crate::trace_compiler!(
                "suspend",
                "discover: marker {ordinal} at {at} is unreachable"
            );
            return None;
        };
        crate::trace_compiler!(
            "suspend",
            "discover: marker {ordinal} at {at} locals={:?} stack={:?}",
            state.locals,
            state.stack
        );
        // What the resume has to put back is not only what liveness calls live across the call:
        // every frame the body can reach from the join claims the locals it holds there, whether
        // or not the body reads them again, and one such frame types but the resume leaves unset
        // fails verification at the edge into it. A slot the verifier holds as `top` here cannot
        // be claimed by any of them without a store in between, so it needs no restoring.
        let mut slots: Vec<u16> = liveness.live_before(index).iter().collect();
        for slot in claimed_from(&graph, index, &frames) {
            if !slots.contains(&slot) {
                slots.push(slot);
            }
        }
        slots.sort_unstable();
        let mut spills = Vec::new();
        // A `long`/`double` occupies two slots and the liveness set holds both. It is spilled once,
        // under its first word; the second is the same value and has no type of its own (the
        // verifier holds it as `top`).
        for slot in slots {
            // The machine's own locals are not spilled: they hold the state that survives the
            // suspension rather than anything the body needs restored.
            if machine.is_some_and(|machine| machine.owns(slot)) {
                continue;
            }
            let held = state.local(slot);
            if matches!(
                held,
                crate::jvm::suspend::cps::VerificationType::Uninitialized(_)
            ) {
                crate::trace_compiler!(
                    "suspend",
                    "discover: slot {slot} holds an unconstructed object at {at}"
                );
                return None;
            }
            let Some(ty) = verif_spill_type(&held.to_verif()) else {
                continue;
            };
            spills.push((slot, ty));
        }
        // What the body holds on the operand stack under this call — the dependency's own values,
        // `acc` in `acc = acc + f(x)` — is saved into slots ABOVE everything the body uses, so it
        // cannot collide with a local. It is saved per suspension, since only one state is ever
        // live at a time. `areturn` discards the stack, so a value that cannot be typed cannot be
        // carried, and the machine declines.
        let mut prefix = Vec::new();
        let mut slot = code.max_locals;
        for value in &state.stack {
            let Some(ty) = verif_spill_type(&value.to_verif()) else {
                crate::trace_compiler!(
                    "suspend",
                    "discover: operand {value:?} under the suspension at {at} cannot be carried"
                );
                return None;
            };
            prefix.push((slot, ty));
            slot += slot_words(ir_ty_to_jvm(&ty));
        }
        spills.extend(prefix.iter().copied());
        spills.sort_by_key(|&(slot, _)| slot);
        *suspensions.get_mut(ordinal as usize)? = SuspensionPlan { spills, prefix };
    }
    Some(MachinePlan {
        suspensions,
        body_locals: code.max_locals,
    })
}

/// The slots some frame reachable from instruction `from` claims a type for.
fn claimed_from(
    graph: &crate::jvm::suspend::cps::ControlGraph,
    from: usize,
    frames: &[(usize, Vec<VerifType>, Vec<VerifType>)],
) -> Vec<u16> {
    let mut seen = vec![false; graph.exit() + 1];
    let mut pending = vec![from];
    while let Some(index) = pending.pop() {
        if index > graph.exit() || std::mem::replace(&mut seen[index], true) {
            continue;
        }
        pending.extend_from_slice(graph.normal_successors(index));
        pending.extend_from_slice(graph.exceptional_successors(index));
    }
    let mut claimed = Vec::new();
    for (index, locals, _) in frames {
        if !seen.get(*index).copied().unwrap_or(false) {
            continue;
        }
        for (slot, typed_as) in locals.iter().enumerate() {
            let described = !matches!(typed_as, VerifType::Top | VerifType::UninitializedThis);
            let slot = slot as u16;
            if described && !claimed.contains(&slot) {
                claimed.push(slot);
            }
        }
    }
    claimed
}

/// Verification types indexed by SLOT: a `long`/`double` is one entry in a frame and two slots.
fn expand_slots(locals: &[VerifType]) -> Vec<VerifType> {
    let mut slots = Vec::with_capacity(locals.len());
    for v in locals {
        slots.push(v.clone());
        if matches!(v, VerifType::Long | VerifType::Double) {
            slots.push(VerifType::Top);
        }
    }
    slots
}

/// The continuation class a machine keeps its state in: `<facade>$<function>$1`.
pub(super) fn continuation_internal(owner: &str, function: &str, ordinal: usize) -> String {
    crate::jvm::suspend::continuation_class_name(owner, function, ordinal)
}

const CONTINUATION_IMPL: &str = "kotlin/coroutines/jvm/internal/ContinuationImpl";
const CONTINUATION: &str = "kotlin/coroutines/Continuation";

/// The descriptor a spilled local is stored under.
fn spill_descriptor(kind: char) -> &'static str {
    match kind {
        'I' => "I",
        'J' => "J",
        'F' => "F",
        'D' => "D",
        _ => "Ljava/lang/Object;",
    }
}

/// Every spill field the plan needs, as `(name, descriptor)`, grouped by representation kind in the
/// order the kinds are first spilled — the field families `I$0`, `L$0`, … name.
pub(super) fn spill_fields(plan: &MachinePlan) -> Vec<(String, &'static str)> {
    let mut counts: Vec<(char, usize)> = Vec::new();
    for suspension in &plan.suspensions {
        let mut here: Vec<(char, usize)> = Vec::new();
        for (_, ty) in &suspension.spills {
            if *ty == Ty::Null {
                continue;
            }
            let kind = spill_kind(*ty);
            match here.iter_mut().find(|(k, _)| *k == kind) {
                Some((_, n)) => *n += 1,
                None => here.push((kind, 1)),
            }
        }
        for (kind, n) in here {
            match counts.iter_mut().find(|(k, _)| *k == kind) {
                Some((_, most)) => *most = (*most).max(n),
                None => counts.push((kind, n)),
            }
        }
    }
    let mut fields = Vec::new();
    for (kind, n) in counts {
        for index in 0..n {
            fields.push((
                MachinePlan::spill_field(kind, index),
                spill_descriptor(kind),
            ));
        }
    }
    fields
}

/// The slots a suspension restores as a constant `null` rather than from a field.
pub(super) fn constant_null_slots(suspension: &SuspensionPlan) -> Vec<u16> {
    suspension
        .spills
        .iter()
        .filter(|(_, ty)| *ty == Ty::Null)
        .map(|&(slot, _)| slot)
        .collect()
}

/// Which field each spill of one suspension is stored in. A constant-`null` slot has none.
pub(super) fn suspension_fields(
    suspension: &SuspensionPlan,
) -> Vec<(u16, Ty, String, &'static str)> {
    let mut next: Vec<(char, usize)> = Vec::new();
    let mut placed = Vec::new();
    for &(slot, ty) in &suspension.spills {
        if ty == Ty::Null {
            continue;
        }
        let kind = spill_kind(ty);
        let index = match next.iter_mut().find(|(k, _)| *k == kind) {
            Some((_, n)) => {
                let at = *n;
                *n += 1;
                at
            }
            None => {
                next.push((kind, 1));
                0
            }
        };
        placed.push((
            slot,
            ty,
            MachinePlan::spill_field(kind, index),
            spill_descriptor(kind),
        ));
    }
    placed
}

/// Everything the emitting pass needs to build one function's machine.
#[derive(Clone, Debug)]
pub(super) struct Machine {
    pub(super) plan: MachinePlan,
    pub(super) slots: MachineSlots,
    /// Internal name of the continuation class this machine keeps its state in.
    pub(super) internal: String,
    /// `Some(owner)` for an instance method: the continuation holds the receiver, because
    /// re-entering the method needs one.
    pub(super) receiver: Option<String>,
    /// `Some(name)` when re-entry goes through a synthetic static on the owner: a PRIVATE method is
    /// not callable from the continuation class, exactly as for kotlinc's `access$<name>`.
    pub(super) bridge: Option<String>,
}

/// The synthetic static a continuation re-enters a private method through.
pub(super) fn access_bridge_name(function: &str) -> String {
    format!("access${function}")
}

/// Build kotlinc's `access$<name>`: take the receiver and every parameter, call the private method
/// with `invokespecial` — the only legal form for a private member — and hand back its result.
pub(super) fn build_access_bridge(
    cw: &mut crate::jvm::classfile::ClassWriter,
    owner: &str,
    function: &str,
    descriptor: &str,
    instance: bool,
) -> Option<(String, String, crate::jvm::classfile::CodeBuilder)> {
    use crate::jvm::classfile::CodeBuilder;
    let (params, ret) = split_descriptor(descriptor)?;
    // Every parameter, and the receiver first when there is one: a bridge declares exactly the
    // slots it is handed.
    let locals = usize::from(instance)
        + params
            .iter()
            .map(|p| usize::from(matches!(p.as_bytes().first(), Some(b'J') | Some(b'D'))) + 1)
            .sum::<usize>();
    let mut code = CodeBuilder::new(locals as u16);
    let mut slot = 0u16;
    if instance {
        code.aload(0);
        slot = 1;
    }
    for parameter in &params {
        match parameter.as_bytes().first() {
            Some(b'J') => {
                code.lload(slot);
                slot += 2;
            }
            Some(b'D') => {
                code.dload(slot);
                slot += 2;
            }
            Some(b'F') => {
                code.fload(slot);
                slot += 1;
            }
            Some(b'L') | Some(b'[') => {
                code.aload(slot);
                slot += 1;
            }
            _ => {
                code.iload(slot);
                slot += 1;
            }
        }
    }
    let target = cw.methodref(owner, function, descriptor);
    let returns = i32::from(ret != "V");
    // `invokespecial` is the only legal form for a private instance method; a private static is
    // called the way any static is.
    match instance {
        true => code.invokespecial(target, i32::from(slot), returns),
        false => code.invokestatic(target, i32::from(slot), returns),
    }
    match ret.as_str() {
        "V" => code.ret_void(),
        "J" => code.lreturn(),
        "D" => code.dreturn(),
        "F" => code.freturn(),
        "I" | "Z" | "B" | "C" | "S" => code.ireturn(),
        _ => code.areturn(),
    }
    let bridge_descriptor = match instance {
        true => format!("(L{owner};{}", &descriptor[1..]),
        false => descriptor.to_string(),
    };
    Some((access_bridge_name(function), bridge_descriptor, code))
}

/// A descriptor's parameter list and return descriptor, as written.
fn split_descriptor(descriptor: &str) -> Option<(Vec<String>, String)> {
    let (mut params, ret) = parse_outer_descriptor(descriptor)?;
    // `parse_outer_descriptor` drops the trailing continuation, which a bridge must pass on.
    params.push(format!("L{CONTINUATION};"));
    Some((params, ret))
}

/// Build the continuation class a machine keeps its state in.
///
/// `<facade>$<function>$1 extends ContinuationImpl`, holding the resumed value, the state label and
/// one field per spilled local. Its `invokeSuspend` stores the value, sets the resume bit and
/// re-enters the function, which then dispatches to the state it stopped in.
pub(super) struct ContinuationClass<'a> {
    /// Internal name of the class to build.
    pub(super) internal: &'a str,
    /// The class holding the method this continuation re-enters, and that method's name and
    /// descriptor.
    pub(super) outer: &'a str,
    pub(super) outer_method: &'a str,
    pub(super) outer_descriptor: &'a str,
    pub(super) plan: &'a MachinePlan,
    /// Class-file major version, matched to the method's own.
    pub(super) major: Option<u16>,
    pub(super) source_file: Option<&'a str>,
    /// `Some(owner)` for an instance method, whose receiver the continuation keeps.
    pub(super) receiver: Option<&'a str>,
    /// `Some(name)` when re-entry goes through a synthetic static on the owner.
    pub(super) bridge: Option<&'a str>,
}

pub(super) fn build_continuation_class(spec: ContinuationClass<'_>) -> Vec<u8> {
    let ContinuationClass {
        internal,
        outer,
        outer_method,
        outer_descriptor,
        plan,
        major,
        source_file,
        receiver,
        bridge,
    } = spec;
    use crate::jvm::classfile::{ClassWriter, CodeBuilder, ACC_FINAL, ACC_PUBLIC};
    let mut cw = ClassWriter::new(internal, CONTINUATION_IMPL);
    if let Some(major) = major {
        cw.set_major(major);
    }
    cw.set_source_file(source_file.map(str::to_string));
    for (name, descriptor) in spill_fields(plan) {
        cw.add_field(0, &name, descriptor);
    }
    cw.add_field(0, "result", "Ljava/lang/Object;");
    // kotlinc's own order for an instance method: the resumed value, the receiver, then the label.
    if let Some(owner) = receiver {
        cw.add_field(ACC_FINAL, "this$0", &format!("L{owner};"));
    }
    cw.add_field(0, "label", "I");

    // `<init>` hands the completion straight to ContinuationImpl, keeping the receiver first when
    // there is one.
    let ctor_descriptor = match receiver {
        Some(owner) => format!("(L{owner};L{CONTINUATION};)V"),
        None => format!("(L{CONTINUATION};)V"),
    };
    let mut ctor = CodeBuilder::new(if receiver.is_some() { 3 } else { 2 });
    if let Some(owner) = receiver {
        let this = cw.fieldref(internal, "this$0", &format!("L{owner};"));
        ctor.aload(0);
        ctor.aload(1);
        ctor.putfield(this, 1);
    }
    ctor.aload(0);
    ctor.aload(if receiver.is_some() { 2 } else { 1 });
    let super_ctor = cw.methodref(CONTINUATION_IMPL, "<init>", &format!("(L{CONTINUATION};)V"));
    ctor.invokespecial(super_ctor, 2, 0);
    ctor.ret_void();
    cw.add_method(0, "<init>", &ctor_descriptor, &ctor);

    // `invokeSuspend(Object)`: keep the value, mark the machine as resuming, and re-enter it.
    let mut invoke = CodeBuilder::new(2);
    invoke.aload(0);
    invoke.aload(1);
    let result = cw.fieldref(internal, "result", "Ljava/lang/Object;");
    invoke.putfield(result, 1);
    invoke.aload(0);
    invoke.aload(0);
    let label = cw.fieldref(internal, "label", "I");
    invoke.getfield(label, 1);
    invoke.push_int(i32::MIN, &mut cw);
    invoke.ior();
    invoke.putfield(label, 1);
    // Every value parameter is passed as a zero: the machine restores the real ones from the fields
    // above before it reads any of them. An instance method is re-entered on the receiver the
    // continuation kept.
    let mut words = 0;
    if let Some(owner) = receiver {
        let this = cw.fieldref(internal, "this$0", &format!("L{owner};"));
        invoke.aload(0);
        invoke.getfield(this, 1);
        words += 1;
    }
    if let Some((parameters, _)) = parse_outer_descriptor(outer_descriptor) {
        for parameter in &parameters {
            push_zero_descriptor(&mut invoke, parameter, &mut cw);
            words += if matches!(parameter.as_str(), "J" | "D") {
                2
            } else {
                1
            };
        }
    }
    invoke.aload(0);
    let class = cw.class_ref(CONTINUATION);
    invoke.checkcast(class);
    words += 1;
    match (bridge, receiver) {
        // The bridge is static and takes the receiver as its first argument, which is already on
        // the stack.
        (Some(bridge), Some(owner)) => {
            let descriptor = format!("(L{owner};{}", &outer_descriptor[1..]);
            let reference = cw.methodref(outer, bridge, &descriptor);
            invoke.invokestatic(reference, words, 1);
        }
        // A private static: same arguments as the method itself, through a static the continuation
        // class is allowed to name.
        (Some(bridge), None) => {
            let reference = cw.methodref(outer, bridge, outer_descriptor);
            invoke.invokestatic(reference, words, 1);
        }
        (None, Some(_)) => {
            let reference = cw.methodref(outer, outer_method, outer_descriptor);
            invoke.invokevirtual(reference, words, 1);
        }
        (None, None) => {
            let reference = cw.methodref(outer, outer_method, outer_descriptor);
            invoke.invokestatic(reference, words, 1);
        }
    }
    invoke.areturn();
    cw.add_method(
        ACC_PUBLIC | ACC_FINAL,
        "invokeSuspend",
        "(Ljava/lang/Object;)Ljava/lang/Object;",
        &invoke,
    );
    cw.finish()
}

/// The outer method's parameter descriptors, excluding its trailing continuation.
fn parse_outer_descriptor(descriptor: &str) -> Option<(Vec<String>, String)> {
    let inner = descriptor.strip_prefix('(')?;
    let end = inner.find(')')?;
    let (params, ret) = (&inner[..end], &inner[end + 1..]);
    let mut out = Vec::new();
    let bytes = params.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        let start = at;
        while bytes.get(at) == Some(&b'[') {
            at += 1;
        }
        if bytes.get(at) == Some(&b'L') {
            while bytes.get(at) != Some(&b';') {
                at += 1;
            }
        }
        at += 1;
        out.push(params[start..at].to_string());
    }
    // The trailing continuation is supplied by the machine itself.
    out.pop();
    Some((out, ret.to_string()))
}

/// Push a zero of the shape `descriptor` names.
fn push_zero_descriptor(
    code: &mut crate::jvm::classfile::CodeBuilder,
    descriptor: &str,
    cw: &mut crate::jvm::classfile::ClassWriter,
) {
    match descriptor.as_bytes().first() {
        Some(b'J') => code.push_long(0, cw),
        Some(b'F') => code.push_float(0.0, cw),
        Some(b'D') => code.push_double(0.0, cw),
        Some(b'L') | Some(b'[') => code.aconst_null(),
        _ => code.push_int(0, cw),
    }
}
