//! JVM emission of a constructor's DEFAULT-argument realizations.
//!
//! A constructor with defaulted parameters gets a synthetic `<init>(…, int mask,
//! DefaultConstructorMarker)` overload that fills each omitted slot and delegates to the real one,
//! and a constructor that is private for a representation reason gets a marker accessor. Both are
//! physical JVM shapes with no Kotlin declaration of their own, so they live here rather than in
//! the emitter facade.

use super::{
    default_mask_bit, default_mask_count, load, method_descriptor, slot_words, store, ClassWriter,
    CodeBuilder, EmitEnv, Emitter, VerifType,
};
use crate::ir::IrFile;
use crate::types::Ty;

/// Emit the synthetic `<init>(params…, int mask, DefaultConstructorMarker)` overload for a class whose
/// primary constructor has defaulted parameters. Unlike a `$default` method this is a CONSTRUCTOR: `this`
/// is slot 0, the real parameters follow, then the mask + marker; after overwriting each masked slot with
/// its default it `invokespecial`s the real `<init>`. Access is `PUBLIC | SYNTHETIC` (0x1001), matching
/// kotlinc. The defaults were lowered in the instance frame (`this` = value 0, params = 1..=n).
#[allow(clippy::too_many_arguments)]
pub(super) fn emit_ctor_default_stub(
    ir: &IrFile,
    owner: &str,
    facade: &str,
    real_params: &[Ty],
    defaults: &[Option<u32>],
    // Whether the constructor this stub fills defaults for is `@Deprecated`. kotlinc repeats the
    // classic `Deprecated` attribute (not the annotation) on the synthetic overload, so a Java
    // caller reaching the defaulted form is warned too.
    deprecated: bool,
    cw: &mut ClassWriter,
    env: &EmitEnv,
) {
    emit_ctor_default_stub_with_prefix(
        ir,
        owner,
        facade,
        &[],
        0,
        real_params,
        defaults,
        None,
        deprecated,
        0x1001,
        cw,
        env,
    );
}

