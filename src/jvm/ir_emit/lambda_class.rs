//! The class kotlinc writes for a lambda `LambdaMetafactory` cannot build (see
//! `crate::jvm::lambda_classes`).
//!
//! `final class X extends Object implements FunctionN<…>`, in kotlinc's member order: the
//! constructor, which stores each captured value in its final synthetic field before `Object()`;
//! the lambda's body as the class's specialized `invoke`; the erased `FunctionN.invoke` bridge to
//! it; and, for a lambda that captures nothing, the `<clinit>` of its `INSTANCE`.

use super::*;

/// Write the class of the lambda `c` realizes.
pub(super) fn emit_lambda_class(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    lambda: &crate::ir::IrLambdaClass,
    facade: &str,
    env: &EmitEnv,
    opts: &EmitOptions,
) -> Vec<u8> {
    const OBJECT: &str = "java/lang/Object";
    let class = c.fq_name();
    let arity = u8::try_from(lambda.bridge.param_tys.len())
        .expect("a lambda class's arity fits a numbered FunctionN");
    // The class's generic header: `Object`, then the function type it implements — except when
    // kotlinc's `hasNothingInNonContravariantPosition` leaves that supertype raw (no `Signature`).
    let formatter = JvmSignatureFormatter::new(ir, env);
    let signature = if lambda.raw_supertype {
        None
    } else {
        formatter
            .ty_at(&lambda.function_type, Wildcards::Suppressed)
            .map(|function| format!("L{OBJECT};{function}"))
    };
    let mut cw = new_writer_generic(&class, signature.as_deref(), OBJECT, opts);
    if let Some(signature) = &signature {
        cw.set_signature(signature);
    }
    cw.set_access(0x0010 | 0x0020 | u16::from(lambda.public_inline)); // FINAL | SUPER | publication
    if let Some((owner, method)) = class_enclosure(ir, env.override_results, c, facade) {
        match method {
            Some((name, descriptor)) => cw.set_enclosing_method(&owner, &name, &descriptor),
            None => cw.set_enclosing_class(&owner),
        }
    }
    env.inner_classes.register(&mut cw);
    cw.add_interface(&jvm_function_interface(arity));
    delegated_property_array::declare(env, c.fq_name, &mut cw);

    // Each captured value's field, typed by the value's declared type: a shared cell is a generic
    // `Ref$ObjectRef<T>`, whose `Signature` the field and the constructor carry.
    let captures = c
        .fields
        .iter()
        .enumerate()
        .map(|(index, field)| Capture {
            names: crate::jvm::capture_names::lambda_class_capture(ir, lambda, index)
                .expect("every field of a lambda class stores one of its captures"),
            ty: jvm_declared_ty(&field.ty),
            descriptor: ir_type_desc(&field.ty),
            signature: parameterized_sig(&formatter, &field.ty),
            parameter_signature: parameterized_sig_at(&formatter, &field.ty, Wildcards::Declared),
        })
        .collect::<Vec<_>>();
    let constructor = format!(
        "({})V",
        captures
            .iter()
            .map(|capture| capture.descriptor.as_str())
            .collect::<String>()
    );
    let constructor_signature = captures
        .iter()
        .any(|capture| capture.parameter_signature.is_some())
        .then(|| {
            let parameters = captures
                .iter()
                .map(|capture| {
                    capture
                        .parameter_signature
                        .as_deref()
                        .unwrap_or(&capture.descriptor)
                })
                .collect::<String>();
            format!("({parameters})V")
        });
    cw.seed_utf8("<init>");
    cw.seed_utf8(&constructor);
    if let Some(signature) = &constructor_signature {
        cw.seed_utf8(signature);
    }
    let words = 1 + captures
        .iter()
        .map(|capture| slot_words(capture.ty))
        .sum::<u16>();
    let mut code = CodeBuilder::new(words);
    let mut slot = 1u16;
    for capture in &captures {
        code.aload(0);
        load(capture.ty, slot, &mut code);
        let reference = cw.fieldref(&class, &capture.names.field, &capture.descriptor);
        code.putfield(reference, slot_words(capture.ty) as i32 + 1);
        slot += slot_words(capture.ty);
    }
    code.aload(0);
    let object = cw.methodref(OBJECT, "<init>", "()V");
    code.invokespecial(object, 0, 0);
    code.ret_void();
    let parameters = captures
        .iter()
        .map(|capture| {
            (
                capture.names.parameter.as_str(),
                capture.descriptor.as_str(),
            )
        })
        .collect::<Vec<_>>();
    let locals =
        function_reference_invoke::reference_constructor_locals(&mut cw, &class, &parameters);
    if lambda.public_inline {
        finish_code_sig::<0x0001>(
            &mut cw,
            "<init>",
            &constructor,
            &mut code,
            words,
            constructor_signature.as_deref(),
        );
    } else {
        finish_code_sig::<0x0000>(
            &mut cw,
            "<init>",
            &constructor,
            &mut code,
            words,
            constructor_signature.as_deref(),
        );
    }
    cw.set_method_debug("<init>", &constructor, None, &locals);
    if env.java_parameters {
        // `-java-parameters` reflects each capture under its field's name, compiler-generated.
        let reflected = captures
            .iter()
            .map(|capture| (Some(capture.names.field.clone()), 0x1000))
            .collect::<Vec<_>>();
        cw.set_method_parameters("<init>", &constructor, &reflected);
    }
    for capture in &captures {
        cw.add_field_late_sig(
            0x1010, // FINAL | SYNTHETIC
            &capture.names.field,
            &capture.descriptor,
            capture.signature.as_deref(),
            None,
            None,
        );
    }

    emit_method(
        ir,
        lambda.invoke,
        StaticOwner::Class(c.fq_name),
        &class,
        facade,
        &mut cw,
        true,
        env,
    );
    for &method in &c.methods {
        if method != lambda.invoke {
            emit_method(
                ir,
                method,
                StaticOwner::Class(c.fq_name),
                &class,
                facade,
                &mut cw,
                false,
                env,
            );
        }
    }
    function_reference_invoke::emit_reference_invoke_bridge(
        ir,
        &mut cw,
        &class,
        &lambda.bridge,
        false,
        lambda.invoke,
        arity,
    );
    if captures.is_empty() || delegated_property_array::exists(env, c.fq_name) {
        emit_initialization(ir, c, &class, facade, env, &mut cw, captures.is_empty());
    }
    if captures.is_empty() {
        add_singleton_instance_field(&mut cw, &class);
    }
    if lambda.public_inline {
        cw.set_kotlin_metadata(
            3,
            &[2, 4, 0],
            super::metadata_policy::synthetic_class_xi(super::metadata_policy::SYNTHETIC_PUBLIC),
            &[],
            &[],
        );
        env.run.finish_class(cw)
    } else {
        finish_local_synthetic_class(cw, env)
    }
}

