//! Compatibility forwarders an implementing class writes for the interface default members it
//! inherits and does not override, and the `-impl` static a value class realizes each one with.
//!
//! Override resolution selects the members and the direct superinterface each is inherited
//! through ([`crate::fir::ResolvedInheritedDefault`]); this module maps one record to its JVM
//! name, descriptor, flags and instructions, written at the position its class kind gives it:
//! after the bridges' predecessors for an ordinary class, and beside its static before the private
//! primary constructor for a value class.

use super::*;

/// How an implementing class's compatibility forwarder reaches the inherited interface body.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ForwarderDispatch {
    /// `invokestatic` on a `$DefaultImpls` holder (the `disable` realization, own module or
    /// dependency).
    HolderStatic,
    /// `invokespecial` on a direct superinterface's default method (the `enable` realization).
    InterfaceSpecial,
}

/// One inherited interface member the class realizes with a forwarder, with the parameter
/// identities its declaration publishes.
struct InheritedForwarder {
    /// The interface the forwarder's target belongs to: the holder's interface, or the direct
    /// superinterface an `invokespecial` names.
    target_interface: TypeName,
    /// The member's name before value-class mangling: its declared or accessor name.
    declared_name: String,
    /// The member's JVM name, which the forwarder takes.
    name: String,
    /// The forwarder's own parameters and result: the member as the class's applied supertype
    /// substitutes it.
    param_tys: Vec<Ty>,
    semantic_params: Vec<Ty>,
    parameter_identities: Vec<crate::fir::ResolvedParameterIdentity>,
    ret: Ty,
    semantic_ret: Ty,
    /// The inherited declaration's own erased parameters and result, which every call to it names.
    /// They differ from the forwarder's only when the supertype specializes the member, and then
    /// an erased bridge also carries them.
    target_params: Vec<Ty>,
    target_ret: Ty,
    /// The forwarder's generic `Signature`, when its substituted types have generic structure.
    signature: Option<String>,
    target_owner: String,
    target_descriptor: String,
    dispatch: ForwarderDispatch,
}

impl InheritedForwarder {
    fn descriptor(&self) -> String {
        method_descriptor(&self.param_tys, self.ret)
    }

    /// The inherited declaration's erased descriptor.
    fn target_member_descriptor(&self) -> String {
        method_descriptor(&self.target_params, self.target_ret)
    }

    /// Load the forwarder's parameters from `first_slot` as the inherited declaration takes them:
    /// a primitive the substitution introduced is boxed for the erased type-parameter slot.
    fn load_target_arguments(&self, cw: &mut ClassWriter, code: &mut CodeBuilder, first_slot: u16) {
        let mut slot = first_slot;
        for (parameter, target) in self.param_tys.iter().zip(&self.target_params) {
            load(*parameter, slot, code);
            if parameter.is_jvm_scalar() && target.is_reference() {
                box_prim_free(cw, code, *parameter);
            }
            slot += slot_words(*parameter);
        }
    }

    /// Cast the inherited declaration's erased result back to the forwarder's substituted one.
    fn cast_target_result(&self, cw: &mut ClassWriter, code: &mut CodeBuilder) {
        if self.target_ret != self.ret && self.target_ret.is_reference() && self.ret.is_reference()
        {
            let result = cw.class_ref(&crate::jvm::names::instanceof_internal_name(self.ret));
            code.checkcast(result);
        }
    }

    /// The local-variable spelling of each declaration parameter; the forwarder, its `-impl` static
    /// and the static's parameter checks all name a parameter this way.
    fn local_variable_names(&self) -> Vec<Option<String>> {
        crate::jvm::parameter_names::resolved_local_variables(
            &self.parameter_identities,
            &self.semantic_params,
            &self.name,
        )
    }

    fn method_parameter_names(&self) -> Vec<Option<String>> {
        crate::jvm::parameter_names::resolved_method_parameters(
            &self.parameter_identities,
            &self.semantic_params,
            &self.name,
        )
    }

    fn result_annotation(&self) -> Option<&'static str> {
        forwarder_nullability(self.semantic_ret)
    }

    fn parameter_annotations(&self) -> Vec<Option<&'static str>> {
        self.semantic_params
            .iter()
            .copied()
            .map(forwarder_nullability)
            .collect()
    }
}

/// `@NotNull`/`@Nullable` for one semantic type of a forwarder: reference types only, and never a
/// bare type variable.
fn forwarder_nullability(ty: Ty) -> Option<&'static str> {
    if matches!(ty.non_null(), Ty::TyParam(..)) || !ir_ty_to_jvm(&ty).is_reference() {
        None
    } else if ty.is_nullable() {
        Some("Lorg/jetbrains/annotations/Nullable;")
    } else {
        Some("Lorg/jetbrains/annotations/NotNull;")
    }
}

