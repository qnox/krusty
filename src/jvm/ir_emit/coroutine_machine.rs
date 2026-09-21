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
        let Some(typed) = nearest_frame_slots(&frames, at) else {
            crate::trace_compiler!("suspend", "discover: no frame at or before {at}");
            return None;
        };
        // What the resume has to put back is not only what liveness calls live across the call: a
        // frame at the join claims every HOST local the merge asserts there, whether or not the body
        // reads it again. A slot the frame types but the resume leaves unset fails verification, so
        // the frame's own view is part of the set.
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
            let from_frame = typed.get(slot as usize).and_then(verif_spill_type);
            let Some(ty) = from_frame.or_else(|| allocated.get(&slot).copied()) else {
                crate::trace_compiler!(
                    "suspend",
                    "discover: slot {slot} live at {at} has no type in a frame of {} slots",
                    typed.len()
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

/// The slot-indexed locals of the last frame at or before `offset`.
fn nearest_frame_slots(
    frames: &[(usize, Vec<VerifType>, Vec<VerifType>)],
    offset: usize,
) -> Option<Vec<VerifType>> {
    let (_, locals, _) = frames
        .iter()
        .filter(|(at, _, _)| *at <= offset)
        .next_back()?;
    let mut slots = Vec::with_capacity(locals.len());
    for v in locals {
        slots.push(v.clone());
        if matches!(v, VerifType::Long | VerifType::Double) {
            slots.push(VerifType::Top);
        }
    }
    Some(slots)
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
    let outer_ref = cw.methodref(outer, outer_method, outer_descriptor);
    match receiver {
        Some(_) => invoke.invokevirtual(outer_ref, words, 1),
        None => invoke.invokestatic(outer_ref, words, 1),
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
