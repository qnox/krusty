//! `@JvmOverloads`: the overloads kotlinc's JVM backend adds for a function or constructor with
//! default arguments.
//!
//! Common IR keeps one declaration with defaults. Its only input here is the resolved annotation
//! identity `kotlin/jvm/JvmOverloads` recorded on the declaration. For a declaration with `N`
//! defaulted parameters the backend adds one overload per `i` in `N - 1 downTo 0`, each keeping
//! every parameter without a default and the first `i` with one. An overload has the declaration's
//! name, visibility, result, type parameters and annotations, is `final`, guards its own non-null
//! reference parameters, and forwards to the declaration's default-argument wrapper with a zero
//! placeholder for each omitted parameter, the mask words and a null marker.

use super::method_defaults::{
    default_stub_boxed_parameters, default_stub_params, emit_omitted_default_placeholder,
    emit_primitive_box_if_needed, static_default_stub_params, stub_parameter_type,
};
use super::*;
use crate::method_parameters::OwnerConstructorPrefix;
use krusty::jvm::classfile::{
    ACC_BRIDGE, ACC_FINAL, ACC_STRICT, ACC_SYNCHRONIZED, ACC_SYNTHETIC, ACC_VARARGS,
};

const JVM_OVERLOADS: &str = "kotlin/jvm/JvmOverloads";

/// Whether a declaration with `annotations` asks for overloads.
pub(super) fn declared_on(annotations: &krusty::ir::DeclarationAnnotations) -> bool {
    annotations
        .applications()
        .any(|annotation| annotation.internal.matches(JVM_OVERLOADS))
}

/// The parameters each overload keeps, in the order kotlinc adds the overloads: fewer defaults
/// kept last. `has_default[p]` is whether parameter `p` declares a default value.
pub(super) fn kept_parameters(has_default: &[bool]) -> Vec<Vec<usize>> {
    let defaulted = has_default.iter().filter(|&&declared| declared).count();
    (0..defaulted)
        .rev()
        .map(|kept_defaults| {
            let mut seen = 0;
            (0..has_default.len())
                .filter(|&parameter| {
                    if !has_default[parameter] {
                        return true;
                    }
                    seen += 1;
                    seen <= kept_defaults
                })
                .collect()
        })
        .collect()
}

/// The mask words of a call that omits the parameters not in `kept`. Bits number the logical
/// parameters, which exclude a leading extension receiver.
pub(super) fn omitted_masks(
    parameter_count: usize,
    receiver_prefix: usize,
    kept: &[usize],
) -> Vec<i32> {
    let logical = parameter_count - receiver_prefix;
    let mut masks = vec![0i32; default_mask_count(logical)];
    for parameter in (receiver_prefix..parameter_count).filter(|p| !kept.contains(p)) {
        let index = parameter - receiver_prefix;
        masks[index / 32] |= default_mask_bit(index);
    }
    masks
}

/// The parameters each `@JvmOverloads` overload of class member `fid` keeps; empty when it has none.
pub(super) fn member_overloads(ir: &IrFile, fid: u32) -> Vec<Vec<usize>> {
    if !ir.function_annotations.get(&fid).is_some_and(declared_on) {
        return Vec::new();
    }
    let Some((_, defaults)) = crate::default_wrappers::member_function(ir, fid)
        .filter(|(declaration, _)| *declaration == fid)
    else {
        return Vec::new();
    };
    let parameters = jvm_function_params(ir, fid).len();
    kept_parameters(
        &(0..parameters)
            .map(|parameter| defaults.get(parameter).is_some_and(Option::is_some))
            .collect::<Vec<_>>(),
    )
}

/// The generic `Signature` of the method standing for `fid` with only its parameters `kept`, or
/// `None` when it would spell `descriptor` back.
pub(super) fn kept_signature(
    formatter: &JvmSignatureFormatter<'_>,
    ir: &IrFile,
    override_results: &crate::override_results::OverrideResults,
    fid: u32,
    kept: &[usize],
    descriptor: &str,
) -> Option<String> {
    let mut parts = override_result_emission::declared_method_signature_parts(
        formatter,
        ir,
        override_results,
        fid,
    )?;
    assert_eq!(
        parts.parameters.len(),
        jvm_function_params(ir, fid).len(),
        "a signature names every parameter of the declaration"
    );
    parts.parameters = kept.iter().map(|&p| parts.parameters[p].clone()).collect();
    Some(parts.render()).filter(|signature| signature != descriptor)
}

