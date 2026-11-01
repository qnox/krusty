//! JVM emission of secondary constructors.

use super::frame_map::FrameKey;
use super::{
    capture_order, constructor_default_masks, instance_field_jvm_name, jvm_tys, load,
    method_descriptor, secondary_prefix_jvm_tys, slot_words, type_descriptor, ClassWriter,
    CodeBuilder, EmitEnv, Emitter,
};
use crate::ir::{IrClass, IrConstructorTarget, IrCtorParameterProvenance, IrFile, IrSecondaryCtor};
use crate::jvm::declared_type_nullability::declared_annotation;
use crate::jvm::method_parameters::OwnerConstructorPrefix;
use crate::jvm::private_static_access::StaticOwner;
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
        // A local class takes its captures in its physical order: `prefix_identities[position]` is
        // the capture value at that position. Value ids and the default stub keep identity order.
        let prefix_identities =
            capture_order::physical(ir, c, (0..sc.prefix_params.len()).collect());
        let sc_prefix_tys = secondary_prefix_jvm_tys(ir, c, sc);
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
                ir,
                c,
                sc,
                self.owner_prefix,
                &sc_param_tys,
            )
        } else {
            Vec::new()
        };
        let declared_access = match sc.metadata_visibility {
            Some(crate::types::Visibility::Private) => 0x0002,
            Some(crate::types::Visibility::Protected) => 0x0004,
            _ => 0x0001,
        };
        let semantically_private =
            super::constructor_accessors::hides_secondary(ir, c, secondary_ordinal)
                || sc.vc_params
                || !owner_prefix_tys.is_empty()
                || declared_access == 0x0002;
        let sc_access = (if semantically_private {
            0x0002
        } else {
            declared_access
        }) | if sc.synthetic { 0x1000 } else { 0 }
            | super::method_access::secondary_constructor_varargs(sc);
        let generated =
            ir.is_generated_secondary_constructor(c.fq_name_id(), secondary_ordinal as u32);
        // A declared constructor a source caller can reach annotates each reference parameter with
        // its nullability, as a function does.
        let nullability = if generated || semantically_private || sc.annotations.deprecated_hidden()
        {
            Vec::new()
        } else {
            sc.named_params
                .iter()
                .zip(&sc_source_tys)
                .map(|((_, semantic), physical)| declared_annotation(*semantic, *physical))
                .collect::<Vec<_>>()
        };
        let nullability_types = nullability.iter().flatten().copied().collect::<Vec<_>>();
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
        let formatter = super::JvmSignatureFormatter::new(ir, env);
        let sc_signature = (|| -> Option<String> {
            if generated {
                return None;
            }
            let mut signature = String::from("(");
            for ((_, semantic), carrier) in sc.named_params.iter().zip(&sc.params) {
                // The JVM represents a value-class parameter by its carrier, which is the type the
                // signature names; value-class lowering already selected it.
                let written = if semantic
                    .non_null()
                    .obj_internal()
                    .is_some_and(|name| ir.is_value_class_name(name))
                {
                    carrier
                } else {
                    semantic
                };
                signature.push_str(&formatter.method_ty(written, super::Wildcards::Declared)?);
            }
            signature.push_str(")V");
            // A `Signature` that spells the descriptor back carries nothing and kotlinc omits it —
            // which is every constructor whose source types are already erased and whose owner
            // prepends nothing. A synthetic constructor carries none either. A capture prefix (an
            // inner class's outer instance, a local class's captured values) is left out of the
            // signature without forcing one, so the comparison skips it; an owner prefix (an
            // enum's name and ordinal) still does.
            let unprefixed = owner_prefix_tys
                .iter()
                .chain(&sc_source_tys)
                .copied()
                .collect::<Vec<_>>();
            super::method_signatures::written_signature(
                sc_access,
                false,
                &method_descriptor(&unprefixed, Ty::Unit),
                Some(signature),
            )
        })();
        // Reserve this constructor's header — its declared annotations included — before its body
        // interns anything, matching the order kotlinc's writer produces.
        cw.reserve_method_pool_with_annotations(
            "<init>",
            &sc_desc,
            sc_signature.as_deref(),
            &nullability_types,
            &sc.annotations,
            &method_parameters,
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
                Some(StaticOwner::Class(c.fq_name)),
                fq_name,
                facade,
                Ty::Unit,
                sc.delegate_prelude
                    .iter()
                    .chain(&sc.delegate_args)
                    .copied()
                    .chain(sc.body),
            );
            e.this_uninitialized = true;
            e.regeneration_site = Some(super::bytecode_inline_call::RegenerationSite::constructor(
                &sc_desc,
                env.signature_symbols,
            ));
            // A secondary constructor can own the class-initialization body when no primary
            // constructor exists. Only exact initializer block identities render boundaries.
            e.render_initializer_boundaries = true;
            let receiver = e.frame.enter(FrameKey::Receiver, Ty::obj(fq_name));
            e.slots.insert(0, (receiver, Ty::obj(fq_name)));
            // The enum name/ordinal are backend-owned physical parameters, not common-IR value
            // identities. Keep them live in every stack-map frame recorded while delegation
            // arguments are evaluated; otherwise a branchy argument turns their slots into `Top`
            // and the later forwarding loads fail verification.
            for (index, ty) in owner_prefix_tys.iter().enumerate() {
                let owner_slot = e.frame.enter(FrameKey::Parameter(index as u16), *ty);
                let _ = e.lease_temporary(owner_slot, *ty);
            }
            // Value ids count the DECLARED parameters only, so the owner's prefix is skipped in
            // the numbering while still consuming its slots.
            let values = prefix_identities
                .iter()
                .zip(&sc_prefix_tys)
                .map(|(&identity, &t)| (identity as u32 + 1, t))
                .chain((sc_prefix_tys.len() as u32 + 1..).zip(sc_source_tys.iter().copied()));
            for (value, t) in values {
                let s = e.frame.enter(FrameKey::Value(value), t);
                e.slots.insert(value, (s, t));
            }
            // A generated constructor that keeps debug locals keeps its body's too: kotlinc lists
            // an `init` block's or a copied default's locals ahead of the parameters.
            e.record_locals = !generated || sc.generated_debug.records_locals();
            // kotlinc guards each checked parameter at the very start, ahead of the delegation.
            for (ordinal, check) in sc.param_checks.iter().enumerate() {
                let Some(crate::ir::IrParameterCheck::NonNull) = check else {
                    continue;
                };
                let name = &sc.named_params[ordinal].0;
                let value = (sc_prefix_tys.len() + ordinal) as u32 + 1;
                if let Some(&(slot, _)) = e.slots.get(&value) {
                    e.checked_parameters.insert(value);
                    sctor.aload(slot);
                    sctor.push_string(name, e.cw);
                    let check = e.cw.methodref(
                        "kotlin/jvm/internal/Intrinsics",
                        "checkNotNullParameter",
                        "(Ljava/lang/Object;Ljava/lang/String;)V",
                    );
                    sctor.invokestatic(check, 2, 0);
                }
            }
            // The checker selected the exact delegation descriptor; lowering only materialized operands.
            use crate::ir::CtorDelegateTarget;
            let (target_class, mut target_jvm_tys, target, default_masks): (
                String,
                Vec<Ty>,
                IrConstructorTarget,
                &[i32],
            ) = match &sc.delegate {
                CtorDelegateTarget::This {
                    target_params,
                    target,
                    default_masks,
                } => (
                    fq_name.to_string(),
                    jvm_tys(target_params),
                    *target,
                    default_masks,
                ),
                CtorDelegateTarget::Super {
                    owner,
                    target_params,
                    target,
                    default_masks,
                } => {
                    let params = capture_order::physical_for(ir, *owner, target_params.clone());
                    let owner =
                        crate::jvm::jvm_class_map::to_jvm_internal(&owner.render()).to_string();
                    (owner, jvm_tys(&params), *target, default_masks)
                }
                CtorDelegateTarget::ImplicitEnumBase => (
                    "java/lang/Enum".to_string(),
                    Vec::new(),
                    IrConstructorTarget::UNRESTRICTED_PRIMARY,
                    &[],
                ),
            };
            // A local superclass takes its captures in its own physical order; a `this(…)` call
            // forwards this constructor's prefix, which its arguments do not include.
            let (delegate_args, target_params) = match &sc.delegate {
                CtorDelegateTarget::This { target_params, .. } => {
                    (sc.delegate_args.clone(), target_params.clone())
                }
                CtorDelegateTarget::Super {
                    owner,
                    target_params,
                    ..
                } => (
                    capture_order::physical_for(ir, *owner, sc.delegate_args.clone()),
                    capture_order::physical_for(ir, *owner, target_params.clone()),
                ),
                CtorDelegateTarget::ImplicitEnumBase => (sc.delegate_args.clone(), Vec::new()),
            };
            let target_owner = match &sc.delegate {
                CtorDelegateTarget::Super { owner, .. } => *owner,
                CtorDelegateTarget::This { .. } | CtorDelegateTarget::ImplicitEnumBase => {
                    c.fq_name_id()
                }
            };
            let delegates_to_this = matches!(sc.delegate, CtorDelegateTarget::This { .. });
            let forwards_owner_prefix =
                delegates_to_this || matches!(sc.delegate, CtorDelegateTarget::ImplicitEnumBase);
            if !delegates_to_this {
                let mut stores = c.pre_super_param_fields.clone();
                stores.sort_by_key(|&(parameter, _)| capture_order::position(ir, c, parameter));
                for (parameter, field_index) in stores {
                    if parameter >= sc.prefix_params.len() as u32 {
                        continue;
                    }
                    // An inner class's enclosing-instance store carries the constructor's own line;
                    // a local class's captured values are stored without one.
                    let encloses = c.ctor_args.get(parameter as usize).is_some_and(|argument| {
                        argument.provenance == IrCtorParameterProvenance::EnclosingInstance
                    });
                    if encloses && !generated && sc.lines.decl_line != 0 {
                        sctor.mark_line(sc.lines.decl_line);
                    }
                    let (slot, ty) = e.slots[&(parameter + 1)];
                    let Some(field) = c.fields.get(field_index as usize) else {
                        e.run.set_emit_error(
                            "secondary constructor prefix store references a missing field"
                                .to_string(),
                        );
                        continue;
                    };
                    sctor.aload(0);
                    load(ty, slot, &mut sctor);
                    let physical_name = instance_field_jvm_name(ir, e.run, c, field_index as usize);
                    let reference =
                        e.cw.fieldref(fq_name, &physical_name, &type_descriptor(field.ty));
                    sctor.putfield(reference, slot_words(field.ty) as i32);
                }
            }
            // A declared constructor's table opens after whatever prologue precedes its delegation,
            // at the delegation's line, or at the `constructor` keyword when it is implicit.
            let delegation_line = sc.lines.delegation_call_line().filter(|_| !generated);
            if let Some(line) = delegation_line {
                sctor.mark_line(line);
            }
            for &statement in &sc.delegate_prelude {
                e.emit(statement, &mut sctor);
            }
            let forwarded = if forwards_owner_prefix {
                forwarded_prefix_tys.as_slice()
            } else {
                &[]
            };
            e.emit_constructor_delegation_arguments(
                &delegate_args,
                &target_params,
                forwarded,
                None,
                &mut sctor,
            );
            target_jvm_tys.splice(0..0, forwarded.iter().copied());
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
            // A delegation target whose primary ctor takes a value-class param has a PRIVATE primary —
            // reach it through the `(…args, DefaultConstructorMarker)` accessor, from a subclass's
            // `super(…)` and from the class's own `this(…)` alike. So are a sealed class's
            // constructors, and a private one that a nested subclass delegates to.
            let targets_hidden_primary =
                target.primary() && e.ir.has_value_param_ctor(&target_class);
            if emitted_default_masks.is_empty()
                && (targets_hidden_primary
                    || super::constructor_accessors::reached_through_accessor(
                        target,
                        Some(c.fq_name_id()),
                        target_owner,
                    ))
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
            // The call itself is back on the delegation's line after an argument on another.
            if let Some(line) = delegation_line {
                sctor.mark_line(line);
            }
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
                    let (slot, ty) = e.slots[&(parameter as u32 + 1)];
                    sctor.aload(0);
                    load(ty, slot, &mut sctor);
                    let physical_name = instance_field_jvm_name(ir, e.run, c, parameter);
                    let reference =
                        e.cw.fieldref(fq_name, &physical_name, &type_descriptor(field.ty));
                    sctor.putfield(reference, slot_words(field.ty) as i32);
                }
            }
            if let Some(body) = sc.body {
                e.emit(body, &mut sctor);
                sec_diverges = e.diverges(body);
            }
            // The constructor returns at the end of its declaration: a block body's closing `}`,
            // else the delegation call's last line.
            if !generated && !sec_diverges && sc.lines.decl_end_line != 0 {
                sctor.mark_line(sc.lines.decl_end_line);
            }
            sec_max = e.frame.max();
            // Body locals precede `this` and the parameters in kotlinc's table, as in a method,
            // and stay in scope through the return.
            let body_locals = std::mem::take(&mut e.open_locals);
            e.record_open_locals(body_locals, |_| None, &mut sctor);
        }
        if !sec_diverges {
            if sc.generated_debug.fallthrough_line().is_some() {
                // The producer maps this return to its own line, so the writer must know where.
                sctor.implicit_ret_void();
            } else {
                sctor.ret_void();
            }
        }
        if !generated {
            sctor.add_local_entry(0, None, 0, "this", &format!("L{fq_name};"));
            let names = crate::jvm::method_parameters::secondary_constructor_identities(
                ir,
                c,
                sc,
                self.owner_prefix,
                &sc_param_tys,
            );
            let mut slot = 1;
            for (name, ty) in names.iter().zip(&sc_param_tys) {
                if let Some(name) = name {
                    sctor.add_local_entry(0, None, slot, name, &super::local_variable_desc(*ty));
                }
                slot += slot_words(*ty);
            }
        }
        sctor.ensure_locals(sec_max);
        sctor.link();
        // A SEALED class hides its secondary ctor behind a PUBLIC `(…args, DefaultConstructorMarker)`
        // accessor that `constructor_accessors` emits after the members. A VALUE-CLASS-parametered
        // secondary ctor gets the same private+marker ABI (kotlinc's), its accessor right here.
        // An owner that carries a synthetic constructor prefix is an ENUM, whose constructors are
        // implicitly private in Kotlin and private in kotlinc's output. Emitting one public would
        // expose a way to construct an enum instance that the source never granted.
        // Otherwise the declaration's own visibility decides, exactly as it does for a method:
        // a `private constructor` is `ACC_PRIVATE`, a `protected` one `ACC_PROTECTED`, and
        // `internal` is public bytecode with the visibility recorded in metadata.
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
        // The body's locals, which a generated constructor's table lists ahead of `this`. One still
        // open at the return stays in scope through it, as in any method's table.
        let body_ranged_locals = if generated && debug_locals.is_some() {
            let code_length = sctor.bytes.len();
            sctor
                .named_local_entries()
                .iter()
                .filter(|&&(start, ..)| usize::from(start) < code_length)
                .map(|&(start, length, slot, ref name, ref desc)| {
                    let length = length.unwrap_or_else(|| {
                        u16::try_from(code_length - usize::from(start))
                            .expect("a method's code fits u16")
                    });
                    (start, length, slot, name.clone(), desc.clone())
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        if let Some(locals) = &debug_locals {
            cw.reserve_ranged_local_names(&body_ranged_locals);
            cw.reserve_method_lvt(locals);
        }
        let body_line_marks = sctor.line_marks().to_vec();
        let restated_pcs = sctor
            .inlined_line_marks()
            .map(|(pc, _)| pc)
            .collect::<Vec<_>>();
        cw.add_method_sig(
            sc_access,
            "<init>",
            &sc_desc,
            &sctor,
            sc_signature.as_deref(),
        );
        if !generated {
            cw.keep_method_debug("<init>", &sc_desc, &sctor);
        }
        if nullability.iter().any(Option::is_some) {
            cw.set_method_nullability("<init>", &sc_desc, None, &nullability);
        }
        cw.set_method_parameters("<init>", &sc_desc, &method_parameters);
        if let Some(locals) = &debug_locals {
            cw.set_method_debug(
                "<init>",
                &sc_desc,
                sc.generated_debug.line().map(|line| (0, line)),
                locals,
            );
            cw.prepend_ranged_locals("<init>", &sc_desc, &body_ranged_locals);
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
            // the declaration's line, and kotlinc writes it once. An inlined line, or the caller's
            // line restated after an inlined body, keeps its entry beside an equal one, as it does in
            // any other body.
            let mut entries: Vec<(u16, u32)> = Vec::new();
            for (pc, line) in opening.into_iter().chain(
                body_line_marks
                    .iter()
                    .map(|&(pc, line)| (pc, u32::from(line))),
            ) {
                if entries.last().is_some_and(|&(_, last)| last == line)
                    && !restated_pcs.contains(&pc)
                {
                    continue;
                }
                entries.push((pc, line));
            }
            cw.set_method_lines("<init>", &sc_desc, &entries);
        } else if let (Some(start), Some(fallthrough)) = (
            sc.generated_debug.line(),
            sc.generated_debug.fallthrough_line(),
        ) {
            // A body-less generated constructor whose producer asked for a closing entry: the
            // delegation on the declaration's line and the implicit return on `fallthrough`.
            if let Some(return_pc) = cw
                .method_implicit_void_return_pc("<init>", &sc_desc)
                .filter(|&pc| pc > 0 && start != fallthrough)
            {
                cw.set_method_lines("<init>", &sc_desc, &[(0, start), (return_pc, fallthrough)]);
            }
        }
        // Declared constructor annotations, with the same `Deprecated` / `ACC_SYNTHETIC` companions
        // a function's carry (see the method emitter).
        if !sc.annotations.retains_none() {
            cw.set_method_annotations("<init>", &sc_desc, &sc.annotations);
            if sc.annotations.deprecated() {
                cw.mark_method_deprecated("<init>", &sc_desc);
            }
            if super::function_annotations::hidden_realization_is_synthetic(&sc.annotations) {
                cw.set_method_synthetic("<init>", &sc_desc);
            }
        }
        if let Some(defaults) = crate::jvm::default_wrappers::secondary_constructor(sc) {
            // The stub's physical prefix is everything ahead of the declared parameters — the
            // owner's synthetic entries included, which is what makes an enum's stub
            // `(String, int, …declared, mask, marker)` as kotlinc writes it. Only the capture
            // prefix is LOGICAL: a default initializer can mention a capture, never `$enum$name`.
            // This stub belongs to the SECONDARY constructor, so its line table is the secondary's:
            // the entry, each mask test and the delegation at the `constructor` keyword, the
            // return at the declaration's closing line, and each masked fill on the lines its
            // default expression marks. Borrowing the primary's class/closing-paren provenance put
            // the whole stub on the class line.
            // An enum's constructors are private, and kotlinc marks the synthetic overload of a
            // private constructor ACC_SYNTHETIC alone: PUBLIC|SYNTHETIC would publish a way to build
            // the class that the declaration does not grant.
            let stub_access = if semantically_private { 0x1000 } else { 0x1001 };
            super::constructor_defaults::emit_ctor_default_stub_with_prefix(
                ir,
                super::constructor_defaults::ConstructorDefaultStub {
                    owner: fq_name,
                    owner_identity: c.fq_name,
                    facade,
                    prefix: &owner_prefix_tys
                        .iter()
                        .copied()
                        .chain(jvm_tys(&sc.prefix_params))
                        .collect::<Vec<_>>(),
                    logical_prefix_count: sc_prefix_tys.len(),
                    real_params: &sc_source_tys,
                    defaults,
                    secondary_lines: Some((sc.lines.decl_line, sc.lines.decl_end_line)),
                    target_uses_marker_accessor: sc.vc_params
                        || super::constructor_accessors::hides_secondary(ir, c, secondary_ordinal),
                    deprecated: sc.annotations.deprecated(),
                    access: stub_access,
                },
                cw,
                env,
            );
        }
        // A class's marker accessors follow its members, the primary's first (see
        // `accessor_emission_order`); an enum's stays beside its constructor.
        if sc.vc_params && !c.is_sealed && c.is_enum {
            let parameter_identities =
                crate::jvm::method_parameters::secondary_constructor_identities(
                    ir,
                    c,
                    sc,
                    self.owner_prefix,
                    &sc_param_tys,
                );
            super::constructor_defaults::emit_ctor_marker_accessor(
                fq_name,
                &sc_param_tys,
                &parameter_identities,
                cw,
            );
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
