//! JVM-default compatibility-holder forwarding.

use super::*;

/// Under `enable`, an interface REPUBLISHES the compatibility surface for every non-abstract,
/// non-private member it inherits and does not redeclare: kotlinc gives even an empty
/// `interface B : A` its own `access$f$jd` bridge (an `invokespecial` through B itself, which the
/// JVM resolves to the inherited default) and a `B$DefaultImpls` holder forwarding to that bridge.
/// A member inherited from a `disable`-compiled dependency has no default method to bridge to —
/// its holder entry forwards straight to the dependency's own holder (behind a `checkcast`), with
/// no `access$…$jd` bridge and no `@Deprecated`, exactly as measured on kotlinc 2.4.10.
pub(super) fn emit_inherited_default_surface(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    cw: &mut ClassWriter,
    default_impls: &mut Option<ClassWriter>,
    opts: &EmitOptions,
    env: &EmitEnv,
) {
    let symbols = env.signature_symbols;
    let closure = sorted_interface_closure(symbols, c.interfaces.iter_ids().collect());
    if closure.is_empty() {
        return;
    }
    let fq_name = c.fq_name();
    let method_key = |name: &str, params: &[Ty]| {
        (
            name.to_string(),
            params
                .iter()
                .map(|parameter| crate::jvm::names::type_descriptor(*parameter))
                .collect::<String>(),
        )
    };
    // A member this interface declares (bodied or abstract) is its own: an abstract redeclaration
    // suppresses an ancestor's body here exactly as on an implementing class.
    let mut selected = c
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
    for (interface, shape) in closure {
        let mut surface = |name: &str,
                           param_tys: &[Ty],
                           semantic_params: &[Ty],
                           local_variable_names: &[Option<String>],
                           method_parameter_names: &[Option<String>],
                           assertion_names: &[Option<String>],
                           physical_ret: Ty,
                           semantic_ret: Ty,
                           is_abstract: bool,
                           visibility: crate::types::Visibility,
                           realization: crate::libraries::MemberRealization,
                           owner: Option<crate::types::TypeName>,
                           descriptor: &str,
                           cw: &mut ClassWriter,
                           default_impls: &mut Option<ClassWriter>| {
            let key = method_key(name, param_tys);
            // The nearest declaration wins even when it is abstract.
            if !selected.insert(key) {
                return;
            }
            if is_abstract || visibility == crate::types::Visibility::Private {
                return;
            }
            let target = match realization {
                // The dependency's `disable` realization: its holder static is the only body.
                crate::libraries::MemberRealization::Direct {
                    pass_receiver: true,
                } => {
                    let Some(holder) = owner else { return };
                    JdHolderTarget::DependencyHolder {
                        declaring: interface,
                        holder,
                        descriptor,
                    }
                }
                // A Kotlin default method somewhere above: bridge through this interface itself.
                crate::libraries::MemberRealization::Dispatch if shape.is_kotlin => {
                    JdHolderTarget::AccessBridge
                }
                // A JAVA default method never joins the Kotlin compatibility surface.
                _ => return,
            };
            if matches!(target, JdHolderTarget::AccessBridge) {
                emit_jd_access_bridge(
                    cw,
                    c.fq_name,
                    c.decl_line,
                    name,
                    param_tys,
                    local_variable_names,
                    physical_ret,
                );
            }
            let di = default_impls.get_or_insert_with(|| {
                let mut w =
                    new_writer(&format!("{fq_name}$DefaultImpls"), "java/lang/Object", opts);
                w.set_access(0x0011 | 0x0020); // PUBLIC | FINAL | SUPER
                w
            });
            if let JdHolderTarget::DependencyHolder {
                declaring, holder, ..
            } = &target
            {
                // The holder references the dependency's nested holder — kotlinc records BOTH
                // `InnerClasses` relations on it.
                di.add_inner_class(crate::jvm::classfile::InnerClassSpec {
                    inner: holder.render(),
                    outer: Some(declaring.render()),
                    name: Some("DefaultImpls".to_string()),
                    access: 0x0019,
                });
            }
            // The guard set of an inherited member follows its semantic parameter nullability —
            // the same selection its `@NotNull` parameter annotations use.
            let guards: Vec<Option<String>> = if opts.param_assertions {
                semantic_params
                    .iter()
                    .enumerate()
                    .map(|(index, parameter)| {
                        (jd_nullability_annotation(parameter)
                            == Some("Lorg/jetbrains/annotations/NotNull;"))
                        .then(|| {
                            assertion_names[index]
                                .clone()
                                .expect("a checked inherited parameter has a semantic JVM label")
                        })
                    })
                    .collect()
            } else {
                Vec::new()
            };
            emit_holder_forward(
                di,
                c.fq_name,
                name,
                param_tys,
                semantic_params,
                local_variable_names,
                method_parameter_names,
                &guards,
                physical_ret,
                semantic_ret,
                // The promoted generic signature of an inherited member is not reconstructed here
                // (the declaring classifier's formals are not this interface's); a generic
                // inherited surface keeps descriptor-only shape. Recorded in docs/SPEC.md.
                None,
                c.decl_line,
                opts.java_parameters,
                target,
            );
        };
        for member in &shape.surface {
            let physical_params = if member.physical_params.len() == member.params.len() {
                member.physical_params.clone()
            } else {
                member.params.clone()
            };
            let name = backend_member_jvm_name(member);
            let mut param_tys = jvm_tys(&physical_params);
            let mut physical_ret = jvm_declared_ty(&member.physical_ret);
            let mut semantic_params = member.params.to_vec();
            let mut local_variable_names = crate::jvm::parameter_names::resolved_local_variables(
                &member.parameter_identities,
                &semantic_params,
                &name,
            );
            let mut method_parameter_names =
                crate::jvm::parameter_names::resolved_method_parameters(
                    &member.parameter_identities,
                    &semantic_params,
                    &name,
                );
            let mut assertion_names = crate::jvm::parameter_names::resolved_assertions(
                &member.parameter_identities,
                &semantic_params,
                &name,
            );
            let mut semantic_ret = member.ret;
            assert_eq!(
                local_variable_names.len(),
                semantic_params.len(),
                "a republished member needs exact metadata parameter identities"
            );
            // A `suspend` member republishes in its CPS shape — trailing `Continuation`
            // (`$completion`, `@NotNull`) and a `@Nullable Object` return — like the
            // implementing-class forwarders.
            if member.suspend() {
                param_tys.push(Ty::obj("kotlin/coroutines/Continuation"));
                physical_ret = Ty::obj("java/lang/Object");
                local_variable_names.push(Some("$completion".to_string()));
                method_parameter_names.push(Some("$completion".to_string()));
                assertion_names.push(Some("$completion".to_string()));
                semantic_params.push(Ty::obj("kotlin/coroutines/Continuation"));
                semantic_ret = Ty::nullable(Ty::obj("java/lang/Object"));
            }
            surface(
                &name,
                &param_tys,
                &semantic_params,
                &local_variable_names,
                &method_parameter_names,
                &assertion_names,
                physical_ret,
                semantic_ret,
                member.is_abstract(),
                member.visibility,
                member.realization,
                member.owner,
                &member.descriptor,
                cw,
                default_impls,
            );
        }
    }
}