/// Emit the overloads of declared function `fid`, which is placed as `slot` says in `owner`.
pub(super) fn emit_function_overloads(
    ir: &IrFile,
    fid: u32,
    slot: MethodSlot,
    owner: &str,
    cw: &mut ClassWriter,
    env: &EmitEnv,
) {
    let Some(annotations) = ir
        .function_annotations
        .get(&fid)
        .filter(|annotations| declared_on(annotations))
    else {
        return;
    };
    let f = &ir.functions[fid as usize];
    if !ir.has_param_defaults(fid) {
        return;
    }
    let defaults = if slot.static_owner == StaticOwner::Facade {
        crate::default_wrappers::top_level_function(ir, fid)
    } else {
        crate::default_wrappers::member_function(ir, fid)
            .filter(|(declaration, _)| *declaration == fid)
            .map(|(_, defaults)| defaults)
    };
    let Some(defaults) = defaults else {
        env.run.set_emit_error(format!(
            "@JvmOverloads function {} has no default-argument wrapper to forward to",
            f.name
        ));
        return;
    };
    if f.dispatch_receiver
        .is_some_and(|owner| ir.is_value_class_name(owner))
    {
        env.run.set_emit_error(format!(
            "@JvmOverloads function {} is a value class member, which the JVM realizes as a static",
            f.name
        ));
        return;
    }
    let singleton = slot.instance && jvm_static_members::singleton_static_owner(ir, fid).is_some();
    let instance = slot.instance && !singleton;
    let params = jvm_function_params(ir, fid);
    let has_default = (0..params.len())
        .map(|parameter| defaults.get(parameter).is_some_and(Option::is_some))
        .collect::<Vec<_>>();
    let overloads = FunctionOverloads {
        ir,
        fid,
        owner,
        instance,
        static_stub: !instance,
        file_class: slot.static_owner == StaticOwner::Facade,
        params: &params,
        annotations,
        env,
    };
    for kept in kept_parameters(&has_default) {
        overloads.emit(&kept, cw);
    }
}

/// One declared function and the facts its overloads share.
struct FunctionOverloads<'a> {
    ir: &'a IrFile,
    fid: u32,
    owner: &'a str,
    instance: bool,
    /// Whether the default-argument wrapper is a static without a leading receiver parameter.
    static_stub: bool,
    /// Whether the overloads are members of a file facade.
    file_class: bool,
    params: &'a [Ty],
    annotations: &'a krusty::ir::DeclarationAnnotations,
    env: &'a EmitEnv<'a>,
}

