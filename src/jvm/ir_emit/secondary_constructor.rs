//! JVM emission of secondary constructors.

use super::{
    constructor_default_masks, emit_ctor_default_stub_with_prefix, emit_ctor_marker_accessor,
    instance_field_jvm_name, jvm_tys, load, method_descriptor, slot_words, type_descriptor,
    ClassWriter, CodeBuilder, EmitEnv, Emitter,
};
use crate::ir::{IrClass, IrFile, IrSecondaryCtor};
use crate::types::Ty;

pub(super) fn defer_serialization_constructor(
    secondary_ordinal: usize,
    serialization_constructor: Option<u32>,
) -> bool {
    serialization_constructor.and_then(|ordinal| usize::try_from(ordinal).ok())
        == Some(secondary_ordinal)
}

pub(super) struct SecondaryConstructorEmitter<'a, 'env, 'writer> {
    pub(super) ir: &'a IrFile,
    pub(super) class: &'a IrClass,
    pub(super) owner: &'a str,
    pub(super) facade: &'a str,
    pub(super) env: &'a EmitEnv<'env>,
    pub(super) writer: &'writer mut ClassWriter,
}

impl SecondaryConstructorEmitter<'_, '_, '_> {
    pub(super) fn emit(self, secondary_ordinal: usize, sc: &IrSecondaryCtor) {
        let ir = self.ir;
        let c = self.class;
        let fq_name = self.owner;
        let facade = self.facade;
        let env = self.env;
        let cw = self.writer;
        let sc_prefix_tys = jvm_tys(&sc.prefix_params);
        let sc_source_tys = jvm_tys(&sc.params);
        let sc_param_tys = sc_prefix_tys
            .iter()
            .chain(&sc_source_tys)
            .copied()
            .collect::<Vec<_>>();
        // Reserve this constructor's header — its declared annotations included — before its body
        // interns anything, matching the order kotlinc's writer produces.
        cw.reserve_method_pool_with_annotations(
            "<init>",
            &method_descriptor(&sc_param_tys, Ty::Unit),
            None,
            &[],
            &sc.annotations,
        );
        let sc_words: u16 = sc_param_tys.iter().map(|t| slot_words(*t)).sum();
        let mut sctor = CodeBuilder::new(1 + sc_words);
        let sec_max;
        let mut sec_diverges = false;
        {
            let mut e = Emitter::new(
                ir,
                cw,
                env,
                fq_name,
                facade,
                Ty::Unit,
                sc.delegate_prelude
                    .iter()
                    .chain(&sc.delegate_args)
                    .copied()
                    .chain(sc.body),
            );
            e.next_slot = 1 + sc_words;
            e.this_uninitialized = true;
            e.slots.insert(0, (0, Ty::obj(fq_name)));
            let mut s = 1u16;
            for (vi, t) in sc_param_tys.iter().enumerate() {
                e.slots.insert(vi as u32 + 1, (s, *t));
                s += slot_words(*t);
            }
            // The checker selected the exact delegation descriptor; lowering only materialized operands.
            use crate::ir::CtorDelegateTarget;
            let (target_class, mut target_jvm_tys, default_masks): (String, Vec<Ty>, &[i32]) =
                match &sc.delegate {
                    CtorDelegateTarget::This {
                        target_params,
                        default_masks,
                        ..
                    } => (fq_name.to_string(), jvm_tys(target_params), default_masks),
                    CtorDelegateTarget::Super {
                        owner,
                        target_params,
                        default_masks,
                    } => {
                        let owner =
                            crate::jvm::jvm_class_map::to_jvm_internal(&owner.render()).to_string();
                        (owner, jvm_tys(target_params), default_masks)
                    }
                };
            let delegates_to_this = matches!(sc.delegate, CtorDelegateTarget::This { .. });
            if !delegates_to_this {
                for &(parameter, field) in &c.pre_super_param_fields {
                    if parameter >= sc.prefix_params.len() as u32 {
                        continue;
                    }
                    let parameter = parameter as usize;
                    let ty = sc_param_tys[parameter];
                    let slot = 1 + sc_param_tys[..parameter]
                        .iter()
                        .map(|ty| slot_words(*ty))
                        .sum::<u16>();
                    let Some(field) = c.fields.get(field as usize) else {
                        e.run.set_emit_error(
                            "secondary constructor prefix store references a missing field"
                                .to_string(),
                        );
                        continue;
                    };
                    sctor.aload(0);
                    load(ty, slot, &mut sctor);
                    let physical_name = instance_field_jvm_name(ir, c, field);
                    let reference =
                        e.cw.fieldref(fq_name, &physical_name, &type_descriptor(field.ty));
                    sctor.putfield(reference, slot_words(field.ty) as i32);
                }
            }
            for &statement in &sc.delegate_prelude {
                e.emit(statement, &mut sctor);
            }
            let dargs = sc.delegate_args.clone();
            if dargs.iter().any(|&a| e.records_frame(a)) {
                let temps = e.spill_to_temps(&dargs, &mut sctor);
                sctor.aload(0);
                if delegates_to_this {
                    for (index, ty) in sc_prefix_tys.iter().enumerate() {
                        let slot = 1 + sc_prefix_tys[..index]
                            .iter()
                            .map(|ty| slot_words(*ty))
                            .sum::<u16>();
                        load(*ty, slot, &mut sctor);
                    }
                    target_jvm_tys.splice(0..0, sc_prefix_tys.iter().copied());
                }
                for &(slot, t, _) in &temps {
                    load(t, slot, &mut sctor);
                }
                for &(_, _, key) in &temps {
                    e.slots.remove(&key);
                }
            } else {
                sctor.aload(0);
                if delegates_to_this {
                    for (index, ty) in sc_prefix_tys.iter().enumerate() {
                        let slot = 1 + sc_prefix_tys[..index]
                            .iter()
                            .map(|ty| slot_words(*ty))
                            .sum::<u16>();
                        load(*ty, slot, &mut sctor);
                    }
                    target_jvm_tys.splice(0..0, sc_prefix_tys.iter().copied());
                }
                for &a in &dargs {
                    e.emit_value(a, &mut sctor);
                }
            }
            let semantic_default_masks =
                constructor_default_masks(&sc.default_parameters, target_jvm_tys.len());
            let emitted_default_masks = if semantic_default_masks.is_empty() {
                default_masks
            } else {
                semantic_default_masks.as_slice()
            };
            if !emitted_default_masks.is_empty() {
                for &mask in emitted_default_masks {
                    sctor.push_int(mask, e.cw);
                    target_jvm_tys.push(Ty::Int);
                }
                sctor.aconst_null();
                target_jvm_tys.push(Ty::obj("kotlin/jvm/internal/DefaultConstructorMarker"));
            }
            // A cross-class delegation target (`super(…)` to a base) whose primary ctor takes a value-class
            // param has a PRIVATE primary — reach it through the `(…args, DefaultConstructorMarker)`
            // accessor. A same-class `this(…)` to the own private primary stays direct (accessible).
            let target_sealed = target_class != fq_name
                && e.ir
                    .classes
                    .iter()
                    .any(|o| o.fq_name_matches(&target_class) && o.is_sealed);
            if emitted_default_masks.is_empty()
                && ((target_class != fq_name && e.ir.has_value_param_ctor(&target_class))
                    || target_sealed)
            {
                sctor.aconst_null();
                target_jvm_tys.push(Ty::obj("kotlin/jvm/internal/DefaultConstructorMarker"));
            }
            let aw: i32 = target_jvm_tys.iter().map(|t| slot_words(*t) as i32).sum();
            let external_descriptor =
                e.ir.external_secondary_super_constructors
                    .get(&(
                        c.fq_name_id(),
                        u32::try_from(secondary_ordinal)
                            .expect("too many secondary constructors for packed identity"),
                    ))
                    .and_then(|target| target.descriptor.as_deref());
            let delegate_descriptor = external_descriptor
                .map(str::to_owned)
                .unwrap_or_else(|| method_descriptor(&target_jvm_tys, Ty::Unit));
            let delegate_init =
                e.cw.methodref(&target_class, "<init>", &delegate_descriptor);
            sctor.invokespecial(delegate_init, aw, 0);
            e.this_uninitialized = false;
            if !delegates_to_this {
                for (parameter, _argument) in c
                    .ctor_args
                    .iter()
                    .take(sc.prefix_params.len())
                    .enumerate()
                    .filter(|(parameter, argument)| {
                        argument.is_field
                            && !c
                                .pre_super_param_fields
                                .iter()
                                .any(|(pre, _)| *pre as usize == *parameter)
                    })
                {
                    let Some(field) = c.fields.get(parameter) else {
                        e.run.set_emit_error(
                            "secondary constructor prefix parameter has no storage field"
                                .to_string(),
                        );
                        continue;
                    };
                    let ty = sc_param_tys[parameter];
                    let slot = 1 + sc_param_tys[..parameter]
                        .iter()
                        .map(|ty| slot_words(*ty))
                        .sum::<u16>();
                    sctor.aload(0);
                    load(ty, slot, &mut sctor);
                    let physical_name = instance_field_jvm_name(ir, c, field);
                    let reference =
                        e.cw.fieldref(fq_name, &physical_name, &type_descriptor(field.ty));
                    sctor.putfield(reference, slot_words(field.ty) as i32);
                }
            }
            if let Some(body) = sc.body {
                e.emit(body, &mut sctor);
                sec_diverges = e.diverges(body);
            }
            sec_max = e.next_slot;
        }
        if !sec_diverges {
            sctor.ret_void();
        }
        sctor.ensure_locals(sec_max);
        sctor.link();
        // A SEALED class's secondary ctor is private too, with its own PUBLIC
        // `(…args, DefaultConstructorMarker)` accessor (kotlinc: EVERY sealed ctor pairs with one).
        // A VALUE-CLASS-parametered secondary ctor gets the same private+marker ABI (kotlinc's).
        let sc_access = (if c.is_sealed || sc.vc_params {
            0x0002
        } else {
            0x0001
        }) | if sc.synthetic { 0x1000 } else { 0 };
        let sc_desc = method_descriptor(&sc_param_tys, Ty::Unit);
        cw.add_method(sc_access, "<init>", &sc_desc, &sctor);
        // Declared constructor annotations, with the same `Deprecated` / `ACC_SYNTHETIC` companions
        // a function's carry (see the method emitter).
        if !sc.annotations.is_empty() {
            cw.set_method_annotations("<init>", &sc_desc, &sc.annotations);
            if sc.annotations.deprecated() {
                cw.mark_method_deprecated("<init>", &sc_desc);
            }
            if sc.annotations.deprecated_hidden() {
                cw.set_method_synthetic("<init>", &sc_desc);
            }
        }
        if sc.defaults.iter().any(Option::is_some) {
            emit_ctor_default_stub_with_prefix(
                ir,
                fq_name,
                facade,
                &sc_prefix_tys,
                sc_prefix_tys.len(),
                &sc_source_tys,
                &sc.defaults,
                sc.annotations.deprecated(),
                0x1001,
                cw,
                env,
            );
        }
        if c.is_sealed || sc.vc_params {
            emit_ctor_marker_accessor(fq_name, &sc_param_tys, cw);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::defer_serialization_constructor;

    #[test]
    fn only_the_producer_recorded_constructor_is_deferred() {
        assert!(!defer_serialization_constructor(0, Some(1)));
        assert!(defer_serialization_constructor(1, Some(1)));
        assert!(!defer_serialization_constructor(2, Some(1)));
        assert!(!defer_serialization_constructor(1, None));
    }
}