/// Emit an ordinary class's forwarders. A value class writes its own, each beside its `-impl`
/// static, through [`emit_value_class_inherited_defaults`].
pub(super) fn emit_default_impls_forwarders(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    cw: &mut ClassWriter,
    env: &EmitEnv,
) {
    if c.is_value {
        return;
    }
    for forwarder in forwarders(ir, c, env) {
        write_forwarder(c, cw, &forwarder, env.java_parameters);
    }
}

/// Emit a value class's forwarders, each preceded by the static kotlinc's value-class lowering
/// gives every compatibility forwarder (the fake override it replaces): `name-impl(carrier, …)`
/// boxes the carrier and calls the forwarder. They follow the class's own members and precede its
/// private primary constructor.
pub(super) fn emit_value_class_inherited_defaults(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    cw: &mut ClassWriter,
    env: &EmitEnv,
) {
    debug_assert!(c.is_value);
    for forwarder in forwarders(ir, c, env) {
        write_value_class_static(c, cw, &forwarder, env.java_parameters);
        write_forwarder(c, cw, &forwarder, env.java_parameters);
    }
}

/// The forwarders `c` realizes its inherited defaults with.
///
/// Under `-jvm-default=disable` kotlinc emits `public <ret> f(args) { return
/// I$DefaultImpls.f(this, args); }`; under `enable` the forwarder is an `invokespecial` of the
/// interface's default method, and `no-compatibility` emits none. A dependency's `disable`
/// realization is reached through its holder in every mode, and a Java default method never gets a
/// forwarder.
fn forwarders(ir: &IrFile, c: &crate::ir::IrClass, env: &EmitEnv) -> Vec<InheritedForwarder> {
    if c.is_interface {
        return Vec::new();
    }
    let formatter = JvmSignatureFormatter::new(ir, env);
    // The type parameters in scope of the class's members, by semantic identity: its own, then
    // those it captures from an enclosing declaration.
    let class_type_params = ir
        .class_signature_name(c.fq_name)
        .into_iter()
        .flat_map(|signature| &signature.type_params)
        .map(|parameter| parameter.semantic_name.clone())
        .chain(c.captured_type_params.iter().cloned())
        .collect::<Vec<_>>();
    ir.inherited_defaults
        .get(&c.fq_name)
        .into_iter()
        .flatten()
        .filter_map(|default| {
            forwarder(ir, &formatter, &class_type_params, default, env.jvm_default)
        })
        .collect()
}

fn forwarder(
    ir: &IrFile,
    formatter: &JvmSignatureFormatter<'_>,
    class_type_params: &[String],
    default: &crate::fir::ResolvedInheritedDefault,
    jvm_default: JvmDefaultMode,
) -> Option<InheritedForwarder> {
    use crate::jvm::inherited_default_realization::ForwarderRealization as Realization;
    let realization =
        crate::jvm::inherited_default_realization::forwarder_realization(default, jvm_default)?;
    let declared = crate::jvm::value_classes::forwarded_member_types(ir, default);
    // A suspend member keeps its declared CPS shape.
    let types = if default.suspend {
        crate::jvm::value_classes::forwarded_member_types(ir, default)
    } else {
        crate::jvm::value_classes::specialized_member_types(ir, default)
    };
    let mut param_tys = jvm_tys(&types.physical_params);
    let mut ret = jvm_declared_ty(&types.physical_ret);
    let mut target_params = jvm_tys(&declared.physical_params);
    let mut target_ret = jvm_declared_ty(&declared.physical_ret);
    let mut semantic_params = types.semantic_params;
    let mut parameter_identities = default.parameter_identities.to_vec();
    let mut semantic_ret = types.semantic_ret;
    let signature = forwarder_signature(
        formatter,
        class_type_params,
        &semantic_params,
        semantic_ret,
        (&param_tys, ret),
    );
    // A `suspend` member's PHYSICAL realization is its CPS shape — a trailing `Continuation` and an
    // `Object` return. kotlinc names the parameter `$completion`, annotates it `@NotNull`, and the
    // return `@Nullable`.
    if default.suspend {
        for params in [&mut param_tys, &mut target_params] {
            params.push(Ty::obj("kotlin/coroutines/Continuation"));
        }
        ret = Ty::obj("java/lang/Object");
        target_ret = ret;
        parameter_identities.push(crate::fir::ResolvedParameterIdentity::SuspendCompletion);
        semantic_params.push(Ty::obj("kotlin/coroutines/Continuation"));
        semantic_ret = Ty::nullable(Ty::obj("java/lang/Object"));
    }
    let dispatch_interface = default.dispatch_interface;
    let (target_interface, target_owner, target_descriptor, dispatch) = match realization {
        Realization::DependencyHolder(holder) => (
            default.declaring_interface,
            holder.owner.render(),
            holder.descriptor.clone(),
            ForwarderDispatch::HolderStatic,
        ),
        Realization::DispatchHolder => {
            let mut with_receiver = vec![Ty::obj_name(dispatch_interface)];
            with_receiver.extend_from_slice(&target_params);
            (
                dispatch_interface,
                crate::types::type_name_nested_child(dispatch_interface, "DefaultImpls").render(),
                method_descriptor(&with_receiver, target_ret),
                ForwarderDispatch::HolderStatic,
            )
        }
        Realization::InterfaceSpecial => (
            dispatch_interface,
            dispatch_interface.render(),
            method_descriptor(&target_params, target_ret),
            ForwarderDispatch::InterfaceSpecial,
        ),
    };
    Some(InheritedForwarder {
        target_interface,
        declared_name: crate::jvm::inherited_default_realization::inherited_member_declared_name(
            default,
        ),
        name: crate::jvm::inherited_default_realization::inherited_member_jvm_name(ir, default),
        param_tys,
        semantic_params,
        parameter_identities,
        ret,
        semantic_ret,
        target_params,
        target_ret,
        signature,
        target_owner,
        target_descriptor,
        dispatch,
    })
}