impl FunctionOverloads<'_> {
    fn emit(&self, kept: &[usize], cw: &mut ClassWriter) {
        let Self {
            ir,
            fid,
            owner,
            instance,
            static_stub,
            file_class,
            params,
            annotations,
            env,
        } = *self;
        let f = &ir.functions[fid as usize];
        let name = &crate::value_classes::member_names::overload_name(ir, fid, kept, file_class);
        let kept_tys = kept.iter().map(|&p| params[p]).collect::<Vec<_>>();
        let ret = jvm_declared_ty(&env.override_results.physical_result(ir, fid));
        let descriptor = method_descriptor(&kept_tys, ret);
        let mut access = method_access::declared_method_access(
            ir,
            fid,
            owner,
            instance,
            false,
            env.lambda_modes,
        );
        // kotlinc derives no `synchronized` or `strictfp` flag for an overload from the annotations
        // it copies.
        access = access
            & !(ACC_SYNTHETIC | ACC_BRIDGE | ACC_VARARGS | ACC_SYNCHRONIZED | ACC_STRICT)
            | ACC_FINAL;
        if method_access::varargs_access(ir, fid) != 0
            && kept.last() == params.len().checked_sub(1).as_ref()
        {
            access |= ACC_VARARGS;
        }
        let formatter = JvmSignatureFormatter::new(ir, env);
        let signature =
            kept_signature(&formatter, ir, env.override_results, fid, kept, &descriptor);
        let signature = method_signatures::written_signature(access, false, &descriptor, signature);
        let declared_nullability::DeclaredNullability {
            result: result_nullability,
            parameters: declared_parameter_nullability,
        } = override_result_emission::declared_nullability(ir, env, fid);
        let parameter_nullability = kept
            .iter()
            .map(|&p| declared_parameter_nullability.get(p).copied().flatten())
            .collect::<Vec<_>>();
        let nullability_annotated =
            function_annotations::nullability_annotated(ir, fid, annotations);
        let declared_annotations = &ir.declared_parameters(fid).annotations;
        let parameter_annotations = if declared_annotations.is_empty() {
            Vec::new()
        } else {
            kept.iter()
                .map(|&p| declared_annotations.get(p).cloned().unwrap_or_default())
                .collect()
        };
        let method_parameters = if env.java_parameters {
            let all = crate::method_parameters::function(ir, fid, params, None);
            kept.iter().filter_map(|&p| all.get(p).cloned()).collect()
        } else {
            Vec::new()
        };
        // kotlinc visits the header before the code, so its constants precede the body's.
        cw.reserve_method_pool_with_annotations(
            name,
            &descriptor,
            signature.as_deref(),
            &[],
            annotations,
            &method_parameters,
        );
        cw.reserve_method_parameter_annotation_pool(
            result_nullability,
            &parameter_annotations,
            &parameter_nullability,
            nullability_annotated,
        );

        let mut slots = Vec::with_capacity(kept.len());
        let mut next = u16::from(instance);
        for ty in &kept_tys {
            slots.push(next);
            next += slot_words(*ty);
        }
        let mut code = CodeBuilder::new(next);
        self.emit_parameter_checks(kept, &slots, cw, &mut code);
        let receiver_prefix = usize::from(ir.extension_receiver_fns.contains(&fid));
        let boxed = default_stub_boxed_parameters(ir, fid);
        let stub_tys = params
            .iter()
            .enumerate()
            .map(|(index, ty)| stub_parameter_type(ir, fid, index, *ty))
            .collect::<Vec<_>>();
        if instance {
            code.aload(0);
        }
        for (index, ty) in params.iter().enumerate() {
            match kept.iter().position(|&p| p == index) {
                Some(position) => {
                    load(*ty, slots[position], &mut code);
                    if let Some(value_class) = boxed.get(&index) {
                        emit_box_impl(ir, cw, value_class, &mut code);
                    }
                    emit_primitive_box_if_needed(cw, *ty, stub_tys[index], &mut code);
                }
                None => emit_omitted_default_placeholder(cw, *ty, stub_tys[index], &mut code),
            }
        }
        let masks = omitted_masks(params.len(), receiver_prefix, kept);
        let mask_count = masks.len();
        for mask in masks {
            code.push_int(mask, cw);
        }
        code.aconst_null();
        let stub_parameters = if static_stub {
            static_default_stub_params(ir, fid)
        } else {
            default_stub_params(ir, fid, Ty::obj(owner))
        };
        debug_assert_eq!(
            stub_parameters.len(),
            usize::from(!static_stub) + params.len() + mask_count + 1
        );
        let argument_words = stub_parameters
            .iter()
            .map(|ty| i32::from(slot_words(*ty)))
            .sum();
        let stub = cw.methodref(
            owner,
            &format!("{}$default", f.name),
            &method_descriptor(&stub_parameters, jvm_declared_ty(&f.ret)),
        );
        code.invokestatic(stub, argument_words, physical_call_result_words(ret));
        emit_return(ret, &mut code);
        self.record_locals(kept, &kept_tys, &slots, &mut code);
        code.ensure_locals(next);
        code.link();

        cw.add_method_sig(access, name, &descriptor, &code, signature.as_deref());
        cw.set_method_parameters(name, &descriptor, &method_parameters);
        if nullability_annotated
            && (result_nullability.is_some() || parameter_nullability.iter().any(Option::is_some))
        {
            cw.set_method_nullability(
                name,
                &descriptor,
                result_nullability,
                &parameter_nullability,
            );
        }
        if !parameter_annotations.is_empty() {
            cw.set_method_param_annotations(name, &descriptor, &parameter_annotations);
        }
        function_annotations::emit_declared(cw, annotations, name, &descriptor);
    }

    /// `Intrinsics.checkNotNullParameter` for each kept parameter the declaration guards.
    fn emit_parameter_checks(
        &self,
        kept: &[usize],
        slots: &[u16],
        cw: &mut ClassWriter,
        code: &mut CodeBuilder,
    ) {
        let ir = self.ir;
        let declared = &ir.functions[self.fid as usize];
        let names = crate::parameter_names::placed(
            ir,
            self.fid,
            None,
            crate::parameter_names::function_assertions(ir, self.fid, self.params),
        );
        for (position, &index) in kept.iter().enumerate() {
            if declared.param_checks.get(index).is_none_or(Option::is_none) {
                continue;
            }
            let name = names
                .as_ref()
                .and_then(|names| names.get(index))
                .and_then(Clone::clone)
                .expect("a checked parameter carries an assertion spelling");
            code.aload(slots[position]);
            code.push_string(&name, cw);
            let check = cw.methodref(
                "kotlin/jvm/internal/Intrinsics",
                "checkNotNullParameter",
                "(Ljava/lang/Object;Ljava/lang/String;)V",
            );
            code.invokestatic(check, 2, 0);
        }
    }

    /// The overload's `LocalVariableTable`: the receiver and the kept parameters.
    fn record_locals(
        &self,
        kept: &[usize],
        kept_tys: &[Ty],
        slots: &[u16],
        code: &mut CodeBuilder,
    ) {
        let ir = self.ir;
        if self.instance {
            code.add_local_entry(0, None, 0, "this", &format!("L{};", self.owner));
        }
        let names = crate::parameter_names::placed(
            ir,
            self.fid,
            None,
            crate::parameter_names::function_locals(ir, self.fid, self.params),
        )
        .unwrap_or_default();
        for (position, &index) in kept.iter().enumerate() {
            if let Some(Some(name)) = names.get(index) {
                code.add_local_entry(
                    0,
                    None,
                    slots[position],
                    name,
                    &local_variable_desc(kept_tys[position]),
                );
            }
        }
    }
}

