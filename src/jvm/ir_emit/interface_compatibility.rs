//! JVM-default compatibility-holder forwarding.

use super::*;

/// Under `enable`, an interface REPUBLISHES the compatibility surface for every non-abstract,
/// non-private member it inherits and does not redeclare: kotlinc gives even an empty
/// `interface B : A` its own `access$f$jd` bridge (an `invokespecial` through B itself, which the
/// JVM resolves to the inherited default) and a `B$DefaultImpls` holder forwarding to that bridge.
/// A member inherited from a `disable`-compiled dependency has no default method to bridge to —
/// its holder entry forwards straight to the dependency's own holder (behind a `checkcast`), with
/// no `access$…$jd` bridge and no `@Deprecated`, exactly as measured on kotlinc 2.4.10. Under
/// `disable` an interface republishes a member of this module's ancestor the same way, forwarding
/// to the ancestor's holder (`Left$DefaultImpls.f` → `Base$DefaultImpls.f`, measured on 2.4.20).
pub(super) fn emit_inherited_default_surface(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    cw: &mut ClassWriter,
    default_impls: &mut Option<ClassWriter>,
    opts: &EmitOptions,
    env: &EmitEnv,
) {
    let symbols = env.signature_symbols;
    let closure =
        super::interface_hierarchy::sorted_closure(symbols, c.interfaces.iter_ids().collect());
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
        let surface = |name: &str,
                       param_tys: &[Ty],
                       semantic_params: &[Ty],
                       local_variable_names: &[Option<String>],
                       reflected: &[crate::jvm::method_parameters::MethodParameter],
                       assertion_names: &[Option<String>],
                       identities: &[crate::fir::ResolvedParameterIdentity],
                       physical_ret: Ty,
                       semantic_ret: Ty,
                       varargs: u16,
                       realization: crate::libraries::MemberRealization,
                       nonvirtual: Option<&crate::libraries::NonvirtualCallRealization>,
                       cw: &mut ClassWriter,
                       default_impls: &mut Option<ClassWriter>| {
            let disable = opts.jvm_default == JvmDefaultMode::Disable;
            let module_holder_descriptor;
            let target = match (nonvirtual, realization) {
                // The dependency's `disable` realization: its holder static is the only body.
                (Some(holder), _) => JdHolderTarget::DependencyHolder {
                    declaring: interface,
                    holder: holder.owner,
                    descriptor: &holder.descriptor,
                },
                // Under `disable` this module's ancestor keeps the body on its own holder.
                (None, crate::libraries::MemberRealization::Dispatch)
                    if disable && shape.source =>
                {
                    let mut with_receiver = vec![Ty::obj_name(interface)];
                    with_receiver.extend_from_slice(param_tys);
                    module_holder_descriptor = method_descriptor(&with_receiver, physical_ret);
                    JdHolderTarget::DependencyHolder {
                        declaring: interface,
                        holder: crate::types::type_name_nested_child(interface, "DefaultImpls"),
                        descriptor: &module_holder_descriptor,
                    }
                }
                // A Kotlin default method somewhere above: bridge through this interface itself.
                (None, crate::libraries::MemberRealization::Dispatch)
                    if shape.is_kotlin && !disable =>
                {
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
                    JdAccessBridgeMember {
                        name,
                        param_tys,
                        parameter_names: local_variable_names,
                        ret: physical_ret,
                        varargs,
                    },
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
            // the same selection its `@NotNull` parameter annotations use. The continuation is the
            // compiler's own argument and is never checked.
            let guards: Vec<Option<String>> = if opts.param_assertions {
                semantic_params
                    .iter()
                    .zip(identities)
                    .enumerate()
                    .map(|(index, (parameter, identity))| {
                        (jd_nullability_annotation(parameter)
                            == Some("Lorg/jetbrains/annotations/NotNull;")
                            && !matches!(
                                identity,
                                crate::fir::ResolvedParameterIdentity::SuspendCompletion
                            ))
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
                HolderForward {
                    interface: c.fq_name,
                    member_name: name,
                    param_tys,
                    semantic_params,
                    local_variable_names,
                    reflected,
                    guards: &guards,
                    ret: physical_ret,
                    semantic_ret,
                    // The promoted generic signature of an inherited member is not reconstructed
                    // here (the declaring classifier's formals are not this interface's); a generic
                    // inherited surface keeps descriptor-only shape. Recorded in docs/SPEC.md.
                    signature: None,
                    // kotlinc gives an inherited member's holder forwarder no line: it has no
                    // source in this interface.
                    decl_line: 0,
                    varargs,
                    target,
                },
            );
        };
        for member in &shape.surface {
            let types = crate::jvm::value_classes::forwarded_member_types(ir, member, shape.source);
            let name = backend_member_jvm_name(ir, member);
            let mut param_tys = jvm_tys(&types.physical_params);
            let mut physical_ret = jvm_declared_ty(&types.physical_ret);
            let mut semantic_params = types.semantic_params;
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
            let mut semantic_ret = types.semantic_ret;
            assert_eq!(
                local_variable_names.len(),
                semantic_params.len(),
                "a republished member needs exact metadata parameter identities"
            );
            let mut identities = member.parameter_identities.to_vec();
            for names in [
                &mut local_variable_names,
                &mut method_parameter_names,
                &mut assertion_names,
            ] {
                crate::jvm::parameter_names::resolved_on_holder(&identities, names);
            }
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
                identities.push(crate::fir::ResolvedParameterIdentity::SuspendCompletion);
            }
            // The nearest declaration wins even when it is abstract.
            if !selected.insert(method_key(&name, &param_tys)) {
                continue;
            }
            if member.is_abstract() || member.visibility == crate::types::Visibility::Private {
                continue;
            }
            let reflected = if opts.java_parameters {
                crate::jvm::method_parameters::resolved_holder_forward(
                    &identities,
                    &method_parameter_names,
                    &param_tys,
                )
            } else {
                Vec::new()
            };
            // A suspend member's trailing continuation, not its vararg array, is the last physical
            // parameter, so only a non-suspend vararg member keeps ACC_VARARGS on this surface.
            let varargs = if member.vararg && !member.suspend() {
                crate::jvm::classfile::ACC_VARARGS
            } else {
                0
            };
            surface(
                &name,
                &param_tys,
                &semantic_params,
                &local_variable_names,
                &reflected,
                &assertion_names,
                &identities,
                physical_ret,
                semantic_ret,
                varargs,
                member.realization,
                member.nonvirtual.as_deref(),
                cw,
                default_impls,
            );
        }
    }
}

/// Emit the `$DefaultImpls` forward kotlinc keeps under `enable` for the interface's own bodied
/// member `fid`: a receiver-first static calling the interface's `access$<name>$jd` bridge. Like
/// every static that takes the interface instance first, it names an extension receiver
/// `$receiver`.
pub(super) fn emit_own_member_forward(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    fid: u32,
    di: &mut ClassWriter,
    signature_formatter: &JvmSignatureFormatter<'_>,
    opts: &EmitOptions,
    env: &EmitEnv,
) {
    let f = &ir.functions[fid as usize];
    let member_desc = declared_method_desc(ir, env.override_results, fid);
    let signature = default_impls::holder_method_signature(
        signature_formatter,
        ir,
        c.fq_name,
        declared_method_signature(signature_formatter, ir, env.override_results, fid).as_deref(),
        &member_desc,
    );
    // Annotation selection reads the SEMANTIC member types when recorded — `f.ret` is
    // already erased, and an erased `T` return must not read as `@NotNull Object`.
    let (semantic_params, semantic_ret) = match ir.member_semantic_sigs.get(&fid) {
        Some((params, ret)) => (params.clone(), *ret),
        None => (jd_declared_param_tys(ir, fid), f.ret),
    };
    let physical_params = jvm_function_params(ir, fid);
    let placed = |names| crate::jvm::parameter_names::placed(ir, fid, Some(c.fq_name), names);
    let assertion_names = placed(crate::jvm::parameter_names::function_assertions(
        ir,
        fid,
        &physical_params,
    ))
    .expect("a compatibility declaration carries exact assertion identities");
    let guards = if opts.param_assertions {
        f.param_checks
            .iter()
            .enumerate()
            .map(|(index, check)| {
                check.as_ref().map(|_| {
                    let _identity = ir
                        .function_parameter_identities(fid)
                        .and_then(|identities| identities.get(index))
                        .expect("a checked compatibility parameter has an identity");
                    assertion_names[index]
                        .clone()
                        .expect("a checked compatibility parameter has a JVM label")
                })
            })
            .collect()
    } else {
        Vec::new()
    };
    let parameter_names = placed(crate::jvm::parameter_names::function_locals(
        ir,
        fid,
        &physical_params,
    ))
    .expect("a compatibility declaration carries exact parameter identities");
    let reflected = if opts.java_parameters {
        crate::jvm::method_parameters::function(ir, fid, &physical_params, Some(c.fq_name))
    } else {
        Vec::new()
    };
    emit_holder_forward(
        di,
        HolderForward {
            interface: c.fq_name,
            member_name: &f.name,
            param_tys: &physical_params,
            semantic_params: &semantic_params,
            local_variable_names: &parameter_names,
            reflected: &reflected,
            guards: &guards,
            ret: jvm_declared_ty(&env.override_results.physical_result(ir, fid)),
            semantic_ret,
            signature: signature.as_deref(),
            // A property accessor has no `fn_decl_lines` entry — its line lives on the
            // property declaration it realizes.
            decl_line: ir.fn_decl_lines.get(&fid).copied().unwrap_or_else(|| {
                c.properties
                    .iter()
                    .find(|property| {
                        let (getter, setter) = accessor_jvm_names(c, &property.name);
                        getter == f.name || setter == f.name
                    })
                    .map(|property| property.decl_line)
                    .unwrap_or(0)
            }),
            varargs: method_access::varargs_access(ir, fid),
            target: JdHolderTarget::AccessBridge,
        },
    );
}

/// One receiver-first `$DefaultImpls` forward: the member it stands for, as the interface spells
/// it, and where it sends the call.
struct HolderForward<'a> {
    interface: crate::types::TypeName,
    member_name: &'a str,
    param_tys: &'a [Ty],
    semantic_params: &'a [Ty],
    local_variable_names: &'a [Option<String>],
    /// The forward's `MethodParameters`, its `$this` included; empty without `-java-parameters`.
    reflected: &'a [crate::jvm::method_parameters::MethodParameter],
    guards: &'a [Option<String>],
    ret: Ty,
    semantic_ret: Ty,
    signature: Option<&'a str>,
    decl_line: u32,
    /// The member's own `ACC_VARARGS` when its last physical parameter is the declared vararg.
    varargs: u16,
    target: JdHolderTarget<'a>,
}

/// Emit one receiver-first `$DefaultImpls` forward to an interface bridge or dependency holder.
fn emit_holder_forward(cw: &mut ClassWriter, forward: HolderForward<'_>) {
    let HolderForward {
        interface,
        member_name,
        param_tys,
        semantic_params,
        local_variable_names,
        reflected,
        guards,
        ret,
        semantic_ret,
        signature,
        decl_line,
        varargs,
        target,
    } = forward;
    assert_eq!(
        local_variable_names.len(),
        param_tys.len(),
        "a compatibility holder needs exact declaration parameter identities"
    );
    assert!(
        reflected.is_empty() || reflected.len() == param_tys.len() + 1,
        "a compatibility holder reflects its receiver and every declaration parameter"
    );
    let fq = interface.render();
    let mut with_receiver = vec![Ty::obj_name(interface)];
    with_receiver.extend_from_slice(param_tys);
    let desc = method_descriptor(&with_receiver, ret);
    cw.seed_utf8(member_name);
    cw.seed_utf8(&desc);
    if let Some(signature) = signature {
        cw.seed_utf8(signature);
    }
    for (parameter, _) in reflected {
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
    cw.add_method_sig(0x0009 | varargs, member_name, &desc, &code, signature);
    cw.set_method_parameters(member_name, &desc, reflected);

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