/// The generic `Signature` of a forwarder whose types have generic structure (`List<String>`, a
/// class type parameter), or `None` when it would only repeat the descriptor.
///
/// A member's own type parameters would need their declarations ahead of the parameter list,
/// which the inherited record does not carry; such a forwarder keeps descriptor-only shape
/// (recorded in docs/SPEC.md).
fn forwarder_signature(
    formatter: &JvmSignatureFormatter<'_>,
    class_type_params: &[String],
    semantic_params: &[Ty],
    semantic_ret: Ty,
    (param_tys, ret): (&[Ty], Ty),
) -> Option<String> {
    // A member's own type parameter (`fun <T> echo`) needs its declaration in the forwarder's
    // signature, which this record does not carry: keep the descriptor-only shape for it.
    if semantic_params
        .iter()
        .chain(std::iter::once(&semantic_ret))
        .any(|ty| mentions_other_type_parameter(*ty, class_type_params))
    {
        return None;
    }
    let mut signature = String::from("(");
    for parameter in semantic_params {
        signature.push_str(&formatter.method_ty(parameter, Wildcards::Declared)?);
    }
    signature.push(')');
    signature.push_str(&formatter.method_ty(&semantic_ret, Wildcards::Suppressed)?);
    (signature != method_descriptor(param_tys, ret)).then_some(signature)
}

/// Whether `ty` mentions a type parameter not named in `known`.
fn mentions_other_type_parameter(ty: Ty, known: &[String]) -> bool {
    match ty {
        Ty::TyParam(name, _) => !known.iter().any(|known| known == name),
        Ty::DefinitelyNotNull(inner)
        | Ty::Nullable(inner)
        | Ty::PlatformNullable(inner)
        | Ty::InProjection(inner)
        | Ty::OutProjection(inner)
        | Ty::StarProjection(inner) => mentions_other_type_parameter(*inner, known),
        Ty::Intersection(parts) => parts
            .iter()
            .any(|part| mentions_other_type_parameter(*part, known)),
        Ty::Obj(_, arguments) => arguments
            .iter()
            .any(|argument| mentions_other_type_parameter(*argument, known)),
        Ty::Fun(signature) => {
            mentions_other_type_parameter(signature.ret, known)
                || signature
                    .params
                    .iter()
                    .any(|parameter| mentions_other_type_parameter(*parameter, known))
        }
        _ => false,
    }
}

/// The line kotlinc maps a generated member of `c` to: where the class declaration starts,
/// annotations included.
fn class_start_line(c: &crate::ir::IrClass) -> u32 {
    if c.decl_start_line == 0 {
        c.decl_line
    } else {
        c.decl_start_line
    }
}