/// A member of a class's source schedule that can carry `@JvmOverloads`.
pub(super) enum OverloadSite {
    Function(u32),
    SecondaryConstructor(usize),
}

/// The overload sites among `members`, in schedule order.
pub(super) fn sites(members: &[member_schedule::SourceOrderedMember<'_>]) -> Vec<OverloadSite> {
    members
        .iter()
        .filter_map(|member| match member {
            member_schedule::SourceOrderedMember::Function(function) => {
                Some(OverloadSite::Function(*function))
            }
            member_schedule::SourceOrderedMember::SecondaryConstructor(ordinal, _) => {
                Some(OverloadSite::SecondaryConstructor(*ordinal))
            }
            member_schedule::SourceOrderedMember::Property(_)
            | member_schedule::SourceOrderedMember::StaticDefaultAccessor(..) => None,
        })
        .collect()
}

/// What the overloads of a class's primary constructor take from the class emission.
pub(super) struct PrimaryOverloads<'a> {
    /// The access word of the primary constructor.
    pub(super) access: u16,
    pub(super) owner: &'a str,
}

/// Emit every `@JvmOverloads` overload of `class`: the primary constructor's first, then those of
/// the other declarations in `sites` order, as kotlinc appends them after the declared members.
pub(super) fn emit_class_overloads(
    ir: &IrFile,
    class: &krusty::ir::IrClass,
    primary: PrimaryOverloads<'_>,
    sites: &[OverloadSite],
    cw: &mut ClassWriter,
    env: &EmitEnv,
) {
    let owner = primary.owner;
    if class.has_primary_ctor && declared_on(&class.primary_ctor_annotations) {
        emit_primary_overloads(ir, class, primary, Phase::Declarations, cw, env);
    }
    for site in sites {
        match *site {
            OverloadSite::Function(function) => {
                let slot = MethodSlot {
                    static_owner: StaticOwner::Class(class.fq_name),
                    instance: !ir.functions[function as usize].is_static,
                };
                emit_function_overloads(ir, function, slot, owner, cw, env);
            }
            OverloadSite::SecondaryConstructor(ordinal) => {
                emit_secondary_overloads(ir, class, ordinal, owner, Phase::Declarations, cw, env);
            }
        }
    }
}

/// Emit the `(…, DefaultConstructorMarker)` accessor of every overload whose declaration the JVM
/// hides behind one: a sealed class's constructors and those taking a value class. kotlinc adds
/// them after the accessors of the declared constructors, the primary's overloads first.
pub(super) fn emit_marker_accessors(
    ir: &IrFile,
    class: &krusty::ir::IrClass,
    owner: &str,
    cw: &mut ClassWriter,
    env: &EmitEnv,
) {
    if class.has_primary_ctor && declared_on(&class.primary_ctor_annotations) {
        let primary = PrimaryOverloads {
            access: method_access::primary_constructor_access(
                ir,
                class,
                false,
                ir.has_value_param_ctor(&class.fq_name()),
            ),
            owner,
        };
        emit_primary_overloads(ir, class, primary, Phase::MarkerAccessors, cw, env);
    }
    for ordinal in 0..class.secondary_ctors.len() {
        emit_secondary_overloads(ir, class, ordinal, owner, Phase::MarkerAccessors, cw, env);
    }
}