/// Emit one receiver-first `$DefaultImpls` forward to an interface bridge or dependency holder.
#[allow(clippy::too_many_arguments)]
pub(super) fn emit_holder_forward(
    cw: &mut ClassWriter,
    interface: crate::types::TypeName,
    member_name: &str,
    param_tys: &[Ty],
    semantic_params: &[Ty],
    local_variable_names: &[Option<String>],
    method_parameter_names: &[Option<String>],
    guards: &[Option<String>],
    ret: Ty,
    semantic_ret: Ty,
    signature: Option<&str>,
    decl_line: u32,
    java_parameters: bool,
    target: JdHolderTarget<'_>,
) {
    assert_eq!(
        local_variable_names.len(),
        param_tys.len(),
        "a compatibility holder needs exact declaration parameter identities"
    );
    assert_eq!(method_parameter_names.len(), param_tys.len());
    let fq = interface.render();
    let mut with_receiver = vec![Ty::obj_name(interface)];
    with_receiver.extend_from_slice(param_tys);
    let desc = method_descriptor(&with_receiver, ret);
    cw.seed_utf8(member_name);
    cw.seed_utf8(&desc);
    if let Some(signature) = signature {
        cw.seed_utf8(signature);
    }
    let method_parameters = if java_parameters {
        super::super::method_parameters::holder_forward(method_parameter_names, param_tys)
    } else {
        Vec::new()
    };
    for (parameter, _) in &method_parameters {
        if let Some(parameter) = parameter {
            cw.seed_utf8(parameter);
        }
    }
    let deprecated = matches!(target, JdHolderTarget::AccessBridge);
    if deprecated {
        cw.seed_utf8("Ljava/lang/Deprecated;");
    }
    let ret_ann = jd_nullability_annotation(&semantic_ret);
    if let Some(annotation) = ret_ann {
        cw.seed_utf8(annotation);
    }
    let mut parameter_annotations = vec![Some("Lorg/jetbrains/annotations/NotNull;")];
    parameter_annotations.extend(semantic_params.iter().map(jd_nullability_annotation));
    for annotation in parameter_annotations.iter().flatten() {
        cw.seed_utf8(annotation);
    }

    let argument_words = 1 + param_tys.iter().map(|ty| slot_words(*ty)).sum::<u16>();
    let mut code = CodeBuilder::new(argument_words);
    let mut slot = 1u16;
    for (index, guard) in guards.iter().enumerate().take(param_tys.len()) {
        if let Some(parameter_name) = guard {
            code.aload(slot);
            code.push_string(parameter_name, cw);
            let check = cw.methodref(
                "kotlin/jvm/internal/Intrinsics",
                "checkNotNullParameter",
                "(Ljava/lang/Object;Ljava/lang/String;)V",
            );
            code.invokestatic(check, 2, 0);
        }
        slot += slot_words(param_tys[index]);
    }
    if decl_line != 0 {
        code.mark_line(decl_line);
    }
    code.aload(0);
    if let JdHolderTarget::DependencyHolder { declaring, .. } = &target {
        let declaring_class = cw.class_ref(&declaring.render());
        code.checkcast(declaring_class);
    }
    let mut slot = 1u16;
    for ty in param_tys {
        load(*ty, slot, &mut code);
        slot += slot_words(*ty);
    }
    match target {
        JdHolderTarget::AccessBridge => {
            let bridge = cw.interface_methodref(&fq, &format!("access${member_name}$jd"), &desc);
            code.invokestatic(bridge, argument_words as i32, slot_words(ret) as i32);
        }
        JdHolderTarget::DependencyHolder {
            holder, descriptor, ..
        } => {
            let target = cw.methodref(&holder.render(), member_name, descriptor);
            code.invokestatic(target, argument_words as i32, slot_words(ret) as i32);
        }
    }
    emit_return(ret, &mut code);
    code.ensure_locals(argument_words);
    code.link();
    cw.add_method_sig(0x0009, member_name, &desc, &code, signature);
    cw.set_method_parameters(member_name, &desc, &method_parameters);

    let mut locals = vec![("$this".to_string(), format!("L{fq};"), 0)];
    let mut slot = 1u16;
    for (index, parameter) in param_tys.iter().enumerate() {
        // A `None` entry is intentionally omitted rather than receiving a positional name.
        if let Some(parameter_name) = &local_variable_names[index] {
            let parameter_name = parameter_name.clone();
            locals.push((parameter_name, local_variable_desc(*parameter), slot));
        }
        slot += slot_words(*parameter);
    }
    cw.set_method_debug(member_name, &desc, None, &locals);
    if deprecated {
        cw.mark_method_deprecated(member_name, &desc);
        cw.add_method_visible_marker_annotation(member_name, &desc, "Ljava/lang/Deprecated;");
    }
    if ret_ann.is_some() || parameter_annotations.iter().any(Option::is_some) {
        cw.set_method_nullability(member_name, &desc, ret_ann, &parameter_annotations);
    }
}
