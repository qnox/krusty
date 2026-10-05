//! Compatibility forwarders an implementing class writes for the interface default members it
//! inherits and does not override, and the `-impl` static a value class realizes each one with.
//!
//! Which members get a forwarder is read from the class's interface closure over the normalized
//! classifier model ([`interface_hierarchy`]); every forwarder is planned once, then written at the
//! position its class kind gives it: after the bridges' predecessors for an ordinary class, and
//! beside its static before the private primary constructor for a value class.

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
    param_tys: Vec<Ty>,
    semantic_params: Vec<Ty>,
    parameter_identities: Vec<crate::fir::ResolvedParameterIdentity>,
    ret: Ty,
    semantic_ret: Ty,
    target_owner: String,
    target_descriptor: String,
    dispatch: ForwarderDispatch,
}

impl InheritedForwarder {
    fn descriptor(&self) -> String {
        method_descriptor(&self.param_tys, self.ret)
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

/// The member's name before value-class mangling: its declared name, or its accessor's.
fn backend_member_declared_name(member: &crate::backend::BackendMemberFact) -> String {
    match &member.name {
        crate::backend::BackendMemberName::Declared(name) => name.to_string(),
        crate::backend::BackendMemberName::PropertyGetter(name) => {
            crate::jvm::names::property_getter_name(name)
        }
        crate::backend::BackendMemberName::PropertySetter(name) => {
            crate::jvm::names::property_setter_name(name)
        }
    }
}

pub(super) fn backend_member_jvm_name(
    ir: &IrFile,
    member: &crate::backend::BackendMemberFact,
) -> String {
    if let Some(name) = &member.physical_name {
        return name.to_string();
    }
    crate::jvm::value_classes::module_member_jvm_name(
        ir,
        &backend_member_declared_name(member),
        &member.params,
        &member.ret,
        member.suspend(),
    )
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
    for forwarder in plan(ir, c, env) {
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
    for forwarder in plan(ir, c, env) {
        write_value_class_static(c, cw, &forwarder, env.java_parameters);
        write_forwarder(c, cw, &forwarder, env.java_parameters);
    }
}

/// Select the inherited members that need a forwarder on `c`.
///
/// Under `-jvm-default=disable` kotlinc emits `public <ret> f(args) { return
/// I$DefaultImpls.f(this, args); }`; under `enable` the forwarder is an `invokespecial` of the
/// interface's default method. A member the class declares itself is left alone — it already
/// overrides the abstract interface method. A member a superclass already realizes is left alone
/// too: the superclass method is that implementation, whether it is a real override or a forwarder
/// to the same interface declaration. A subclass that inherits a more specific interface override
/// still forwards to that declaration.
fn plan(ir: &IrFile, c: &crate::ir::IrClass, env: &EmitEnv) -> Vec<InheritedForwarder> {
    if c.is_interface {
        return Vec::new();
    }
    let symbols = env.signature_symbols;
    let derives_from = |candidate: crate::types::TypeName, ancestor: crate::types::TypeName| {
        interface_hierarchy::derives_from(symbols, candidate, ancestor)
    };
    let closure = interface_hierarchy::sorted_closure(symbols, c.interfaces.iter_ids().collect());
    let superclass_interfaces = ir
        .superclass_interfaces
        .get(&c.fq_name)
        .map(Vec::as_slice)
        .unwrap_or_default();

    let method_key = |name: &str, params: &[Ty]| {
        (
            name.to_string(),
            params
                .iter()
                .map(|parameter| crate::jvm::names::type_descriptor(*parameter))
                .collect::<String>(),
        )
    };
    // Include already-derived generic/covariant bridges. They are emitted before these forwarders;
    // ignoring them creates duplicate `(name, descriptor)` methods on a concrete implementer.
    let mut implemented = c
        .methods
        .iter()
        .map(|fid| {
            let function = &ir.functions[*fid as usize];
            method_key(&function.name, &jvm_function_params(ir, *fid))
        })
        .chain(
            c.bridges
                .iter()
                .map(|bridge| method_key(&bridge.name, &bridge.erased_params)),
        )
        .collect::<std::collections::HashSet<_>>();
    // A property override is realized as a FIELD-backed or computed accessor synthesized OUTSIDE
    // `c.methods` (`class Ann : Greeter { override val who = "HR" }` derives `getWho` from the
    // field). Without these keys the pass emits a forwarder DUPLICATING that accessor —
    // `ClassFormatError: Duplicate method name` at class-load time. Keyed by FULL descriptor and
    // recorded only for accessors the class actually EMITS: a `val` has no setter (suppressing an
    // inherited `setX(I)V` forwarder on its name alone left the class abstract), and a same-name
    // accessor with a DIFFERENT return coexists with the forwarder on the JVM — kotlinc emits
    // both `getX()Ljava/lang/String;` (the accessor) and `getX()I` (the forwarder).
    let mut emitted_accessors: std::collections::HashSet<(String, String)> =
        std::collections::HashSet::new();
    for (name, ty, has_setter, is_private) in c
        .fields
        .iter()
        .map(|field| {
            (
                field.name.as_str(),
                field.ty,
                !field.is_final(),
                field.is_private(),
            )
        })
        .chain(
            c.properties
                .iter()
                .map(|p| (p.name.as_str(), p.ty, p.is_var, p.visibility.is_private())),
        )
    {
        // A private property has no accessors — in-class reads go straight to the field.
        if is_private {
            continue;
        }
        let (getter, setter) = accessor_jvm_names(c, name);
        let jt = jvm_declared_ty(&ty);
        emitted_accessors.insert((getter, method_descriptor(&[], jt)));
        if has_setter {
            emitted_accessors.insert((setter, method_descriptor(&[jt], Ty::Unit)));
        }
    }
    // An `invokespecial` interface forwarder must NAME a direct superinterface of this class (the
    // JVM's rule for interface `super` calls); the body then resolves to the maximally-specific
    // default. Pick the first DECLARED superinterface through which the winning declaration is
    // inherited — measured on kotlinc: `class C : D, B` with the override on `B` names `B`, and
    // `class C : B` with the body up on `A` names `B`, not `A`.
    let interface_special_target = |declaring: crate::types::TypeName| {
        c.interfaces
            .iter_ids()
            .find(|&direct| direct == declaring || derives_from(direct, declaring))
    };
    let mut selected = std::collections::HashSet::new();
    let mut forwarders = Vec::new();
    for (interface, shape) in closure {
        for member in &shape.surface {
            let types = crate::jvm::value_classes::forwarded_member_types(ir, member, shape.source);
            let mut param_tys = jvm_tys(&types.physical_params);
            let mut ret = jvm_declared_ty(&types.physical_ret);
            let mut semantic_params = types.semantic_params;
            let mut parameter_identities = member.parameter_identities.to_vec();
            let mut semantic_ret = types.semantic_ret;
            assert_eq!(
                parameter_identities.len(),
                semantic_params.len(),
                "an inherited member needs exact metadata parameter identities"
            );
            // A `suspend` member's PHYSICAL realization is its CPS shape — a trailing
            // `Continuation` and an `Object` return. The semantic record keeps the declared
            // params/return, so adjust here or the forwarder declares a method the interface does
            // not have (and misses the class's own CPS-shaped override in `implemented`). kotlinc
            // names the parameter `$completion`, annotates it `@NotNull`, and the return
            // `@Nullable`.
            if member.suspend() {
                param_tys.push(Ty::obj("kotlin/coroutines/Continuation"));
                ret = Ty::obj("java/lang/Object");
                parameter_identities.push(crate::fir::ResolvedParameterIdentity::SuspendCompletion);
                semantic_params.push(Ty::obj("kotlin/coroutines/Continuation"));
                semantic_ret = Ty::nullable(Ty::obj("java/lang/Object"));
            }
            let name = backend_member_jvm_name(ir, member);
            let key = method_key(&name, &param_tys);
            // The nearest declaration wins even when it is abstract: an abstract redeclaration
            // suppresses a farther ancestor's body rather than exposing it as a fake override.
            if !selected.insert(key.clone()) {
                continue;
            }
            // Common resolution records which exact interface declarations arrive through the
            // direct superclass. Another compatibility forwarder would hide that inherited
            // implementation. A more-specific directly declared interface has a different
            // identity and therefore still receives its own forwarder.
            if superclass_interfaces.contains(&interface) {
                continue;
            }
            if member.is_abstract()
                || member.visibility == crate::types::Visibility::Private
                || implemented.contains(&key)
                || emitted_accessors.contains(&(name.clone(), method_descriptor(&param_tys, ret)))
            {
                continue;
            }
            // A dependency's `disable` realization: its holder static is the only body.
            let holder = member.nonvirtual.as_deref();
            let (target_interface, target_owner, target_descriptor, dispatch) =
                match (holder, member.realization) {
                    (Some(holder), _) => (
                        interface,
                        holder.owner.render(),
                        holder.descriptor.clone(),
                        ForwarderDispatch::HolderStatic,
                    ),
                    // This module's body under `disable`: every interface between the class and the
                    // declaration republishes it on its own holder, so the forwarder calls the holder
                    // of the direct superinterface it is inherited through, as it names that
                    // interface's default under `enable`.
                    (None, crate::libraries::MemberRealization::Dispatch)
                        if env.jvm_default == JvmDefaultMode::Disable && shape.source =>
                    {
                        let Some(named) = interface_special_target(interface) else {
                            continue;
                        };
                        let mut with_receiver = vec![Ty::obj_name(named)];
                        with_receiver.extend_from_slice(&param_tys);
                        (
                            named,
                            crate::types::type_name_nested_child(named, "DefaultImpls").render(),
                            method_descriptor(&with_receiver, ret),
                            ForwarderDispatch::HolderStatic,
                        )
                    }
                    // A KOTLIN interface member whose body is a JVM default method on the interface
                    // (this module under `enable`, or a dependency compiled under `enable`/
                    // `no-compatibility`): kotlinc forwards with a Java-style interface `super` call.
                    // A JAVA default method never gets a forwarder, and `no-compatibility` emits none.
                    (None, crate::libraries::MemberRealization::Dispatch)
                        if env.jvm_default != JvmDefaultMode::NoCompatibility
                            && shape.is_kotlin =>
                    {
                        let Some(named) = interface_special_target(interface) else {
                            continue;
                        };
                        (
                            named,
                            named.render(),
                            method_descriptor(&param_tys, ret),
                            ForwarderDispatch::InterfaceSpecial,
                        )
                    }
                    _ => continue,
                };
            implemented.insert(key);
            forwarders.push(InheritedForwarder {
                target_interface,
                declared_name: backend_member_declared_name(member),
                name,
                param_tys,
                semantic_params,
                parameter_identities,
                ret,
                semantic_ret,
                target_owner,
                target_descriptor,
                dispatch,
            });
        }
    }
    forwarders
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
        None,
        &header_annotations,
        &crate::ir::DeclarationAnnotations::default(),
        &reflected,
    );
    let argument_words = 1 + param_tys.iter().map(|t| slot_words(*t)).sum::<u16>();
    let mut code = CodeBuilder::new(argument_words);
    code.aload(0);
    // A holder static takes the interface instance first. kotlinc reinterprets a value class's box
    // as that interface with a `checkcast`; an ordinary class passes `this` as it is.
    if c.is_value && dispatch == ForwarderDispatch::HolderStatic {
        let receiver = cw.class_ref(&target_interface.render());
        code.checkcast(receiver);
    }
    let mut slot = 1u16;
    for ty in param_tys {
        load(*ty, slot, &mut code);
        slot += slot_words(*ty);
    }
    match dispatch {
        ForwarderDispatch::HolderStatic => {
            let target = cw.methodref(target_owner, name, target_descriptor);
            code.invokestatic(target, argument_words as i32, slot_words(ret) as i32);
        }
        // The interface publishes the body as a DEFAULT method: the forwarder is a Java-style
        // interface `super` call. The JVM requires the named interface to be a DIRECT
        // superinterface of this class — the planner selects it.
        ForwarderDispatch::InterfaceSpecial => {
            let target = cw.interface_methodref(target_owner, name, target_descriptor);
            code.invokespecial(target, argument_words as i32, slot_words(ret) as i32);
        }
    }
    emit_return(ret, &mut code);
    finish_code::<0x0041>(cw, name, &desc, &mut code, argument_words);
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
    let mut slot = slot_words(carrier);
    for ty in &forwarder.param_tys {
        load(*ty, slot, &mut code);
        slot += slot_words(*ty);
    }
    let entry = cw.methodref(&owner, &forwarder.name, &forwarder.descriptor());
    let parameter_words = argument_words - slot_words(carrier);
    code.invokevirtual(
        entry,
        parameter_words as i32,
        slot_words(forwarder.ret) as i32,
    );
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