/// Write one forwarder: `ACC_PUBLIC | ACC_BRIDGE`, its arguments passed through unchanged.
fn write_forwarder(
    c: &crate::ir::IrClass,
    cw: &mut ClassWriter,
    forwarder: &InheritedForwarder,
    java_parameters: bool,
) {
    let InheritedForwarder {
        target_interface,
        name,
        param_tys,
        parameter_identities,
        ret,
        target_owner,
        target_descriptor,
        dispatch,
        ..
    } = forwarder;
    let (name, ret, dispatch) = (name.as_str(), *ret, *dispatch);
    assert_eq!(
        parameter_identities.len(),
        param_tys.len(),
        "an inherited forwarder needs every declaration parameter identity"
    );
    let desc = forwarder.descriptor();
    let parameter_annotations = forwarder.parameter_annotations();
    // The forwarder is an instance method again, so an extension receiver is an ordinary,
    // unflagged parameter.
    let reflected = if java_parameters {
        crate::jvm::method_parameters::resolved_class_forwarder(
            parameter_identities,
            &forwarder.method_parameter_names(),
            param_tys,
        )
    } else {
        Vec::new()
    };
    // The header — name, descriptor, the `MethodParameters` names, then the return's and
    // parameters' nullability types — interns before the body, as ASM's `visitMethod`,
    // `visitParameter` and annotation visits precede the code.
    let header_annotations = std::iter::once(forwarder.result_annotation())
        .chain(parameter_annotations.iter().copied())
        .flatten()
        .collect::<Vec<_>>();
    cw.reserve_method_pool_with_annotations(
        name,
        &desc,
        forwarder.signature.as_deref(),
        &header_annotations,
        &crate::ir::DeclarationAnnotations::default(),
        &reflected,
    );
    let argument_words = 1 + param_tys.iter().map(|t| slot_words(*t)).sum::<u16>();
    let target_words = 1 + forwarder
        .target_params
        .iter()
        .map(|t| slot_words(*t) as i32)
        .sum::<i32>();
    let result_words = slot_words(forwarder.target_ret) as i32;
    let mut code = CodeBuilder::new(argument_words);
    code.aload(0);
    // A holder static takes the interface instance first. kotlinc reinterprets a value class's box
    // as that interface with a `checkcast`; an ordinary class passes `this` as it is.
    if c.is_value && dispatch == ForwarderDispatch::HolderStatic {
        let receiver = cw.class_ref(&target_interface.render());
        code.checkcast(receiver);
    }
    forwarder.load_target_arguments(cw, &mut code, 1);
    match dispatch {
        ForwarderDispatch::HolderStatic => {
            let target = cw.methodref(target_owner, name, target_descriptor);
            code.invokestatic(target, target_words, result_words);
        }
        // The interface publishes the body as a DEFAULT method: the forwarder is a Java-style
        // interface `super` call. The JVM requires the named interface to be a DIRECT
        // superinterface of this class — override resolution selects it.
        ForwarderDispatch::InterfaceSpecial => {
            let target = cw.interface_methodref(target_owner, name, target_descriptor);
            code.invokespecial(target, target_words, result_words);
        }
    }
    forwarder.cast_target_result(cw, &mut code);
    emit_return(ret, &mut code);
    finish_code_sig::<0x0041>(
        cw,
        name,
        &desc,
        &mut code,
        argument_words,
        forwarder.signature.as_deref(),
    );
    cw.set_method_parameters(name, &desc, &reflected);
    let mut locals = vec![("this".to_string(), format!("L{};", c.fq_name()), 0)];
    let mut slot = 1u16;
    for (parameter, parameter_name) in param_tys.iter().zip(forwarder.local_variable_names()) {
        if let Some(parameter_name) = parameter_name {
            locals.push((parameter_name, local_variable_desc(*parameter), slot));
        }
        slot += slot_words(*parameter);
    }
    let line = class_start_line(c);
    cw.set_method_debug(name, &desc, (line != 0).then_some((0, line)), &locals);
    cw.set_method_nullability(
        name,
        &desc,
        forwarder.result_annotation(),
        &parameter_annotations,
    );
    // Only a holder call makes the class REFERENCE the nested holder; an `invokespecial`
    // forwarder names the interface alone, and kotlinc records no `InnerClasses` entry for it.
    if dispatch == ForwarderDispatch::HolderStatic {
        cw.add_inner_class(crate::jvm::classfile::InnerClassSpec {
            inner: target_owner.to_string(),
            outer: Some(target_interface.render()),
            name: Some("DefaultImpls".to_string()),
            access: 0x0019,
        });
    }
}