/// What a constructor declaration's overloads are emitted as.
#[derive(Clone, Copy)]
enum Phase {
    /// The overload constructors themselves.
    Declarations,
    /// The marker accessors of the overloads a hidden constructor has.
    MarkerAccessors,
}

fn emit_primary_overloads(
    ir: &IrFile,
    class: &krusty::ir::IrClass,
    primary: PrimaryOverloads<'_>,
    phase: Phase,
    cw: &mut ClassWriter,
    env: &EmitEnv,
) {
    let Some(defaults) = crate::default_wrappers::primary_constructor(ir, class) else {
        return;
    };
    let hidden = class.is_sealed || ir.has_value_param_ctor(&class.fq_name());
    let prefix_count = class.constructor_prefix_count as usize;
    let physical = class_ctor_jvm_tys(ir, class);
    let (prefix, source) = physical.split_at(prefix_count);
    let formatter = JvmSignatureFormatter::new(ir, env);
    let source_parameters = primary_ctor_source_parameters(ir, class);
    let identities = crate::parameter_names::constructor_identities(&class.ctor_args);
    let user_annotations = if class.ctor_param_annotations.is_empty() {
        Vec::new()
    } else {
        source_parameters
            .iter()
            .map(|parameter| parameter.annotations.cloned().unwrap_or_default())
            .collect()
    };
    let signature = |kept: &[usize]| {
        constructor_signatures::primary_constructor_signature_keeping(&formatter, ir, class, kept)
    };
    let overloads = ConstructorOverloads {
        owner: primary.owner,
        access: primary.access,
        prefix,
        source,
        has_default: defaults[prefix_count..]
            .iter()
            .map(Option::is_some)
            .collect(),
        checks: (prefix_count..class.ctor_args.len())
            .map(|index| class.ctor_args[index].check.clone())
            .collect(),
        local_names: capture_order::physical(ir, class, (0..class.ctor_args.len()).collect())
            .into_iter()
            .map(|index| crate::parameter_names::local_variable(&identities[index], "<init>"))
            .collect(),
        nullability: source_parameters
            .iter()
            .map(|parameter| nullability_annotation(parameter.nullability))
            .collect(),
        parameter_annotations: user_annotations,
        method_parameters: primary_constructor_method_parameters(ir, class, env.java_parameters),
        annotations: &class.primary_ctor_annotations,
        signature: &signature,
        varargs: false,
    };
    overloads.emit_all(phase, hidden, cw);
}

fn emit_secondary_overloads(
    ir: &IrFile,
    class: &krusty::ir::IrClass,
    ordinal: usize,
    owner: &str,
    phase: Phase,
    cw: &mut ClassWriter,
    env: &EmitEnv,
) {
    let constructor = &class.secondary_ctors[ordinal];
    if !declared_on(&constructor.annotations) {
        return;
    }
    let Some(defaults) = crate::default_wrappers::secondary_constructor(constructor) else {
        return;
    };
    let declaration = secondary_constructor::SecondaryDeclaration::of(
        ir,
        class,
        ordinal,
        constructor,
        class.is_enum,
    );
    let hidden = constructor_accessors::hides_secondary(ir, class, ordinal)
        || (constructor.vc_params && !class.is_enum);
    // An enum's constructors take the constant's name and ordinal first, and so do its overloads.
    let owner_prefix = if class.is_enum {
        OwnerConstructorPrefix::enum_class()
    } else {
        OwnerConstructorPrefix::none()
    };
    let prefix = owner_prefix
        .types
        .iter()
        .copied()
        .chain(secondary_prefix_jvm_tys(ir, class, constructor))
        .collect::<Vec<_>>();
    let source = jvm_tys(&constructor.params);
    let physical = prefix.iter().chain(&source).copied().collect::<Vec<_>>();
    let method_parameters = if env.java_parameters {
        crate::method_parameters::secondary_constructor(
            ir,
            class,
            constructor,
            &owner_prefix,
            &physical,
        )
    } else {
        Vec::new()
    };
    let access = declaration.access;
    let signature = |kept: &[usize]| {
        secondary_constructor::written_signature(
            ir,
            env,
            constructor,
            Some(kept),
            &owner_prefix.types,
            access,
        )
    };
    let overloads = ConstructorOverloads {
        owner,
        access,
        prefix: &prefix,
        source: &source,
        has_default: defaults.iter().map(Option::is_some).collect(),
        checks: constructor
            .param_checks
            .iter()
            .zip(&constructor.named_params)
            .map(|(check, (name, _))| {
                matches!(check, Some(krusty::ir::IrParameterCheck::NonNull)).then(|| name.clone())
            })
            .collect(),
        local_names: crate::method_parameters::secondary_constructor_identities(
            ir,
            class,
            constructor,
            &owner_prefix,
            &physical,
        ),
        nullability: declaration.nullability,
        parameter_annotations: Vec::new(),
        method_parameters,
        annotations: &constructor.annotations,
        signature: &signature,
        varargs: method_access::secondary_constructor_varargs(constructor) != 0,
    };
    overloads.emit_all(phase, hidden, cw);
}

