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
    /// `(slot, type)` of every local live across the suspension, ascending by slot.
    pub(super) spills: Vec<(u16, Ty)>,
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
    /// Slot of the resumed value (`$result`).
    pub(super) fn result_slot(&self) -> u16 {
        self.body_locals
    }

    /// Slot of the machine's own continuation.
    pub(super) fn continuation_slot(&self) -> u16 {
        self.body_locals + 1
    }

    /// Slot holding `COROUTINE_SUSPENDED`, compared against every suspension's result.
    pub(super) fn suspended_slot(&self) -> u16 {
        self.body_locals + 2
    }

    /// One past the machine's own locals.
    pub(super) fn max_locals(&self) -> u16 {
        self.body_locals + 3
    }

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
        VerifType::Null => Ty::nullable(Ty::obj("kotlin/Any")),
        // `Top` is an unassigned slot and `UninitializedThis` cannot be spilled.
        _ => return None,
    })
}

/// Read one function's spill plan off the bytecode the discovery pass emitted.
///
/// The markers say where each suspension landed — a position no offset recorded before the splice
/// could have predicted. Around each one, the locals live across it are the spill set, and their
/// types come from the nearest frame at or before it, which is the only typed view of the frame
/// this pass has.
///
/// `None` when the body cannot be analyzed or a live local cannot be typed: the machine then
/// declines and the caller bails exactly as it did before.
pub(super) fn discover(
    code: &crate::jvm::classfile::CodeBuilder,
    allocated: &HashMap<u16, Ty>,
    machine: Option<MachineSlots>,
    at_markers: &HashMap<usize, Vec<VerifType>>,
    cw: &crate::jvm::classfile::ClassWriter,
) -> Option<MachinePlan> {
    use crate::jvm::classfile::CoroutineMarker;
    use crate::jvm::suspend::cps::{ControlGraph, Handler, LocalLiveness};

    let markers: Vec<(usize, u16)> = code
        .marker_positions()?
        .into_iter()
        .filter(|(_, kind, _)| *kind == CoroutineMarker::Suspension)
        .map(|(at, _, ordinal)| (at, ordinal))
        .collect();
    crate::trace_compiler!("suspend", "discover: {} marker(s)", markers.len());
    if markers.is_empty() {
        return None;
    }
    let Some(insns) = crate::jvm::inline::disassemble(&code.bytes) else {
        crate::trace_compiler!("suspend", "discover: disassemble failed");
        return None;
    };
    let offsets = crate::jvm::inline::insn_offsets_at(&insns, 0);
    let index_of = |offset: usize| offsets.iter().position(|&at| at == offset);
    let regions = code
        .resolved_exceptions()
        .iter()
        .map(|&(start, end, handler, _)| {
            Some(Handler {
                start: index_of(start as usize)?,
                end: index_of(end as usize)?,
                handler: index_of(handler as usize)?,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    let Some(graph) = ControlGraph::build(&insns, &regions) else {
        crate::trace_compiler!("suspend", "discover: control graph declined");
        return None;
    };
    let Some(liveness) = LocalLiveness::analyze(&insns, &graph) else {
        crate::trace_compiler!("suspend", "discover: liveness declined");
        return None;
    };

    // Frames, ascending, as the only typed view of the frame this pass has.
    let mut frames = code.resolved_frames();
    frames.sort_by_key(|(at, _, _)| *at);

    let mut suspensions = vec![SuspensionPlan::default(); markers.len()];
    for (at, ordinal) in markers {
        let index = index_of(at)?;
        let live = liveness.live_before(index);
        // Two partial views of the same frame, overlaid. A frame describes the SPLICED body's own
        // locals, which no IR declaration names; the emitter's view at the marker describes the
        // host's, including the ones assigned between frames. The merge point's frame is built from
        // the emitter's view, so a local either view types is one the resume has to restore.
        let mut typed = nearest_frame_slots(&frames, at).unwrap_or_default();
        if let Some(held) = at_markers.get(&(ordinal as usize)) {
            let held = expand_slots(held);
            if held.len() > typed.len() {
                typed.resize(held.len(), VerifType::Top);
            }
            for (slot, ty) in held.into_iter().enumerate() {
                if matches!(typed[slot], VerifType::Top) {
                    typed[slot] = ty;
                }
            }
        }
        // What the resume has to put back is not only what liveness calls live across the call: the
        // frame where the two paths meet claims every local the emitter held here, whether or not
        // the body reads it again, and one that frame types but the resume leaves unset fails
        // verification.
        let mut slots: Vec<u16> = live.iter().collect();
        for (slot, typed_as) in typed.iter().enumerate() {
            let slot = slot as u16;
            let described = !matches!(typed_as, VerifType::Top | VerifType::UninitializedThis);
            if described && !slots.contains(&slot) {
                slots.push(slot);
            }
        }
        slots.sort_unstable();
        let mut spills = Vec::new();
        // A `long`/`double` occupies two slots and the liveness set holds both. It is spilled once,
        // under its first word; the second is the same value and has no type of its own (a frame
        // describes it as `top`).
        let mut high_word: Option<u16> = None;
        for slot in slots {
            if high_word == Some(slot) {
                high_word = None;
                continue;
            }
            // The machine's own locals are not spilled: they hold the state that survives the
            // suspension rather than anything the body needs restored.
            if machine.is_some_and(|machine| machine.owns(slot)) {
                continue;
            }
            // A frame describes the host's locals; the ones this emitter allocated inside the
            // spliced body are assigned between frames and are known only to it.
            let from_frame = typed
                .get(slot as usize)
                .and_then(verif_spill_type)
                .or_else(|| local_table_type(code, slot, at))
                .or_else(|| stored_reference_type(&insns, index, slot, cw));
            let Some(ty) = from_frame.or_else(|| allocated.get(&slot).copied()) else {
                crate::trace_compiler!(
                    "suspend",
                    "discover: slot {slot} live at {at} has no type in a frame of {} slots (stores {:?})",
                    typed.len(),
                    stores_into(&insns, index, slot)
                );
                return None;
            };
            if matches!(spill_kind(ty), 'J' | 'D') {
                high_word = Some(slot + 1);
            }
            spills.push((slot, ty));
        }
        spills.sort_by_key(|&(slot, _)| slot);
        *suspensions.get_mut(ordinal as usize)? = SuspensionPlan { spills };
    }
    Some(MachinePlan {
        suspensions,
        body_locals: code.max_locals,
    })
}

/// The type of a REFERENCE local the spliced body stores between frames, read off the instruction
/// that produced the value.
///
/// A dependency compiled without a local-variable table names such a slot nowhere: no frame types
/// it, no declaration in this file owns it, and the debug table is empty. What produced the value
/// still says what it is — a call's return type, a `checkcast`, a field read.
fn stored_reference_type(
    insns: &[crate::jvm::inline::Insn],
    index: usize,
    slot: u16,
    cw: &crate::jvm::classfile::ClassWriter,
) -> Option<Ty> {
    use crate::jvm::inline::Insn;
    let store = insns.iter().take(index).rposition(|insn| {
        matches!(insn, Insn::Plain { op, operands }
            if matches!((*op, operands.first()), (0x3a, Some(&n)) if u16::from(n) == slot)
                || (0x4b..=0x4e).contains(op) && u16::from(op - 0x4b) == slot)
    })?;
    let Insn::Plain { op, operands } = insns.get(store.checked_sub(1)?)? else {
        return None;
    };
    let index_of = |operands: &[u8]| -> Option<u16> {
        Some(u16::from_be_bytes([*operands.first()?, *operands.get(1)?]))
    };
    match op {
        // A call's declared return type.
        0xb6 | 0xb7 | 0xb8 | 0xb9 => {
            let (_, _, descriptor) = cw.methodref_parts(index_of(operands)?)?;
            let returns = descriptor.rsplit(')').next()?;
            descriptor_reference_ty(returns)
        }
        // A narrowing the body performed itself, or an instance it just built.
        0xc0 | 0xbb | 0xbd => {
            let class = cw.class_name_at(index_of(operands)?)?;
            Some(Ty::obj(class))
        }
        _ => None,
    }
}

/// A field descriptor's `Ty`, for REFERENCES only: a primitive there would mean the store was not
/// an `astore` and something is being misread.
fn descriptor_reference_ty(descriptor: &str) -> Option<Ty> {
    matches!(descriptor.as_bytes().first(), Some(b'L') | Some(b'['))
        .then(|| crate::jvm::ir_emit::ty_from_field_descriptor(descriptor))
}

/// The store opcodes that write `slot` before instruction `index`. Diagnostic: it says what kind of
/// value an untypeable slot holds.
fn stores_into(insns: &[crate::jvm::inline::Insn], index: usize, slot: u16) -> Vec<u8> {
    use crate::jvm::inline::Insn;
    insns
        .iter()
        .take(index)
        .filter_map(|insn| match insn {
            Insn::Plain { op, operands } => {
                let wrote = match (*op, operands.first()) {
                    (0x36..=0x3a, Some(&n)) => Some(u16::from(n)),
                    (0x3b..=0x4e, _) => Some(u16::from((op - 0x3b) % 4)),
                    _ => None,
                };
                (wrote == Some(slot)).then_some(*op)
            }
            _ => None,
        })
        .collect()
}

/// The type a spliced body's own local is declared with, from the debug table.
///
/// A local assigned between frames — `astore` into a slot the dependency's body owns — is typed by
/// neither a frame nor this emitter's slot map. The splice copies the dependency's
/// LocalVariableTable into the host, and that names the descriptor.
fn local_table_type(code: &crate::jvm::classfile::CodeBuilder, slot: u16, at: usize) -> Option<Ty> {
    let at = u16::try_from(at).ok()?;
    code.local_entries()
        .iter()
        .find(|(start, length, entry_slot, _, _)| {
            *entry_slot == slot
                && *start <= at
                && length.is_none_or(|length| at < start.saturating_add(length))
        })
        .map(|(_, _, _, _, descriptor)| crate::jvm::ir_emit::ty_from_field_descriptor(descriptor))
}

/// The slot-indexed locals of the last frame at or before `offset`.
fn nearest_frame_slots(
    frames: &[(usize, Vec<VerifType>, Vec<VerifType>)],
    offset: usize,
) -> Option<Vec<VerifType>> {
    let (_, locals, _) = frames
        .iter()
        .filter(|(at, _, _)| *at <= offset)
        .next_back()?;
    Some(expand_slots(locals))
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
pub(super) fn continuation_internal(owner: &str, function: &str) -> String {
    format!("{owner}${function}$1")
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

/// Which field each spill of one suspension is stored in.
pub(super) fn suspension_fields(
    suspension: &SuspensionPlan,
) -> Vec<(u16, Ty, String, &'static str)> {
    let mut next: Vec<(char, usize)> = Vec::new();
    let mut placed = Vec::new();
    for &(slot, ty) in &suspension.spills {
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
) -> Option<(String, String, crate::jvm::classfile::CodeBuilder)> {
    use crate::jvm::classfile::CodeBuilder;
    let (params, ret) = split_descriptor(descriptor)?;
    // The receiver plus every parameter: a bridge declares exactly the slots it is handed.
    let locals = 1 + params
        .iter()
        .map(|p| usize::from(matches!(p.as_bytes().first(), Some(b'J') | Some(b'D'))) + 1)
        .sum::<usize>();
    let mut code = CodeBuilder::new(locals as u16);
    code.aload(0);
    let mut slot = 1u16;
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
    code.invokespecial(target, i32::from(slot), returns);
    match ret.as_str() {
        "V" => code.ret_void(),
        "J" => code.lreturn(),
        "D" => code.dreturn(),
        "F" => code.freturn(),
        "I" | "Z" | "B" | "C" | "S" => code.ireturn(),
        _ => code.areturn(),
    }
    let bridge_descriptor = format!("(L{owner};{}", &descriptor[1..]);
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
pub(super) fn build_continuation_class(
    internal: &str,
    outer: &str,
    outer_method: &str,
    outer_descriptor: &str,
    plan: &MachinePlan,
    major: Option<u16>,
    source_file: Option<&str>,
    receiver: Option<&str>,
    bridge: Option<&str>,
) -> Vec<u8> {
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
        (_, Some(_)) => {
            let reference = cw.methodref(outer, outer_method, outer_descriptor);
            invoke.invokevirtual(reference, words, 1);
        }
        (_, None) => {
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