/// Write the static a value class keeps for one inherited forwarder: `public static`, the carrier
/// first, then the forwarder's parameters. It checks each `@NotNull` parameter, boxes the carrier
/// and calls the forwarder on the box. Like every generated static it maps no line; its locals and
/// `MethodParameters` name the carrier `arg0` (synthetic) and move an extension receiver into a
/// mandated parameter.
fn write_value_class_static(
    c: &crate::ir::IrClass,
    cw: &mut ClassWriter,
    forwarder: &InheritedForwarder,
    java_parameters: bool,
) {
    let carrier = jvm_declared_ty(
        &c.fields
            .first()
            .expect("a value class stores its carrier in its one field")
            .ty,
    );
    let name = crate::jvm::value_classes::inherited_member_impl_name(
        &forwarder.declared_name,
        &forwarder.name,
    );
    let carrier_identity = crate::ir::IrParameterIdentity::generated(
        crate::ir::IrGeneratedParameterRole::ValueClassCarrier,
        None,
    );
    let carrier_name = crate::jvm::parameter_names::local_variable(&carrier_identity, &name)
        .expect("a value-class carrier has a JVM name");
    let mut params = vec![carrier];
    params.extend_from_slice(&forwarder.param_tys);
    let desc = method_descriptor(&params, forwarder.ret);
    let local_variable_names = forwarder.local_variable_names();
    assert_eq!(
        local_variable_names.len(),
        forwarder.param_tys.len(),
        "a value-class inherited static names every declaration parameter"
    );
    let reflected = if java_parameters {
        crate::jvm::method_parameters::value_class_inherited_static(
            &carrier_name,
            &forwarder.parameter_identities,
            &forwarder.method_parameter_names(),
            &forwarder.param_tys,
        )
    } else {
        Vec::new()
    };
    let mut parameter_annotations = vec![None];
    parameter_annotations.extend(forwarder.parameter_annotations());
    let header_annotations = std::iter::once(forwarder.result_annotation())
        .chain(parameter_annotations.iter().copied())
        .flatten()
        .collect::<Vec<_>>();
    cw.reserve_method_pool_with_annotations(
        &name,
        &desc,
        None,
        &header_annotations,
        &crate::ir::DeclarationAnnotations::default(),
        &reflected,
    );
    let argument_words = params.iter().map(|ty| slot_words(*ty)).sum::<u16>();
    let mut code = CodeBuilder::new(argument_words);
    let mut slot = slot_words(carrier);
    for (index, parameter) in forwarder.param_tys.iter().enumerate() {
        // The continuation is the compiler's own argument, never a checked declaration parameter.
        let checked = parameter_annotations[index + 1]
            == Some("Lorg/jetbrains/annotations/NotNull;")
            && !matches!(
                forwarder.parameter_identities[index],
                crate::fir::ResolvedParameterIdentity::SuspendCompletion
            );
        if checked {
            let label = local_variable_names[index]
                .as_deref()
                .expect("a checked inherited parameter has a JVM name");
            code.aload(slot);
            code.push_string(label, cw);
            let check = cw.methodref(
                "kotlin/jvm/internal/Intrinsics",
                "checkNotNullParameter",
                "(Ljava/lang/Object;Ljava/lang/String;)V",
            );
            code.invokestatic(check, 2, 0);
        }
        slot += slot_words(*parameter);
    }
    load(carrier, 0, &mut code);
    let owner = c.fq_name();
    let box_impl = cw.methodref(
        &owner,
        "box-impl",
        &method_descriptor(&[carrier], Ty::obj_name(c.fq_name)),
    );
    code.invokestatic(box_impl, slot_words(carrier) as i32, 1);
    // kotlinc calls the box's entry for the inherited declaration itself — under a specializing
    // supertype that is the erased bridge — and casts its result back.
    forwarder.load_target_arguments(cw, &mut code, slot_words(carrier));
    let entry = cw.methodref(
        &owner,
        &forwarder.name,
        &forwarder.target_member_descriptor(),
    );
    let parameter_words = forwarder
        .target_params
        .iter()
        .map(|ty| slot_words(*ty) as i32)
        .sum::<i32>();
    code.invokevirtual(
        entry,
        parameter_words,
        slot_words(forwarder.target_ret) as i32,
    );
    forwarder.cast_target_result(cw, &mut code);
    emit_return(forwarder.ret, &mut code);
    finish_code::<0x0009>(cw, &name, &desc, &mut code, argument_words);
    cw.set_method_parameters(&name, &desc, &reflected);
    let mut locals = vec![(carrier_name, local_variable_desc(carrier), 0)];
    let mut slot = slot_words(carrier);
    for (parameter, parameter_name) in forwarder.param_tys.iter().zip(local_variable_names) {
        if let Some(parameter_name) = parameter_name {
            locals.push((parameter_name, local_variable_desc(*parameter), slot));
        }
        slot += slot_words(*parameter);
    }
    cw.set_method_debug(&name, &desc, None, &locals);
    cw.set_method_nullability(
        &name,
        &desc,
        forwarder.result_annotation(),
        &parameter_annotations,
    );
}