/// EVERY parameter defaulted → kotlinc also emits the no-arg convenience `<init>()`
/// (`AuditFilters()` in Java/reflection), delegating to the `$default` overload with a full
/// mask — AFTER the declared methods (kotlinc's member order), at the primary's declared
/// visibility (a PROTECTED primary gets a protected convenience ctor; one hidden behind its
/// marker accessor for a value-class parameter is still public). A declared constructor without
/// value parameters already is that `<init>()`, so kotlinc's JvmDefaultConstructorLowering adds
/// none beside it.
/// A `@JvmOverloads` primary's last overload already is that `<init>()`.
pub(super) fn emit_default_constructor(
    ir: &IrFile,
    c: &krusty::ir::IrClass,
    fq_name: &str,
    is_continuation: bool,
    cw: &mut ClassWriter,
) {
    let declares_no_arg_ctor = c.secondary_ctors.iter().any(|sc| sc.params.is_empty())
        || declared_on(&c.primary_ctor_annotations);
    if c.has_primary_ctor && !declares_no_arg_ctor {
        let param_tys = class_ctor_jvm_tys(ir, c);
        let ctor_access = if is_continuation || c.is_anonymous_object {
            0x0000
        } else if c.is_singleton() || c.is_value || c.is_sealed {
            0x0002
        } else {
            match ir.ctor_visibilities.get(&c.fq_name_id()) {
                Some(krusty::types::Visibility::Protected) => 0x0004,
                Some(krusty::types::Visibility::Private) => 0x0002,
                _ => 0x0001,
            }
        };
        if let Some(defaults) = ir.class_ctor_defaults(fq_name) {
            if !param_tys.is_empty()
                && defaults.len() == param_tys.len()
                && defaults.iter().all(Option::is_some)
                && !c.is_sealed
                && (ctor_access == 0x0001 || ctor_access == 0x0004)
            {
                // ASM interns a method's name and descriptor at its header visit, before its body.
                cw.reserve_method_name("<init>");
                cw.reserve_descriptor("()V");
                let mut z = CodeBuilder::new(1);
                z.aload(0);
                for &t in &param_tys {
                    push_zero(t, &mut z, cw);
                }
                for mask in full_default_masks(param_tys.len()) {
                    z.push_int(mask, cw);
                }
                z.aconst_null();
                let mut stub_params = param_tys.clone();
                stub_params.extend(std::iter::repeat_n(
                    Ty::Int,
                    default_mask_count(param_tys.len()),
                ));
                stub_params.push(Ty::obj("kotlin/jvm/internal/DefaultConstructorMarker"));
                let aw: i32 = 1 + stub_params
                    .iter()
                    .map(|t| slot_words(*t) as i32)
                    .sum::<i32>();
                let m = cw.methodref(
                    fq_name,
                    "<init>",
                    &method_descriptor(&stub_params, Ty::Unit),
                );
                z.invokespecial(m, aw, 0);
                z.ret_void();
                z.ensure_locals(1);
                z.link();
                cw.add_method(ctor_access, "<init>", "()V", &z);
                // kotlinc gives the convenience ctor a `this` LocalVariableTable (no line table).
                cw.set_method_debug(
                    "<init>",
                    "()V",
                    None,
                    &[("this".to_string(), format!("L{fq_name};"), 0)],
                );
                // The convenience ctor STANDS IN for the declared primary, so kotlinc repeats the
                // primary's declared annotations on it (the `$default` overload between them, being
                // synthetic, gets none). Their descriptors already interned at the primary.
                let annotations = &c.primary_ctor_annotations;
                if !annotations.retains_none() {
                    cw.set_method_annotations("<init>", "()V", annotations);
                    if annotations.deprecated() {
                        cw.mark_method_deprecated("<init>", "()V");
                    }
                    if function_annotations::hidden_realization_is_synthetic(annotations) {
                        cw.set_method_synthetic("<init>", "()V");
                    }
                }
            }
        }
    }
}