/// Enum constructors have compiler-supplied `(name, ordinal)` parameters before their source value
/// parameters. They participate in the physical descriptor and delegation but not in Kotlin's
/// default-mask ordinals or in the checked default expression's value numbering.
#[allow(clippy::too_many_arguments)]
pub(super) fn emit_ctor_default_stub_with_prefix(
    ir: &IrFile,
    owner: &str,
    facade: &str,
    physical_prefix: &[Ty],
    logical_prefix_count: usize,
    real_params: &[Ty],
    defaults: &[Option<u32>],
    // Source provenance for the stub's `LineNumberTable`. `None` is the PRIMARY constructor's: the
    // class declaration line at entry and around the delegation, each masked fill at its property's
    // declaration line, the return at the primary's closing paren. A SECONDARY constructor is a
    // different declaration and must not borrow those — `Some` carries its own entry line and, per
    // parameter, the line of the default expression being filled.
    secondary_lines: Option<(u32, &[Option<u32>], u32)>,
    deprecated: bool,
    access: u16,
    cw: &mut ClassWriter,
    env: &EmitEnv,
) {
    debug_assert!(logical_prefix_count <= physical_prefix.len());
    let n = real_params.len();
    // The FILE facade, not the class. A default initializer is ordinary file-level code that happens
    // to run inside the constructor; same-file top-level calls still belong to the facade.
    let mut e = Emitter::new(
        ir,
        cw,
        env,
        owner,
        facade,
        Ty::Unit,
        defaults.iter().flatten().copied(),
    );
    e.this_uninitialized = true;
    let marker = Ty::obj("kotlin/jvm/internal/DefaultConstructorMarker");
    // `this` at slot 0 = value-index 0; real params at value-index 1..=n.
    e.slots.insert(0, (0, Ty::obj(owner)));
    let mut slot = 1u16;
    let mut prefix_slots = Vec::with_capacity(physical_prefix.len());
    for (index, &ty) in physical_prefix.iter().enumerate() {
        prefix_slots.push((slot, ty));
        if index < logical_prefix_count {
            e.slots.insert(index as u32 + 1, (slot, ty));
        }
        slot += slot_words(ty);
    }
    let mut param_slots: Vec<(u16, Ty)> = Vec::new();
    for (i, t) in real_params.iter().enumerate() {
        e.slots
            .insert((logical_prefix_count + i + 1) as u32, (slot, *t));
        param_slots.push((slot, *t));
        slot += slot_words(*t);
    }
    let mask_slots: Vec<u16> = (0..default_mask_count(real_params.len()))
        .map(|_| {
            let s = slot;
            // Backend temporaries — see the member stub: typed in frames, named by no value.
            // Held for the whole stub: nothing releases a mask word before the method ends.
            let _ = e.lease_temporary(s, Ty::Int);
            slot += 1;
            s
        })
        .collect();
    let _ = e.lease_temporary(slot, marker);
    slot += 1;
    e.next_slot = slot;

    // The stackmap frame at each mask-branch target: `this` (slot 0) is UNINITIALIZED (the real `<init>`
    // has not run yet), the params keep their types, then the mask ints + marker. Built manually because
    // the frame machinery types slot 0 from `e.slots` as an initialized `Object`, which the verifier rejects.
    let branch_locals: Vec<VerifType> = {
        let mut raw = vec![VerifType::Top; e.next_slot as usize];
        raw[0] = VerifType::UninitializedThis;
        for &(prefix_slot, prefix_ty) in &prefix_slots {
            raw[prefix_slot as usize] = e.verif_single(prefix_ty);
        }
        for &(pslot, pty) in &param_slots {
            raw[pslot as usize] = e.verif_single(pty);
        }
        for &mask_slot in &mask_slots {
            raw[mask_slot as usize] = VerifType::Integer;
        }
        raw[slot as usize - 1] = e.verif_single(marker);
        // Collapse the two-slot categories (long/double occupy one verif entry) and trim trailing Top.
        let mut out = Vec::new();
        let mut i = 0;
        while i < raw.len() {
            let wide = matches!(raw[i], VerifType::Long | VerifType::Double);
            out.push(raw[i].clone());
            i += if wide { 2 } else { 1 };
        }
        while out.last() == Some(&VerifType::Top) {
            out.pop();
        }
        out
    };
    // kotlinc's `$default` ctor LineNumberTable: the CLASS declaration line at entry, each masked
    // fill's value at its PARAMETER's declaration line, the delegation back at the class line, and
    // the `return` at the primary ctor's closing-`)` line — consecutive same-line entries collapse.
    let class_decl = ir
        .classes
        .iter()
        .find(|candidate| candidate.fq_name() == owner);
    let class_line = secondary_lines
        .map(|(entry, _, _)| entry)
        .unwrap_or_else(|| class_decl.map_or(0, |candidate| candidate.decl_line));
    let mut lines: Vec<(u16, u32)> = vec![(0, class_line)];
    let mut code = CodeBuilder::new(slot);
    for (i, def) in defaults.iter().enumerate().take(n) {
        if let Some(def_expr) = def {
            let (pslot, pty) = param_slots[i];
            code.iload(mask_slots[i / 32]);
            code.push_int(default_mask_bit(i), e.cw);
            code.iand();
            let skip = code.new_label();
            code.add_frame_if_new(skip, branch_locals.clone(), vec![]);
            code.ifeq(skip);
            let parameter_line = match secondary_lines {
                Some((_, parameter_lines, _)) => parameter_lines.get(i).copied().flatten(),
                None => class_decl
                    .and_then(|candidate| {
                        candidate.fields.get(i).and_then(|field| {
                            ir.prop_decl_lines
                                .get(&(candidate.fq_name_id(), field.name.clone()))
                        })
                    })
                    .copied(),
            };
            if let Some(param_line) = parameter_line {
                lines.push((code.bytes.len() as u16, param_line));
            }
            e.emit_value(*def_expr, &mut code);
            store(pty, pslot, &mut code);
            code.bind(skip);
            // The mask/branch machinery for the next slot maps back to the class declaration. The
            // final fill lands at the delegation pc, where the same line is added below and collapsed.
            lines.push((code.bytes.len() as u16, class_line));
        }
    }
    // `invokespecial <owner>.<init>(realparams)V` — delegate to the real primary constructor.
    lines.push((code.bytes.len() as u16, class_line));
    code.aload(0);
    for &(prefix_slot, prefix_ty) in &prefix_slots {
        load(prefix_ty, prefix_slot, &mut code);
    }
    for &(pslot, pty) in &param_slots {
        load(pty, pslot, &mut code);
    }
    let physical_params = physical_prefix
        .iter()
        .chain(real_params)
        .copied()
        .collect::<Vec<_>>();
    let init_desc = method_descriptor(&physical_params, Ty::Unit);
    let aw: i32 = 1 + physical_params
        .iter()
        .map(|t| slot_words(*t) as i32)
        .sum::<i32>();
    let m = e.cw.methodref(owner, "<init>", &init_desc);
    code.invokespecial(m, aw, 0);
    // The `return` maps to the declaration's CLOSING line: the primary constructor's `)` for a
    // primary's stub, and the secondary's own last line for a secondary's — kotlinc's rule, and the
    // two are different declarations' lines, so neither may stand in for the other.
    let close_line = match secondary_lines {
        Some((_, _, close)) => (close != 0).then_some(close),
        None => class_decl
            .and_then(|candidate| ir.ctor_close_lines.get(&candidate.fq_name_id()))
            .copied(),
    };
    if let Some(close) = close_line {
        lines.push((code.bytes.len() as u16, close));
    }
    code.ret_void();
    code.ensure_locals(e.next_slot);
    code.link();

    let mut stub_params = physical_params;
    stub_params.extend(std::iter::repeat_n(
        Ty::Int,
        default_mask_count(real_params.len()),
    ));
    stub_params.push(marker);
    let desc = method_descriptor(&stub_params, Ty::Unit);
    e.cw.add_method(access, "<init>", &desc, &code);
    if class_line != 0 {
        lines.dedup_by_key(|(_, line)| *line);
        e.cw.set_method_lines("<init>", &desc, &lines);
    }
    if deprecated {
        e.cw.mark_method_deprecated("<init>", &desc);
    }
}

