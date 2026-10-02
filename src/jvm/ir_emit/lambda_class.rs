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
    // The class's generic header: `Object`, then the function type it implements.
    let formatter = JvmSignatureFormatter::new(ir, env);
    let signature = formatter
        .ty_at(&lambda.function_type, Wildcards::Suppressed)
        .map(|function| format!("L{OBJECT};{function}"));
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
        .map(|field| Capture {
            name: field.name.as_str(),
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
        let reference = cw.fieldref(&class, capture.name, &capture.descriptor);
        code.putfield(reference, slot_words(capture.ty) as i32 + 1);
        slot += slot_words(capture.ty);
    }
    code.aload(0);
    let object = cw.methodref(OBJECT, "<init>", "()V");
    code.invokespecial(object, 0, 0);
    code.ret_void();
    // kotlinc names the captured `this` parameter `$receiver`, every other one after its field.
    let parameters = captures
        .iter()
        .enumerate()
        .map(|(index, capture)| {
            let name = if lambda.receiver_captures.contains(&(index as u32)) {
                "$receiver"
            } else {
                capture.name
            };
            (name, capture.descriptor.as_str())
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
    for capture in &captures {
        cw.add_field_late_sig(
            0x1010, // FINAL | SYNTHETIC
            capture.name,
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
struct Capture<'a> {
    name: &'a str,
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