/// One constructor declaration with defaults and the facts its overloads share.
struct ConstructorOverloads<'a> {
    owner: &'a str,
    access: u16,
    /// The compiler-supplied leading parameters, in physical order. Every overload forwards them.
    prefix: &'a [Ty],
    /// The declared parameters.
    source: &'a [Ty],
    has_default: Vec<bool>,
    /// Per declared parameter, the name its non-null guard quotes.
    checks: Vec<Option<String>>,
    /// Per physical parameter, prefix first, its `LocalVariableTable` name.
    local_names: Vec<Option<String>>,
    /// Per declared parameter, its nullability annotation; empty when none is published.
    nullability: Vec<Option<&'static str>>,
    /// Per declared parameter, its own annotations; empty when none is declared.
    parameter_annotations: Vec<krusty::ir::DeclarationAnnotations>,
    /// Per physical parameter, prefix first; empty without `-java-parameters`.
    method_parameters: Vec<crate::method_parameters::MethodParameter>,
    annotations: &'a krusty::ir::DeclarationAnnotations,
    /// The generic `Signature` of the overload keeping the declared parameters it is given.
    signature: &'a dyn Fn(&[usize]) -> Option<String>,
    /// Whether the declaration ends in a `vararg` parameter.
    varargs: bool,
}