/// Emit the PUBLIC|SYNTHETIC accessor `<init>(…args, DefaultConstructorMarker)` for a class whose primary
/// constructor is private (its parameters mention a value class). It delegates straight to the private
/// `<init>` — `this` at slot 0, the real params, then the marker (unused); `invokespecial` the primary,
/// return. Straight-line (no branches ⇒ no StackMapTable). Distinct from the default-arg overload, which
/// carries the extra `int mask` and fills defaults.
pub(super) fn emit_ctor_marker_accessor(owner: &str, real_params: &[Ty], cw: &mut ClassWriter) {
    let mut slot = 1u16; // slot 0 = `this`
    let mut param_slots: Vec<(u16, Ty)> = Vec::new();
    for t in real_params {
        param_slots.push((slot, *t));
        slot += slot_words(*t);
    }
    let total = slot + 1; // + the marker local
                          // The accessor's OWN descriptor interns before its body's Methodref — kotlinc visits a method's
                          // signature before its code, so the private ctor this delegates to must not claim the earlier slot.
    let mut stub_params = real_params.to_vec();
    stub_params.push(Ty::obj("kotlin/jvm/internal/DefaultConstructorMarker"));
    let desc = method_descriptor(&stub_params, Ty::Unit);
    cw.reserve_descriptor(&desc);
    let mut code = CodeBuilder::new(total);
    code.aload(0);
    for &(pslot, pty) in &param_slots {
        load(pty, pslot, &mut code);
    }
    let init_desc = method_descriptor(real_params, Ty::Unit);
    let aw: i32 = 1 + real_params
        .iter()
        .map(|t| slot_words(*t) as i32)
        .sum::<i32>();
    let m = cw.methodref(owner, "<init>", &init_desc);
    code.invokespecial(m, aw, 0);
    code.ret_void();
    code.ensure_locals(total);
    code.link();

    cw.add_method(0x1001 /* PUBLIC | SYNTHETIC */, "<init>", &desc, &code);
}
