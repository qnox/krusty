//! JVM emission of secondary constructors.

use super::{
    constructor_default_masks, instance_field_jvm_name, jvm_tys, load, method_descriptor,
    slot_words, type_descriptor, ClassWriter, CodeBuilder, EmitEnv, Emitter,
};
use crate::ir::{IrClass, IrFile, IrSecondaryCtor};
use crate::jvm::method_parameters::OwnerConstructorPrefix;
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
    /// Parameters the OWNER's constructors carry ahead of everything the declaration wrote: an
    /// enum's synthetic `(String name, int ordinal)`. They occupy the leading slots and are
    /// forwarded to a `this(…)` delegation, but they are not value parameters, so the body's value
    /// ids still start at the first declared one. ONE description feeds the descriptor, the
    /// `MethodParameters` identities, the generated debug locals, the default stub and the
    /// delegation, so a kind that grows a prefix cannot reach one of them and miss another.
    pub(super) owner_prefix: &'a OwnerConstructorPrefix,
}

impl SecondaryConstructorEmitter<'_, '_, '_> {
    pub(super) fn emit(self, secondary_ordinal: usize, sc: &IrSecondaryCtor) {
        let ir = self.ir;
        let c = self.class;
        let fq_name = self.owner;
        let facade = self.facade;
        let env = self.env;
        let cw = self.writer;
        let owner_prefix_tys = self.owner_prefix.types.clone();
        // An owner prefix and a capture prefix never coexist: only a LOCAL class captures, and no
        // class kind that prepends its own parameters can be declared local. The default stub below
        // relies on it — it binds the FIRST `logical_prefix_count` physical entries to value ids.
        debug_assert!(
            owner_prefix_tys.is_empty() || sc.prefix_params.is_empty(),
            "a constructor prefix is an owner's synthetic one or a capture list, never both"
        );
        let sc_prefix_tys = jvm_tys(&sc.prefix_params);
        let sc_source_tys = jvm_tys(&sc.params);
        let sc_param_tys = owner_prefix_tys
            .iter()
            .chain(&sc_prefix_tys)
            .chain(&sc_source_tys)
            .copied()
            .collect::<Vec<_>>();
        // Everything ahead of the declared parameters: forwarded verbatim to a `this(…)`
        // delegation and spliced into the target's descriptor.
        let forwarded_prefix_tys = owner_prefix_tys
            .iter()
            .chain(&sc_prefix_tys)
            .copied()
            .collect::<Vec<_>>();
        let owner_prefix_words: u16 = owner_prefix_tys.iter().map(|ty| slot_words(*ty)).sum();
        let method_parameters = if env.java_parameters {
            crate::jvm::method_parameters::secondary_constructor(
                c,
                sc,
                self.owner_prefix,
                &sc_param_tys,
            )
        } else {
            Vec::new()
        };
        // Reserve this constructor's header — its declared annotations included — before its body
        // interns anything, matching the order kotlinc's writer produces.
        cw.reserve_method_pool_with_annotations(
            "<init>",
            &method_descriptor(&sc_param_tys, Ty::Unit),
            None,
            &[],
            &sc.annotations,
            &method_parameters,
        );
        let sc_words: u16 = sc_param_tys.iter().map(|t| slot_words(*t)).sum();
        let mut sctor = CodeBuilder::new(1 + sc_words);
        let sec_max;
        let mut sec_diverges = false;
        let delegation_pc;
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
            // The enum name/ordinal are backend-owned physical parameters, not common-IR value
            // identities. Keep them live in every stack-map frame recorded while delegation
            // arguments are evaluated; otherwise a branchy argument turns their slots into `Top`
            // and the later forwarding loads fail verification.
            let mut owner_slot = 1u16;
            for ty in &owner_prefix_tys {
                let _ = e.lease_temporary(owner_slot, *ty);
                owner_slot += slot_words(*ty);
            }
            // Value ids count the DECLARED parameters only, so the owner's prefix is skipped in
            // the numbering while still consuming its slots.
            let mut s = 1u16 + owner_prefix_words;
            for (vi, t) in sc_prefix_tys.iter().chain(&sc_source_tys).enumerate() {
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
                    CtorDelegateTarget::ImplicitEnumBase => {
                        ("java/lang/Enum".to_string(), Vec::new(), &[])
                    }
                };
            let delegates_to_this = matches!(sc.delegate, CtorDelegateTarget::This { .. });
            let forwards_owner_prefix =
                delegates_to_this || matches!(sc.delegate, CtorDelegateTarget::ImplicitEnumBase);
            if !delegates_to_this {
                for &(parameter, field) in &c.pre_super_param_fields {
                    if parameter >= sc.prefix_params.len() as u32 {
                        continue;
                    }
                    let parameter = parameter as usize + owner_prefix_tys.len();
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
            // kotlinc gives a declared secondary constructor one line entry, at the delegation it
            // was written with — after whatever prologue precedes it, which is why the pc is taken
            // here rather than assumed to be 0.
            delegation_pc = Some(sctor.bytes.len() as u16);
            for &statement in &sc.delegate_prelude {
                e.emit(statement, &mut sctor);
            }
            let dargs = sc.delegate_args.clone();
            if dargs.iter().any(|&a| e.records_frame(a)) {
                let temps = e.spill_to_temps(&dargs, &mut sctor);
                sctor.aload(0);
                if forwards_owner_prefix {
                    for (index, ty) in forwarded_prefix_tys.iter().enumerate() {
                        let slot = 1 + forwarded_prefix_tys[..index]
                            .iter()
                            .map(|ty| slot_words(*ty))
                            .sum::<u16>();
                        load(*ty, slot, &mut sctor);
                    }
                    target_jvm_tys.splice(0..0, forwarded_prefix_tys.iter().copied());
                }
                for &(slot, t, _) in &temps {
                    load(t, slot, &mut sctor);
                }
                for &(_, _, lease) in &temps {
                    e.release_temporary(lease);
                }
            } else {
                sctor.aload(0);
                if forwards_owner_prefix {
                    for (index, ty) in forwarded_prefix_tys.iter().enumerate() {
                        let slot = 1 + forwarded_prefix_tys[..index]
                            .iter()
                            .map(|ty| slot_words(*ty))
                            .sum::<u16>();
                        load(*ty, slot, &mut sctor);
                    }
                    target_jvm_tys.splice(0..0, forwarded_prefix_tys.iter().copied());
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
        // An owner that carries a synthetic constructor prefix is an ENUM, whose constructors are
        // implicitly private in Kotlin and private in kotlinc's output. Emitting one public would
        // expose a way to construct an enum instance that the source never granted.
        // Otherwise the declaration's own visibility decides, exactly as it does for a method:
        // a `private constructor` is `ACC_PRIVATE`, a `protected` one `ACC_PROTECTED`, and
        // `internal` is public bytecode with the visibility recorded in metadata.
        let declared_access = match sc.metadata_visibility {
            Some(crate::types::Visibility::Private) => 0x0002,
            Some(crate::types::Visibility::Protected) => 0x0004,
            _ => 0x0001,
        };
        let enum_entry_subclass_target = !owner_prefix_tys.is_empty()
            && c.enum_entries.iter().any(|entry| {
                entry.subclass.is_some()
                    && jvm_tys(&entry.constructor_parameter_types) == sc_source_tys
            });
        let semantically_private = c.is_sealed
            || sc.vc_params
            || !owner_prefix_tys.is_empty()
            || declared_access == 0x0002;
        let sc_access = (if enum_entry_subclass_target {
            // Kotlin uses nestmate access for an entry-body subclass. Krusty does not emit
            // nestmate attributes yet, so use the same package-private synthetic bridge contract
            // as the enum primary constructor instead of emitting an inaccessible private target.
            0x1000
        } else if semantically_private {
            0x0002
        } else {
            declared_access
        }) | if sc.synthetic { 0x1000 } else { 0 };
        let sc_desc = method_descriptor(&sc_param_tys, Ty::Unit);
        // A constructor whose OWNER prepends parameters has a descriptor its declaration did not
        // write, so kotlinc records the source shape in a generic `Signature` — `()V` for an enum's
        // `constructor()`, `(Ljava/lang/String;)V` for `constructor(label: String)`. Without it
        // reflection (and `javap`) reports the ABI prefix as if the source had declared it. The
        // primary already carried one; the secondary path did not.
        // Formatted from the SEMANTIC parameter types, not from descriptors: a `Signature` exists
        // precisely to say what a descriptor cannot, so concatenating descriptors would drop every
        // type argument and type variable — `constructor(values: List<String>)` would sign
        // `(Ljava/util/List;)V` where kotlinc signs `(Ljava/util/List<Ljava/lang/String;>;)V`.
        // A COMPILER-GENERATED constructor records none, for the same reason a compiler-invented
        // accessor does not: the attribute exists for a source or Java caller, and nothing in
        // source can name this constructor to call it. kotlinc declares the serialization
        // plugin's deserialization constructor with its erased descriptor alone.
        let generated =
            ir.is_generated_secondary_constructor(c.fq_name_id(), secondary_ordinal as u32);
        let formatter = super::JvmSignatureFormatter::new(ir, env);
        let sc_signature = (|| -> Option<String> {
            if generated {
                return None;
            }
            let mut signature = String::from("(");
            for (_, semantic) in &sc.named_params {
                signature.push_str(&formatter.method_ty(semantic, super::Wildcards::Declared)?);
            }
            signature.push_str(")V");
            // A `Signature` exists to say what the DESCRIPTOR cannot. One that spells the descriptor
            // back carries nothing and kotlinc omits it — which is every constructor whose source
            // types are already erased and whose owner prepends nothing.
            (signature != sc_desc).then_some(signature)
        })();
        // The debug locals are built BEFORE the method is added so their names and descriptors can
        // be interned first: `add_method` computes the `StackMapTable`, which interns each
        // parameter's verification type, and kotlinc's writer visits the locals before the frames.
        let debug_locals = sc.generated_debug.records_locals().then(|| {
            assert_eq!(
                owner_prefix_tys.len() + sc.named_params.len(),
                sc_param_tys.len(),
                "generated constructor debug identities exactly match physical arity"
            );
            // The owner's synthetic prefix occupies the leading slots and names no local —
            // kotlinc's `LocalVariableTable` for an enum constructor has no entry for the name or
            // the ordinal — so the declared identities start after its words.
            let mut locals = vec![("this".to_string(), format!("L{fq_name};"), 0u16)];
            let mut slot = 1u16 + owner_prefix_words;
            for ((name, _), physical) in sc
                .named_params
                .iter()
                .zip(&sc_param_tys[owner_prefix_tys.len()..])
            {
                locals.push((name.clone(), type_descriptor(*physical), slot));
                slot += slot_words(*physical);
            }
            locals
        });
        if let Some(locals) = &debug_locals {
            cw.reserve_method_lvt(locals);
        }
        let body_line_marks = sctor.line_marks().to_vec();
        cw.add_method_sig(
            sc_access,
            "<init>",
            &sc_desc,
            &sctor,
            sc_signature.as_deref(),
        );
        if let Some((pc, line)) =
            delegation_pc.zip((sc.lines.delegation_line != 0).then_some(sc.lines.delegation_line))
        {
            cw.set_method_lines("<init>", &sc_desc, &[(pc, line)]);
        }
        cw.set_method_parameters("<init>", &sc_desc, &method_parameters);
        if let Some(locals) = &debug_locals {
            cw.set_method_debug(
                "<init>",
                &sc_desc,
                sc.generated_debug.line().map(|line| (0, line)),
                locals,
            );
        }
        // A constructor's line table is CURATED: `add_method` drops the marks a body emitted,
        // because an ordinary `<init>` builds its table from the class declaration and its property
        // initializers instead. A GENERATED constructor has no such curation to fall back on, and
        // its body's marks are exactly the table kotlinc writes — the default value of each
        // defaulted property on that property's own line, the store after it back on the class's.
        // So they are handed back explicitly here rather than left dropped.
        if sc.generated_debug.records_locals() && !body_line_marks.is_empty() {
            // The declaration's own entry opens the table: the constructor's prologue runs before
            // any statement the body marked, so its first mark is not at pc 0.
            let opening = sc
                .generated_debug
                .line()
                .filter(|_| body_line_marks.first().is_none_or(|&(pc, _)| pc != 0))
                .map(|line| (0u16, line));
            // Consecutive entries for the SAME line collapse, exactly as `CodeBuilder::mark_line`
            // collapses them within one body: the opening entry and the body's first mark are both
            // the declaration's line, and kotlinc writes it once.
            let mut entries: Vec<(u16, u32)> = Vec::new();
            for (pc, line) in opening.into_iter().chain(
                body_line_marks
                    .iter()
                    .map(|&(pc, line)| (pc, u32::from(line))),
            ) {
                if entries.last().is_some_and(|&(_, last)| last == line) {
                    continue;
                }
                entries.push((pc, line));
            }
            cw.set_method_lines("<init>", &sc_desc, &entries);
        }
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
            // The stub's physical prefix is everything ahead of the declared parameters — the
            // owner's synthetic entries included, which is what makes an enum's stub
            // `(String, int, …declared, mask, marker)` as kotlinc writes it. Only the capture
            // prefix is LOGICAL: a default initializer can mention a capture, never `$enum$name`.
            // This stub belongs to the SECONDARY constructor, so its line table is the secondary's:
            // each masked fill maps to the default expression it evaluates, and the entry to the
            // first of them — the `constructor(…)` declaration itself. Borrowing the primary's
            // class/field/closing-paren provenance put the whole stub on the class line.
            // Every one of these is the DECLARATION's own recorded line, never a descendant
            // expression's: the entry at the `constructor` keyword, each masked fill at that
            // parameter's default, and the delegation at the declaration's closing line.
            let default_lines = sc
                .lines
                .defaults
                .iter()
                .map(|line| (*line != 0).then_some(*line))
                .collect::<Vec<_>>();
            // An enum's constructors are private, and kotlinc marks the synthetic overload of a
            // private constructor ACC_SYNTHETIC alone: PUBLIC|SYNTHETIC would publish a way to build
            // the class that the declaration does not grant.
            let stub_access = if semantically_private { 0x1000 } else { 0x1001 };
            super::constructor_defaults::emit_ctor_default_stub_with_prefix(
                ir,
                fq_name,
                facade,
                &forwarded_prefix_tys,
                sc_prefix_tys.len(),
                &sc_source_tys,
                &sc.defaults,
                Some((sc.lines.decl_line, &default_lines, sc.lines.decl_end_line)),
                sc.annotations.deprecated(),
                stub_access,
                cw,
                env,
            );
        }
        if c.is_sealed || sc.vc_params {
            super::constructor_defaults::emit_ctor_marker_accessor(fq_name, &sc_param_tys, cw);
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