fn emit_initialization(
    ir: &IrFile,
    class: &IrClass,
    internal: &str,
    facade: &str,
    env: &EmitEnv<'_>,
    writer: &mut ClassWriter,
    singleton: bool,
) {
    writer.seed_utf8("<clinit>");
    writer.seed_utf8("()V");
    let mut emitter = Emitter::new(
        ir,
        writer,
        env,
        Some(StaticOwner::Class(class.fq_name)),
        internal,
        facade,
        Ty::Unit,
        std::iter::empty(),
    );
    let mut code = CodeBuilder::new(0);
    emitter.emit_delegated_property_array(env, class.fq_name, internal, &mut code);
    if singleton {
        // Retained typed operations still require class reification. A concrete call-site copy
        // contains ordinary specialized operations and therefore has no declaration marker.
        if class.methods.iter().any(|&method| {
            ir.functions[method as usize]
                .body
                .is_some_and(|body| body_has_reified_markers(ir, body))
        }) {
            let marker = emitter.cw.methodref(
                "kotlin/jvm/internal/Intrinsics",
                "needClassReification",
                "()V",
            );
            code.invokestatic(marker, 0, 0);
        }
        let classifier = emitter.cw.class_ref(internal);
        let constructor = emitter.cw.methodref(internal, "<init>", "()V");
        let field = emitter
            .cw
            .fieldref(internal, "INSTANCE", &format!("L{internal};"));
        code.new_obj(classifier);
        code.dup();
        code.invokespecial(constructor, 0, 0);
        code.putstatic(field, 1);
    }
    code.ret_void();
    let locals = emitter.frame.max();
    finish_code::<0x0008>(emitter.cw, "<clinit>", "()V", &mut code, locals);
}

/// A captured value's field, as the constructor and `invoke` spell it.
struct Capture {
    names: crate::jvm::capture_names::CaptureNames,
    ty: Ty,
    descriptor: String,
    signature: Option<String>,
    parameter_signature: Option<String>,
}

/// The error for a lambda `LambdaMetafactory` cannot adapt that reaches emission as an
/// `invokedynamic`: the lambda-class pass did not realize its shape, and the indy would fail to link.
pub(super) fn unrealized(ir: &IrFile, lambda: u32) -> String {
    format!(
        "internal error: lambda {} needs the class kotlinc writes when LambdaMetafactory cannot \
         adapt its signature, and its shape is not realized as one yet",
        ir.functions[lambda as usize].name
    )
}

/// JVM descriptors passed to `LambdaMetafactory` for one selected SAM conversion.
pub(super) struct SamBootstrapDescriptors {
    pub(super) erased_method: String,
    pub(super) instantiated_method: String,
}

