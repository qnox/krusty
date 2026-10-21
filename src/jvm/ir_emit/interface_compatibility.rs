//! JVM-default compatibility-holder forwarding.

use super::*;

/// Under `enable`, an interface REPUBLISHES the compatibility surface for every non-abstract,
/// non-private member it inherits and does not redeclare (override resolution selects them as the
/// interface's inherited defaults): kotlinc gives even an empty `interface B : A` its own
/// `access$f$jd` bridge (an `invokespecial` through B itself, which the JVM resolves to the
/// inherited default) and a `B$DefaultImpls` holder forwarding to that bridge. A member inherited
/// from a `disable`-compiled dependency has no default method to bridge to — its holder entry
/// forwards straight to the dependency's own holder (behind a `checkcast`), with no
/// `access$…$jd` bridge and no `@Deprecated`, exactly as measured on kotlinc 2.4.10. Under
/// `disable` an interface republishes a member of this module's ancestor the same way, forwarding
/// to the ancestor's holder (`Left$DefaultImpls.f` → `Base$DefaultImpls.f`, measured on 2.4.20).
pub(super) fn emit_inherited_default_surface(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    cw: &mut ClassWriter,
    default_impls: &mut Option<ClassWriter>,
    callables: &crate::backend::CheckedBackendCallables,
    opts: &EmitOptions,
) {
    let fq_name = c.fq_name();
    let disable = opts.jvm_default == JvmDefaultMode::Disable;
    for default in ir.inherited_defaults.get(&c.fq_name).into_iter().flatten() {
        let interface = default.declaring_interface;
        let types = match &default.body {
            // A Java default method never joins the Kotlin compatibility surface, and a dependency's
            // default method has no holder to republish under `disable`.
            crate::fir::InheritedDefaultBody::JavaDefaultMethod => continue,
            crate::fir::InheritedDefaultBody::DependencyInterfaceMethod(_) if disable => continue,
            _ => crate::jvm::value_classes::forwarded_member_types(ir, callables, default),
        };
        let name = crate::jvm::inherited_default_realization::inherited_member_jvm_name(
            ir, callables, default,
        );
        let mut param_tys = jvm_tys(&types.physical_params);
        let mut physical_ret = jvm_declared_ty(&types.physical_ret);
        let mut semantic_params = types.semantic_params;
        let mut local_variable_names = crate::jvm::parameter_names::resolved_local_variables(
            &default.parameter_identities,
            &semantic_params,
            &name,
        );
        let mut method_parameter_names = crate::jvm::parameter_names::resolved_method_parameters(
            &default.parameter_identities,
            &semantic_params,
            &name,
        );
        let mut assertion_names = crate::jvm::parameter_names::resolved_assertions(
            &default.parameter_identities,
            &semantic_params,
            &name,
        );
        let mut semantic_ret = types.semantic_ret;
        assert_eq!(
            local_variable_names.len(),
            semantic_params.len(),
            "a republished member needs exact metadata parameter identities"
        );
        let mut identities = default.parameter_identities.to_vec();
        for names in [
            &mut local_variable_names,
            &mut method_parameter_names,
            &mut assertion_names,
        ] {
            crate::jvm::parameter_names::resolved_on_holder(&identities, names);
        }
        // A `suspend` member republishes in its CPS shape — trailing `Continuation`
        // (`$completion`, `@NotNull`) and a `@Nullable Object` return — like the implementing-class
        // forwarders.
        if default.suspend {
            param_tys.push(Ty::obj("kotlin/coroutines/Continuation"));
            physical_ret = Ty::obj("java/lang/Object");
            local_variable_names.push(Some("$completion".to_string()));
            method_parameter_names.push(Some("$completion".to_string()));
            assertion_names.push(Some("$completion".to_string()));
            semantic_params.push(Ty::obj("kotlin/coroutines/Continuation"));
            semantic_ret = Ty::nullable(Ty::obj("java/lang/Object"));
            identities.push(crate::fir::ResolvedParameterIdentity::SuspendCompletion);
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
        let module_holder_descriptor;
        let target = match &default.body {
            // The dependency's `disable` realization: its holder static is the only body.
            crate::fir::InheritedDefaultBody::DependencyHolder(declaration) => {
                let holder = callables
                    .callable(*declaration)
                    .and_then(|callable| callable.nonvirtual_realization.as_deref())
                    .expect("a dependency holder default has frozen nonvirtual facts");
                JdHolderTarget::DependencyHolder {
                    declaring: interface,
                    holder: holder.owner,
                    descriptor: &holder.descriptor,
                }
            }
            // Under `disable` this module's ancestor keeps the body on its own holder.
            crate::fir::InheritedDefaultBody::Module if disable => {
                let mut with_receiver = vec![Ty::obj_name(interface)];
                with_receiver.extend_from_slice(&param_tys);
                module_holder_descriptor = method_descriptor(&with_receiver, physical_ret);
                JdHolderTarget::DependencyHolder {
                    declaring: interface,
                    holder: crate::types::type_name_nested_child(interface, "DefaultImpls"),
                    descriptor: &module_holder_descriptor,
                }
            }
            // A Kotlin default method somewhere above: bridge through this interface itself.
            _ => JdHolderTarget::AccessBridge,
        };
        // A suspend member's trailing continuation, not its vararg array, is the last physical
        // parameter, so only a non-suspend vararg member keeps ACC_VARARGS on this surface.
        let varargs = if default.vararg && !default.suspend {
            crate::jvm::classfile::ACC_VARARGS
        } else {
            0
        };
        if matches!(target, JdHolderTarget::AccessBridge) {
            access_bridges::emit_jd_access_bridge(
                cw,
                c.fq_name,
                super::static_accessors::class_declaration_line(c),
                access_bridges::JdAccessBridgeMember {
                    name: &name,
                    param_tys: &param_tys,
                    parameter_names: &local_variable_names,
                    ret: physical_ret,
                    varargs,
                    suspend: default.suspend,
                },
            );
        }
        let di = default_impls.get_or_insert_with(|| {
            let mut w = new_writer(&format!("{fq_name}$DefaultImpls"), "java/lang/Object", opts);
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
        // The guard set of an inherited member follows its semantic parameter nullability — the
        // same selection its `@NotNull` parameter annotations use. The continuation is the
        // compiler's own argument and is never checked.
        let guards: Vec<Option<String>> = if opts.param_assertions {
            semantic_params
                .iter()
                .zip(&identities)
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
                member_name: &name,
                param_tys: &param_tys,
                semantic_params: &semantic_params,
                local_variable_names: &local_variable_names,
                reflected: &reflected,
                guards: &guards,
                ret: physical_ret,
                semantic_ret,
                // The promoted generic signature of an inherited member is not reconstructed here
                // (the declaring classifier's formals are not this interface's); a generic
                // inherited surface keeps descriptor-only shape. Recorded in docs/SPEC.md.
                signature: None,
                // kotlinc gives an inherited member's holder forwarder no line: it has no source in
                // this interface.
                lines: ForwardLines::default(),
                varargs,
                target,
                // An inherited member is not declared here; its holder copies none of its source.
                annotations: ForwardAnnotations::default(),
            },
        );
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
            lines: ForwardLines {
                // The forward spans the declaration it stands for: kotlinc calls on the
                // declaration's start line, then returns on its end line. A function records its
                // signature line; an accessor's header line is its declaration line. A property
                // accessor has no `fn_decl_lines` entry — its line lives on the property
                // declaration it realizes.
                start: ir
                    .fn_sig_lines
                    .get(&fid)
                    .or_else(|| ir.fn_decl_lines.get(&fid))
                    .copied()
                    .unwrap_or_else(|| {
                        c.properties
                            .iter()
                            .find(|property| {
                                let (getter, setter) = accessor_jvm_names(c, &property.name);
                                getter == f.name || setter == f.name
                            })
                            .map(|property| property.decl_line)
                            .unwrap_or(0)
                    }),
                end: ir.fn_end_lines.get(&fid).copied().unwrap_or(0),
            },
            varargs: method_access::varargs_access(ir, fid),
            target: JdHolderTarget::AccessBridge,
            annotations: ForwardAnnotations {
                declared: ir.function_annotations.get(&fid),
                parameters: ir.fn_param_annotations.get(&fid).map_or(&[], Vec::as_slice),
            },
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
    lines: ForwardLines,
    /// The member's own `ACC_VARARGS` when its last physical parameter is the declared vararg.
    varargs: u16,
    target: JdHolderTarget<'a>,
    annotations: ForwardAnnotations<'a>,
}

/// The source annotations a forward copies from the member it stands for: kotlinc gives an own
/// member's `$DefaultImpls` forward the member's declared and parameter annotations.
#[derive(Clone, Copy, Default)]
struct ForwardAnnotations<'a> {
    declared: Option<&'a crate::ir::DeclarationAnnotations>,
    /// One entry per declared parameter, without the holder's receiver.
    parameters: &'a [crate::ir::DeclarationAnnotations],
}

/// The source lines of the declaration a forward stands for; 0 when it has none.
#[derive(Clone, Copy, Default)]
struct ForwardLines {
    start: u32,
    end: u32,
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
        lines,
        varargs,
        target,
        annotations,
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
    let declared = annotations.declared.cloned().unwrap_or_default();
    let retained_where = |visible: bool| {
        crate::ir::DeclarationAnnotations::new(
            declared
                .iter()
                .filter(|annotation| {
                    (annotation.retention != crate::types::AnnotationRetention::Binary) == visible
                })
                .cloned()
                .collect(),
        )
    };
    // kotlinc writes the member's RUNTIME annotations, then the forward's `@java.lang.Deprecated`,
    // then the member's BINARY ones, before the compiler's nullability types.
    cw.reserve_method_pool_with_annotations(
        member_name,
        &desc,
        signature,
        &[],
        &retained_where(true),
        reflected,
    );
    let deprecated = matches!(target, JdHolderTarget::AccessBridge);
    if deprecated {
        cw.seed_utf8("Ljava/lang/Deprecated;");
    }
    let ret_ann = jd_nullability_annotation(&semantic_ret);
    let mut parameter_annotations = vec![Some("Lorg/jetbrains/annotations/NotNull;")];
    parameter_annotations.extend(semantic_params.iter().map(jd_nullability_annotation));
    let mut user_parameter_annotations = vec![crate::ir::DeclarationAnnotations::default()];
    user_parameter_annotations.extend(annotations.parameters.iter().cloned());
    let annotation_types = function_annotations::header_annotation_types(
        ret_ann,
        &user_parameter_annotations,
        &parameter_annotations,
        true,
    );
    let annotation_types: Vec<&str> = annotation_types.iter().map(String::as_str).collect();
    cw.reserve_method_pool_with_annotations(
        member_name,
        &desc,
        signature,
        &annotation_types,
        &retained_where(false),
        &[],
    );

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
    if lines.start != 0 {
        code.mark_line(lines.start);
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
    if lines.end != 0 {
        code.mark_line(lines.end);
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
    cw.set_method_annotations(member_name, &desc, &declared);
    cw.set_method_param_annotations(member_name, &desc, &user_parameter_annotations);
    if deprecated {
        cw.mark_method_deprecated(member_name, &desc);
        cw.add_method_visible_marker_annotation(member_name, &desc, "Ljava/lang/Deprecated;");
    }
    if ret_ann.is_some() || parameter_annotations.iter().any(Option::is_some) {
        cw.set_method_nullability(member_name, &desc, ret_ann, &parameter_annotations);
    }
}

/// `@NotNull`/`@Nullable` for one semantic type in the `-jvm-default` compatibility surface, the
/// same selection the implementing-class forwarders use: reference types only, and never a bare
/// type variable (kotlinc leaves `T`-typed positions unannotated).
fn jd_nullability_annotation(ty: &Ty) -> Option<&'static str> {
    if ty.non_null().is_ty_param() || !ir_ty_to_jvm(ty).is_reference() {
        None
    } else if ty.is_nullable() {
        Some("Lorg/jetbrains/annotations/Nullable;")
    } else {
        Some("Lorg/jetbrains/annotations/NotNull;")
    }
}

/// A declared member's parameter types with the side-table declared nullability applied, so the
/// compatibility surface annotates `x: T?` parameters `@Nullable` exactly as the abstract
/// declaration path does.
fn jd_declared_param_tys(ir: &IrFile, fid: u32) -> Vec<Ty> {
    let f = &ir.functions[fid as usize];
    let declared_nullable = ir.fn_param_declared_nullable.get(&fid);
    f.params
        .iter()
        .enumerate()
        .map(|(i, t)| {
            if declared_nullable
                .and_then(|v| v.get(i))
                .copied()
                .unwrap_or(false)
            {
                Ty::nullable(*t)
            } else {
                *t
            }
        })
        .collect()
}

/// Where an `enable`-mode holder forward sends its call.
enum JdHolderTarget<'a> {
    /// The interface's own `access$<name>$jd` bridge (the member is a default method here).
    AccessBridge,
    /// The `$DefaultImpls` static holding the inherited body: a `disable`-compiled dependency's,
    /// or under `disable` an ancestor's of this module. There is no default method to bridge to.
    DependencyHolder {
        declaring: crate::types::TypeName,
        holder: crate::types::TypeName,
        descriptor: &'a str,
    },
}