impl ConstructorOverloads<'_> {
    /// Every overload of the declaration, as `phase` asks. `hidden`: the JVM reaches the
    /// declaration through a marker accessor, so each overload has one too.
    fn emit_all(&self, phase: Phase, hidden: bool, cw: &mut ClassWriter) {
        for kept in kept_parameters(&self.has_default) {
            match phase {
                Phase::Declarations => self.emit(&kept, cw),
                Phase::MarkerAccessors if hidden => self.emit_marker_accessor(&kept, cw),
                Phase::MarkerAccessors => {}
            }
        }
    }

    fn emit_marker_accessor(&self, kept: &[usize], cw: &mut ClassWriter) {
        let physical = self
            .prefix
            .iter()
            .copied()
            .chain(kept.iter().map(|&index| self.source[index]))
            .collect::<Vec<_>>();
        let identities = (0..self.prefix.len())
            .chain(kept.iter().map(|&index| self.prefix.len() + index))
            .map(|position| self.local_names.get(position).cloned().flatten())
            .collect::<Vec<_>>();
        constructor_defaults::emit_ctor_marker_accessor(self.owner, &physical, &identities, cw);
    }

    fn emit(&self, kept: &[usize], cw: &mut ClassWriter) {
        let owner = self.owner;
        let kept_tys = kept.iter().map(|&p| self.source[p]).collect::<Vec<_>>();
        let physical = self
            .prefix
            .iter()
            .chain(&kept_tys)
            .copied()
            .collect::<Vec<_>>();
        let descriptor = method_descriptor(&physical, Ty::Unit);
        let trailing_kept = self.source.len().checked_sub(1).as_ref() == kept.last();
        let access = self.access & !(ACC_SYNTHETIC | ACC_VARARGS)
            | if self.varargs && trailing_kept {
                ACC_VARARGS
            } else {
                0
            };
        let signature = (self.signature)(kept);
        let nullability = if self.nullability.is_empty() {
            Vec::new()
        } else {
            kept.iter().map(|&p| self.nullability[p]).collect()
        };
        let nullability_types = nullability.iter().flatten().copied().collect::<Vec<_>>();
        let parameter_annotations = kept
            .iter()
            .filter_map(|&p| self.parameter_annotations.get(p).cloned())
            .collect::<Vec<_>>();
        let method_parameters = self
            .method_parameters
            .iter()
            .enumerate()
            .filter(|(position, _)| {
                *position < self.prefix.len() || kept.contains(&(position - self.prefix.len()))
            })
            .map(|(_, parameter)| parameter.clone())
            .collect::<Vec<_>>();
        cw.reserve_method_pool_with_annotations(
            "<init>",
            &descriptor,
            signature.as_deref(),
            &nullability_types,
            self.annotations,
            &method_parameters,
        );
        let mut code = CodeBuilder::new(1 + physical.iter().map(|ty| slot_words(*ty)).sum::<u16>());
        let mut next = 1u16;
        let prefix_slots = self
            .prefix
            .iter()
            .map(|ty| {
                let slot = next;
                next += slot_words(*ty);
                slot
            })
            .collect::<Vec<_>>();
        let kept_slots = kept_tys
            .iter()
            .map(|ty| {
                let slot = next;
                next += slot_words(*ty);
                slot
            })
            .collect::<Vec<_>>();
        for (position, &index) in kept.iter().enumerate() {
            if let Some(name) = self.checks.get(index).and_then(Option::as_ref) {
                code.aload(kept_slots[position]);
                code.push_string(name, cw);
                let check = cw.methodref(
                    "kotlin/jvm/internal/Intrinsics",
                    "checkNotNullParameter",
                    "(Ljava/lang/Object;Ljava/lang/String;)V",
                );
                code.invokestatic(check, 2, 0);
            }
        }
        code.aload(0);
        for (ty, slot) in self.prefix.iter().zip(&prefix_slots) {
            load(*ty, *slot, &mut code);
        }
        for (index, ty) in self.source.iter().enumerate() {
            match kept.iter().position(|&p| p == index) {
                Some(position) => load(*ty, kept_slots[position], &mut code),
                None => push_zero(*ty, &mut code, cw),
            }
        }
        let mut masks = vec![0i32; default_mask_count(self.source.len())];
        for index in (0..self.source.len()).filter(|index| !kept.contains(index)) {
            masks[index / 32] |= default_mask_bit(index);
        }
        let mask_count = masks.len();
        for mask in masks {
            code.push_int(mask, cw);
        }
        code.aconst_null();
        let stub_parameters = self
            .prefix
            .iter()
            .chain(self.source)
            .copied()
            .chain(std::iter::repeat_n(Ty::Int, mask_count))
            .chain(std::iter::once(Ty::obj(
                "kotlin/jvm/internal/DefaultConstructorMarker",
            )))
            .collect::<Vec<_>>();
        let argument_words = 1 + stub_parameters
            .iter()
            .map(|ty| i32::from(slot_words(*ty)))
            .sum::<i32>();
        let stub = cw.methodref(
            owner,
            "<init>",
            &method_descriptor(&stub_parameters, Ty::Unit),
        );
        code.invokespecial(stub, argument_words, 0);
        code.ret_void();
        let mut locals = vec![("this".to_string(), format!("L{owner};"), 0u16)];
        let kept_physical = (0..self.prefix.len())
            .map(|position| (position, self.prefix[position], prefix_slots[position]))
            .chain(kept.iter().enumerate().map(|(position, &index)| {
                (
                    self.prefix.len() + index,
                    kept_tys[position],
                    kept_slots[position],
                )
            }));
        for (position, ty, slot) in kept_physical {
            if let Some(Some(name)) = self.local_names.get(position) {
                locals.push((name.clone(), local_variable_desc(ty), slot));
            }
        }
        code.ensure_locals(next);
        code.link();
        cw.add_method_sig(access, "<init>", &descriptor, &code, signature.as_deref());
        cw.set_method_debug("<init>", &descriptor, None, &locals);
        cw.set_method_parameters("<init>", &descriptor, &method_parameters);
        if nullability.iter().any(Option::is_some) {
            cw.set_method_nullability("<init>", &descriptor, None, &nullability);
        }
        if !parameter_annotations.is_empty() {
            cw.set_method_param_annotations("<init>", &descriptor, &parameter_annotations);
        }
        if !self.annotations.retains_none() {
            cw.set_method_annotations("<init>", &descriptor, self.annotations);
            if self.annotations.deprecated() {
                cw.mark_method_deprecated("<init>", &descriptor);
            }
            if function_annotations::hidden_realization_is_synthetic(self.annotations) {
                cw.set_method_synthetic("<init>", &descriptor);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{kept_parameters, omitted_masks};

    #[test]
    fn overloads_drop_trailing_defaults_first() {
        let kept = kept_parameters(&[false, true, true, false, true]);
        assert_eq!(
            kept,
            vec![vec![0, 1, 2, 3], vec![0, 1, 3], vec![0, 3]],
            "one overload per defaulted parameter, keeping the first `i` of them"
        );
        assert!(kept_parameters(&[false, false]).is_empty());
    }

    #[test]
    fn masks_number_logical_parameters_after_the_receiver() {
        // A receiver at 0, defaults at 1 and 2: omitting both sets bits 0 and 1.
        assert_eq!(omitted_masks(3, 1, &[0]), vec![0b11]);
        assert_eq!(omitted_masks(3, 1, &[0, 1]), vec![0b10]);
    }
}