/// Build the erased SAM and instantiated method descriptors once for class-mode selection and
/// indy emission. Keeping this at the lambda-class boundary prevents a representation decision
/// from drifting away from the actual bootstrap arguments.
pub(super) fn sam_bootstrap_descriptors(
    ir: &IrFile,
    impl_fn: u32,
    target: &crate::ir::IrSamTarget,
    captures: usize,
) -> Result<SamBootstrapDescriptors, &'static str> {
    let impl_params = super::declaration_types::jvm_function_params(ir, impl_fn);
    let lam_tys = impl_params
        .get(captures..)
        .ok_or("lambda implementation has fewer parameters than captured values")?;
    let impl_ret =
        crate::jvm::method_descriptors::jvm_declared_ty(&ir.functions[impl_fn as usize].ret);
    let (sam_parameters, sam_result) = ir
        .lambda_sam_jvm_signature
        .get(&impl_fn)
        .map(|(parameters, result)| (parameters.as_slice(), *result))
        .unwrap_or((
            target.declared_parameters.as_slice(),
            target.declared_result,
        ));
    let mut sam_parameters = crate::jvm::method_descriptors::jvm_tys(sam_parameters);
    let sam_result = if target.suspend {
        sam_parameters.push(Ty::obj("kotlin/coroutines/Continuation"));
        Ty::obj("java/lang/Object")
    } else if target.overrides_non_primitive_result {
        crate::jvm::method_descriptors::jvm_declared_ty(&Ty::nullable(sam_result))
    } else {
        crate::jvm::method_descriptors::jvm_declared_ty(&sam_result)
    };
    let erased_method = crate::jvm::names::method_descriptor(&sam_parameters, sam_result);
    let (sam_params, sam_ret) = crate::jvm::names::parse_method_descriptor(&erased_method)
        .ok_or("selected SAM descriptor is malformed")?;
    if sam_params.len() != lam_tys.len() {
        return Err("selected SAM descriptor has the wrong parameter count");
    }
    let params: String = lam_tys
        .iter()
        .zip(sam_params)
        .map(|(&logical, physical)| {
            if super::descriptor_is_reference(physical) {
                super::boxed_descriptor(logical)
            } else {
                crate::jvm::names::type_descriptor(logical)
            }
        })
        .collect();
    let ret = if sam_ret == "V" {
        "V".to_string()
    } else if super::descriptor_is_reference(sam_ret) {
        super::boxed_descriptor(impl_ret)
    } else {
        crate::jvm::names::type_descriptor(impl_ret)
    };
    Ok(SamBootstrapDescriptors {
        erased_method,
        instantiated_method: format!("({params}){ret}"),
    })
}

/// Whether the instantiated descriptor has the one mismatch this class strategy repairs: an
/// `Object` slot against a more specific erased reference bound. Other impossible adaptations are
/// frontend errors and must not silently select a backend fallback.
fn has_bounded_erasure_mismatch(sam: &str, instantiated: &str) -> bool {
    let Some((sam_params, sam_ret)) = crate::jvm::names::parse_method_descriptor(sam) else {
        return false;
    };
    let Some((inst_params, inst_ret)) = crate::jvm::names::parse_method_descriptor(instantiated)
    else {
        return false;
    };
    sam_params.len() == inst_params.len()
        && (sam_params
            .iter()
            .zip(inst_params)
            .any(|(slot, specialized)| erased_top_against_bound(specialized, slot))
            || erased_top_against_bound(inst_ret, sam_ret))
}

/// `Object`/`Any` is not a subtype of a more specific reference. That is the mismatch
/// `LambdaMetafactory` reports as "class java.lang.Object is not a subtype of …".
fn erased_top_against_bound(specialized: &str, sam_slot: &str) -> bool {
    is_erased_top(specialized) && descriptor_is_reference(sam_slot) && !is_erased_top(sam_slot)
}

fn is_erased_top(descriptor: &str) -> bool {
    descriptor == "Ljava/lang/Object;"
}

/// Whether this SAM conversion's instantiated signature is not a subtype of the erased interface
/// method, so the closure must be a class (and its implementation must be visible to that class).
pub(super) fn bounded_erasure_needs_class(
    ir: &IrFile,
    impl_fn: u32,
    target: &crate::ir::IrSamTarget,
    captures: usize,
) -> bool {
    sam_bootstrap_descriptors(ir, impl_fn, target, captures).is_ok_and(|descriptors| {
        has_bounded_erasure_mismatch(&descriptors.erased_method, &descriptors.instantiated_method)
    })
}

#[cfg(test)]
mod specialization_tests {
    use super::has_bounded_erasure_mismatch;

    #[test]
    fn object_parameter_does_not_link_against_a_bounded_sam_slot() {
        assert!(has_bounded_erasure_mismatch(
            "(LTop;)V",
            "(Ljava/lang/Object;)V"
        ));
        assert!(has_bounded_erasure_mismatch(
            "(LTop;)LCommon;",
            "(Ljava/lang/Object;)Ljava/lang/Object;"
        ));
    }

    #[test]
    fn a_more_specific_parameter_still_links() {
        assert!(!has_bounded_erasure_mismatch(
            "(Ljava/lang/Object;Ljava/lang/Object;)I",
            "(Ljava/lang/Integer;Ljava/lang/Integer;)I"
        ));
        assert!(!has_bounded_erasure_mismatch("(LTop;)V", "(LTop;)V"));
        assert!(!has_bounded_erasure_mismatch(
            "(Ljava/lang/String;)V",
            "(Ljava/lang/String;)V"
        ));
    }
}
