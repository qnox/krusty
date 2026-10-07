//! The class kotlinc writes for a suspend lambda (`SuspendLambdaLowering`, then `ClassCodegen`).
//!
//! The lambda extends `SuspendLambda`, implements its `FunctionN` and is its own continuation.
//! Its members, in kotlinc's order: the constructor over the captured values and the completion,
//! `invokeSuspend` (the lambda's body, which kotlinc's coroutine transformer turns into the state
//! machine when the class is written), `create` for a lambda of at most one parameter, the typed
//! `invoke`, and the erased `FunctionN.invoke` bridge to it. A fresh copy of the lambda, built by
//! `create` or inline in `invoke`, keeps each parameter the body reads in its field.

use super::*;
use crate::jvm::suspend::cps::SuspendLambdaMember;
use crate::jvm::suspend::SuspendLambdaClass;

const SUSPEND_LAMBDA: &str = "kotlin/coroutines/jvm/internal/SuspendLambda";
const CONTINUATION: &str = "kotlin/coroutines/Continuation";
const CONTINUATION_DESC: &str = "Lkotlin/coroutines/Continuation;";
const OBJECT_DESC: &str = "Ljava/lang/Object;";
const INVOKE_SUSPEND_DESC: &str = "(Ljava/lang/Object;)Ljava/lang/Object;";

/// A field of the lambda class, as the members that read and write it spell it.
struct Field {
    name: String,
    descriptor: String,
    ty: Ty,
    private: bool,
}

/// One of the lambda's own parameters: its semantic and physical types, and its field when the
/// body reads it.
struct Parameter {
    semantic: Ty,
    physical: Ty,
    field: Option<Field>,
}

/// A captured value's field.
struct Capture {
    field: Field,
    /// The semantic captured type, retained independently from the field's physical descriptor so
    /// its generic `Signature` never has to be recovered from a parallel class-field table.
    semantic: Ty,
}

/// The facts every member of the class is written from.
struct Shape {
    class: String,
    captures: Vec<Capture>,
    parameters: Vec<Parameter>,
    /// The lambda's result, as its continuation's type argument.
    result: Ty,
    /// The `FunctionN` arity: the parameters and the continuation.
    arity: u8,
}

impl Shape {
    fn new(c: &crate::ir::IrClass, lambda: &SuspendLambdaClass) -> Shape {
        let field = |index: u32| {
            let field = &c.fields[index as usize];
            Field {
                name: field.name.clone(),
                descriptor: ir_type_desc(&field.ty),
                ty: jvm_value_ty(&field.ty),
                private: field.is_private(),
            }
        };
        let Ty::Fun(signature) = lambda.function_type.non_null() else {
            unreachable!("a suspend lambda has a function type")
        };
        let parameters = lambda
            .parameters
            .iter()
            .enumerate()
            .map(|(index, &(ty, stored))| {
                // A parameter never has a `void` type: a `Unit` one is the `Unit` object.
                let [physical] = jvm_tys(&[ty])[..] else {
                    unreachable!("one type maps to one JVM type")
                };
                let semantic = signature.params.get(index).copied().unwrap_or(ty);
                Parameter {
                    semantic: if semantic == Ty::Unit {
                        physical
                    } else {
                        semantic
                    },
                    physical,
                    field: stored.map(field),
                }
            })
            .collect::<Vec<_>>();
        Shape {
            class: c.fq_name(),
            captures: lambda
                .captures
                .iter()
                .map(|capture| Capture {
                    field: field(capture.field),
                    semantic: capture.ty,
                })
                .collect(),
            arity: u8::try_from(parameters.len() + 1)
                .expect("a suspend lambda's arity fits its FunctionN"),
            parameters,
            result: signature.ret,
        }
    }

    /// The physical parameters of `member`, each with its descriptor, in descriptor order.
    fn physical_parameters(&self, member: SuspendLambdaMember) -> Vec<(Ty, String)> {
        let object = || (Ty::obj("java/lang/Object"), OBJECT_DESC.to_string());
        let leading: Vec<(Ty, String)> = match member {
            SuspendLambdaMember::Constructor => self
                .captures
                .iter()
                .map(|capture| (capture.field.ty, capture.field.descriptor.clone()))
                .collect(),
            SuspendLambdaMember::Create => self
                .parameters
                .first()
                .map(|_| object())
                .into_iter()
                .collect(),
            SuspendLambdaMember::Invoke => self
                .parameters
                .iter()
                .map(|parameter| (parameter.physical, type_descriptor(parameter.physical)))
                .collect(),
        };
        leading
            .into_iter()
            .chain(std::iter::once((
                Ty::obj(CONTINUATION),
                CONTINUATION_DESC.to_string(),
            )))
            .collect()
    }

    /// `member`'s local-variable table rows for `this` and its parameters: each parameter named
    /// from the identities the lambda's realization recorded, never from its position.
    fn parameter_locals(
        &self,
        ir: &IrFile,
        lambda: &SuspendLambdaClass,
        member: SuspendLambdaMember,
    ) -> Vec<(String, String, u16)> {
        let physical = self.physical_parameters(member);
        let types = physical.iter().map(|(ty, _)| *ty).collect::<Vec<_>>();
        let names = crate::jvm::parameter_names::suspend_lambda_member(
            ir,
            &lambda.member_parameters,
            member,
            &types,
        );
        let mut locals = vec![("this".to_string(), self.self_desc(), 0u16)];
        let mut slot = 1u16;
        for (name, (ty, descriptor)) in names.into_iter().zip(physical) {
            locals.push((name, descriptor, slot));
            slot += slot_words(ty);
        }
        locals
    }

    /// `member`'s `MethodParameters`, planned from the same recorded identities as its locals but
    /// independently of them.
    fn method_parameters(
        &self,
        ir: &IrFile,
        lambda: &SuspendLambdaClass,
        member: SuspendLambdaMember,
    ) -> Vec<(Option<String>, u16)> {
        let types = self
            .physical_parameters(member)
            .into_iter()
            .map(|(ty, _)| ty)
            .collect::<Vec<_>>();
        crate::jvm::method_parameters::suspend_lambda_member(
            ir,
            &lambda.member_parameters,
            member,
            &types,
        )
    }

    fn self_desc(&self) -> String {
        format!("L{};", self.class)
    }

    fn constructor_desc(&self) -> String {
        let captures: String = self
            .captures
            .iter()
            .map(|c| c.field.descriptor.as_str())
            .collect();
        format!("({captures}{CONTINUATION_DESC})V")
    }

    fn capture_words(&self) -> u16 {
        self.captures
            .iter()
            .map(|capture| slot_words(capture.field.ty))
            .sum()
    }

    fn has_create(&self) -> bool {
        self.parameters.len() <= 1
    }

    fn create_desc(&self) -> String {
        let value = if self.parameters.is_empty() {
            ""
        } else {
            OBJECT_DESC
        };
        format!("({value}{CONTINUATION_DESC}){CONTINUATION_DESC}")
    }

    fn invoke_desc(&self) -> String {
        let parameters: String = self
            .parameters
            .iter()
            .map(|parameter| type_descriptor(parameter.physical))
            .collect();
        format!("({parameters}{CONTINUATION_DESC}){OBJECT_DESC}")
    }
}

/// Write the class of the suspend lambda `c` realizes.
pub(super) fn emit_suspend_lambda_class(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    lambda: &SuspendLambdaClass,
    facade: &str,
    env: &EmitEnv,
    opts: &EmitOptions,
) -> Vec<u8> {
    let shape = Shape::new(c, lambda);
    let formatter = JvmSignatureFormatter::new(ir, env);
    let result = formatter
        .ty_at(&shape.result, Wildcards::Suppressed)
        .expect("a suspend lambda's result has a JVM signature");
    let class_signature = class_signature(&formatter, &shape, &result);
    let mut cw = new_writer_generic(&shape.class, Some(&class_signature), SUSPEND_LAMBDA, opts);
    cw.set_signature(&class_signature);
    // An inline or specialized suspend lambda is public: its body is copied into other packages.
    // A lambda outside an inline declaration stays package-private.
    let public = crate::jvm::inner_classes::suspend_lambda_is_public(ir, c);
    let visibility = if public { 0x0001 } else { 0 };
    cw.set_access(visibility | 0x0010 | 0x0020); // [PUBLIC |] FINAL | SUPER
    if let Some((owner, method)) = class_enclosure(ir, env.override_results, c, facade) {
        match method {
            Some((name, descriptor)) => cw.set_enclosing_method(&owner, &name, &descriptor),
            None => cw.set_enclosing_class(&owner),
        }
    }
    env.inner_classes.register(&mut cw);
    cw.add_interface(&jvm_function_interface(shape.arity));
    // kotlinc writes a class's fields after its methods, as the coroutine transformer adds the
    // spill fields ahead of them: `label`, the parameters' fields, then the captured values.
    cw.add_field_late(0x0000, "label", "I", None, None);
    for parameter in &shape.parameters {
        if let Some(field) = &parameter.field {
            let access = if field.private { 0x1002 } else { 0x1000 };
            cw.add_field_late(access, &field.name, &field.descriptor, None, None);
        }
    }
    // A captured value's field carries its type's generic `Signature`, as a shared cell's
    // `Ref$ObjectRef<T>` does.
    for Capture { field, semantic } in &shape.captures {
        let signature = parameterized_sig(&formatter, semantic);
        cw.add_field_late_sig(
            0x1010,
            &field.name,
            &field.descriptor,
            signature.as_deref(),
            None,
            None,
        );
    }

    emit_constructor(
        ir,
        &mut cw,
        &formatter,
        &shape,
        lambda,
        public,
        env.java_parameters,
    );
    emit_method(
        ir,
        lambda.invoke_suspend,
        StaticOwner::Class(c.fq_name),
        &shape.class,
        facade,
        &mut cw,
        true,
        env,
    );
    // A lambda value created inside this state machine may retain a standalone implementation.
    // Placement attaches that private static to this exact class so its metafactory handle has
    // legal access. Emit those attached helpers beside invokeSuspend; a continuation generated
    // while emitting one re-enters through the access bridge emitted on this same writer.
    for &method in &c.methods {
        if method != lambda.invoke_suspend {
            emit_method(
                ir,
                method,
                StaticOwner::Class(c.fq_name),
                &shape.class,
                facade,
                &mut cw,
                false,
                env,
            );
        }
    }
    cw.set_method_debug(
        "invokeSuspend",
        INVOKE_SUSPEND_DESC,
        None,
        &[
            ("this".to_string(), shape.self_desc(), 0),
            ("$result".to_string(), OBJECT_DESC.to_string(), 1),
        ],
    );
    if shape.has_create() {
        emit_create(ir, &mut cw, &shape, lambda, env.java_parameters);
    }
    emit_invoke(
        ir,
        &mut cw,
        &formatter,
        &shape,
        lambda,
        &result,
        env.java_parameters,
    );
    emit_bridge(ir, &mut cw, &shape, lambda);
    // A continuation may have been built before JVM lambda placement attached its private static
    // helper to this class. Its exact function identity now resolves through the physical-owner
    // table, and the cross-class re-entry therefore needs the same synthetic accessor ordinary
    // classifier emission writes on the declaration owner.
    static_accessors::emit(
        ir,
        &env.run.static_accessor_plan.borrow(),
        StaticOwner::Class(c.fq_name),
        facade,
        c.decl_start_line.max(c.decl_line),
        &mut cw,
    );
    // A lambda class is local to the scope it was written in.
    let (d1, d2) = match lambda_metadata(ir, lambda, &formatter) {
        Ok(metadata) => metadata,
        Err(error) => {
            env.run.set_emit_error(error);
            return Vec::new();
        }
    };
    cw.set_kotlin_metadata(
        3,
        &opts.metadata_version(),
        synthetic_class_xi(SYNTHETIC_LOCAL),
        &d1,
        &d2,
    );
    env.run.finish_class(cw)
}

/// The lambda's function, which kotlinc records in the class's `@Metadata` for reflection: its
/// receiver, value parameters and result, without its context parameters.
fn lambda_metadata(
    ir: &IrFile,
    lambda: &SuspendLambdaClass,
    formatter: &JvmSignatureFormatter<'_>,
) -> Result<(Vec<String>, Vec<String>), String> {
    let function_type = super::class_lambda_reflection::approximate_unpublished_type_parameters(
        lambda.function_type,
        &lambda.type_parameters,
    );
    let Ty::Fun(signature) = function_type.non_null() else {
        unreachable!("a suspend lambda has a function type")
    };
    let own = &signature.params[signature.context_count..];
    let (receiver, values) = match signature.has_receiver {
        true => (Some(own[0]), &own[1..]),
        false => (None, own),
    };
    assert_eq!(
        values.len(),
        lambda.metadata_names.len(),
        "a suspend lambda names each value parameter of its function type"
    );
    let parameters: Vec<(&str, Ty)> = lambda
        .metadata_names
        .iter()
        .map(String::as_str)
        .zip(values.iter().copied())
        .collect();
    let local_classifiers = crate::jvm::local_classifiers::names(ir);
    let enum_entry_bodies = crate::jvm::local_classifiers::enum_entry_bodies(ir);
    let approximate_intersection = |ty| formatter.declaration_approximation(ty);
    let (bytes, strings) = crate::metadata::lambda_function::build(
        &crate::metadata::lambda_function::LambdaFunction {
            function_name: crate::metadata::lambda_function::function_name(lambda.form),
            jvm_method: None,
            explicit_suspend: lambda.explicit_suspend,
            receiver,
            parameters: &parameters,
            result: signature.ret,
            type_parameters: &lambda.type_parameters,
            local_classifiers: &local_classifiers,
            enum_entry_bodies: &enum_entry_bodies,
            intersection_approximation: &approximate_intersection,
        },
    )?;
    Ok((crate::metadata::encoding::bytes_to_strings(&bytes), strings))
}

/// `SuspendLambda` and the lambda's `FunctionN` over its parameters, its continuation and `Object`.
fn class_signature(formatter: &JvmSignatureFormatter, shape: &Shape, result: &str) -> String {
    let mut signature = format!(
        "L{SUSPEND_LAMBDA};L{}<",
        jvm_function_interface(shape.arity)
    );
    for parameter in &shape.parameters {
        signature.push_str(
            &formatter
                .ty_at(&parameter.semantic, Wildcards::Suppressed)
                .expect("a suspend lambda's parameter has a JVM signature"),
        );
    }
    signature.push_str(&format!("L{CONTINUATION}<-{result}>;{OBJECT_DESC}>;"));
    signature
}

/// `<init>(captures…, Continuation)`: store each captured value, then call
/// `SuspendLambda(arity, completion)`. `-java-parameters` reflects each capture under its field's
/// name, compiler-generated, also where the local-variable table calls it `$receiver`.
fn emit_constructor(
    ir: &IrFile,
    cw: &mut ClassWriter,
    formatter: &JvmSignatureFormatter,
    shape: &Shape,
    lambda: &SuspendLambdaClass,
    public: bool,
    java_parameters: bool,
) {
    let descriptor = shape.constructor_desc();
    let mut signature = String::from("(");
    for capture in &lambda.captures {
        signature.push_str(
            &formatter
                .method_ty(&capture.ty, Wildcards::Declared)
                .expect("a captured value has a JVM signature"),
        );
    }
    signature.push_str(&format!("L{CONTINUATION}<-{}>;)V", shape.self_desc()));
    cw.seed_utf8("<init>");
    cw.seed_utf8(&descriptor);
    cw.seed_utf8(&signature);
    let completion = 1 + shape.capture_words();
    let mut code = CodeBuilder::new(completion + 1);
    let mut slot = 1u16;
    for Capture { field, .. } in &shape.captures {
        code.aload(0);
        load(field.ty, slot, &mut code);
        let reference = cw.fieldref(&shape.class, &field.name, &field.descriptor);
        code.putfield(reference, slot_words(field.ty) as i32);
        slot += slot_words(field.ty);
    }
    code.aload(0);
    code.push_int(i32::from(shape.arity), cw);
    code.aload(completion);
    let super_constructor = cw.methodref(
        SUSPEND_LAMBDA,
        "<init>",
        "(ILkotlin/coroutines/Continuation;)V",
    );
    code.invokespecial(super_constructor, 2, 0);
    code.ret_void();
    let locals = shape.parameter_locals(ir, lambda, SuspendLambdaMember::Constructor);
    cw.reserve_method_lvt(&locals);
    if public {
        finish_code_sig::<0x0001>(
            cw,
            "<init>",
            &descriptor,
            &mut code,
            completion + 1,
            Some(&signature),
        );
    } else {
        finish_code_sig::<0x0000>(
            cw,
            "<init>",
            &descriptor,
            &mut code,
            completion + 1,
            Some(&signature),
        );
    }
    cw.set_method_debug("<init>", &descriptor, None, &locals);
    if java_parameters {
        let reflected = shape.method_parameters(ir, lambda, SuspendLambdaMember::Constructor);
        cw.set_method_parameters("<init>", &descriptor, &reflected);
    }
}

/// `new Self(captures…, completion)`, left on the stack: the fresh copy `create` and `invoke`
/// start from.
fn construct_copy(cw: &mut ClassWriter, code: &mut CodeBuilder, shape: &Shape, completion: u16) {
    let class = cw.class_ref(&shape.class);
    code.new_obj(class);
    code.dup();
    for Capture { field, .. } in &shape.captures {
        code.aload(0);
        let reference = cw.fieldref(&shape.class, &field.name, &field.descriptor);
        code.getfield(reference, slot_words(field.ty) as i32);
    }
    code.aload(completion);
    let constructor = cw.methodref(&shape.class, "<init>", &shape.constructor_desc());
    code.invokespecial(constructor, shape.capture_words() as i32 + 1, 0);
}

/// Store the value on top of the stack into the copy in `copy`'s parameter field: `load` pushes
/// it after the copy.
fn store_parameter(
    cw: &mut ClassWriter,
    code: &mut CodeBuilder,
    shape: &Shape,
    copy: u16,
    field: &Field,
    load: impl FnOnce(&mut ClassWriter, &mut CodeBuilder),
) {
    code.aload(copy);
    load(cw, code);
    let reference = cw.fieldref(&shape.class, &field.name, &field.descriptor);
    code.putfield(reference, slot_words(field.ty) as i32);
}

/// `create([Object value,] Continuation)`: a fresh copy of the lambda with its parameter stored,
/// as a `Continuation<Unit>`.
fn emit_create(
    ir: &IrFile,
    cw: &mut ClassWriter,
    shape: &Shape,
    lambda: &SuspendLambdaClass,
    java_parameters: bool,
) {
    let descriptor = shape.create_desc();
    let parameter = shape.parameters.first();
    let signature = format!(
        "({}L{CONTINUATION}<*>;)L{CONTINUATION}<Lkotlin/Unit;>;",
        if parameter.is_some() { OBJECT_DESC } else { "" }
    );
    cw.seed_utf8("create");
    cw.seed_utf8(&descriptor);
    cw.seed_utf8(&signature);
    let completion = 1 + u16::from(parameter.is_some());
    let copy = completion + 1;
    let mut code = CodeBuilder::new(copy);
    construct_copy(cw, &mut code, shape, completion);
    if let Some(parameter) = parameter.filter(|parameter| parameter.field.is_some()) {
        let field = parameter.field.as_ref().expect("filtered on its field");
        code.astore(copy);
        store_parameter(cw, &mut code, shape, copy, field, |cw, code| {
            code.aload(1);
            if parameter.physical.is_jvm_scalar() {
                unbox_prim_from(
                    cw,
                    code,
                    Ty::obj("java/lang/Object"),
                    semantic_scalar_adapter(parameter.semantic, parameter.physical),
                );
            }
        });
        code.aload(copy);
    }
    let continuation = cw.class_ref(CONTINUATION);
    code.checkcast(continuation);
    code.areturn();
    let locals = shape.parameter_locals(ir, lambda, SuspendLambdaMember::Create);
    cw.reserve_method_lvt(&locals);
    let max_locals = if parameter.is_some_and(|parameter| parameter.field.is_some()) {
        copy + 1
    } else {
        copy
    };
    finish_code_sig::<0x0011>(
        cw,
        "create",
        &descriptor,
        &mut code,
        max_locals,
        Some(&signature),
    );
    cw.set_method_debug("create", &descriptor, None, &locals);
    if java_parameters {
        let reflected = shape.method_parameters(ir, lambda, SuspendLambdaMember::Create);
        cw.set_method_parameters("create", &descriptor, &reflected);
    }
}

/// The typed `invoke(parameters…, Continuation)`: start a fresh copy of the lambda with `Unit`.
/// With `create`, the copy is `create`'s; otherwise it is built and filled in place.
fn emit_invoke(
    ir: &IrFile,
    cw: &mut ClassWriter,
    formatter: &JvmSignatureFormatter,
    shape: &Shape,
    lambda: &SuspendLambdaClass,
    result: &str,
    java_parameters: bool,
) {
    let descriptor = shape.invoke_desc();
    let mut signature = String::from("(");
    for parameter in &shape.parameters {
        signature.push_str(
            &formatter
                .method_ty(&parameter.semantic, Wildcards::Declared)
                .expect("a suspend lambda's parameter has a JVM signature"),
        );
    }
    signature.push_str(&format!("L{CONTINUATION}<-{result}>;){OBJECT_DESC}"));
    cw.seed_utf8("invoke");
    cw.seed_utf8(&descriptor);
    cw.seed_utf8(&signature);
    let mut slots = Vec::with_capacity(shape.parameters.len());
    let mut completion = 1u16;
    for parameter in &shape.parameters {
        slots.push(completion);
        completion += slot_words(parameter.physical);
    }
    let copy = completion + 1;
    let mut code = CodeBuilder::new(copy);
    let max_locals = if shape.has_create() {
        code.aload(0);
        if let (Some(parameter), Some(&slot)) = (shape.parameters.first(), slots.first()) {
            load(parameter.physical, slot, &mut code);
            if parameter.physical.is_jvm_scalar() {
                box_prim_free(
                    cw,
                    &mut code,
                    semantic_scalar_adapter(parameter.semantic, parameter.physical),
                );
            }
        }
        code.aload(completion);
        let create = cw.methodref(&shape.class, "create", &shape.create_desc());
        code.invokevirtual(create, 1 + shape.parameters.len() as i32, 1);
        let class = cw.class_ref(&shape.class);
        code.checkcast(class);
        copy
    } else {
        construct_copy(cw, &mut code, shape, completion);
        code.astore(copy);
        for (parameter, &slot) in shape.parameters.iter().zip(&slots) {
            if let Some(field) = &parameter.field {
                store_parameter(cw, &mut code, shape, copy, field, |_, code| {
                    load(parameter.physical, slot, code)
                });
            }
        }
        code.aload(copy);
        copy + 1
    };
    let unit = cw.fieldref("kotlin/Unit", "INSTANCE", "Lkotlin/Unit;");
    code.getstatic(unit, 1);
    let invoke_suspend = cw.methodref(&shape.class, "invokeSuspend", INVOKE_SUSPEND_DESC);
    code.invokevirtual(invoke_suspend, 1, 1);
    code.areturn();
    let locals = shape.parameter_locals(ir, lambda, SuspendLambdaMember::Invoke);
    cw.reserve_method_lvt(&locals);
    finish_code_sig::<0x0011>(
        cw,
        "invoke",
        &descriptor,
        &mut code,
        max_locals,
        Some(&signature),
    );
    cw.set_method_debug("invoke", &descriptor, None, &locals);
    if java_parameters {
        let reflected = shape.method_parameters(ir, lambda, SuspendLambdaMember::Invoke);
        cw.set_method_parameters("invoke", &descriptor, &reflected);
    }
}

/// The erased `FunctionN.invoke(Object…)` bridge to the typed `invoke`. Its parameters are the
/// typed `invoke`'s recorded `FunctionN` values, erased.
fn emit_bridge(ir: &IrFile, cw: &mut ClassWriter, shape: &Shape, lambda: &SuspendLambdaClass) {
    let descriptor = jvm_function_invoke_descriptor(shape.arity);
    cw.seed_utf8("invoke");
    cw.seed_utf8(&descriptor);
    let locals_count = 1 + u16::from(shape.arity);
    let mut code = CodeBuilder::new(locals_count);
    code.aload(0);
    for (index, parameter) in shape.parameters.iter().enumerate() {
        code.aload(1 + index as u16);
        if parameter.physical.is_jvm_scalar() {
            unbox_prim_from(
                cw,
                &mut code,
                Ty::obj("java/lang/Object"),
                semantic_scalar_adapter(parameter.semantic, parameter.physical),
            );
        } else if let Some(internal) = checkcast_internal(parameter.physical) {
            let class = cw.class_ref(&internal);
            code.checkcast(class);
        }
    }
    code.aload(locals_count - 1);
    let continuation = cw.class_ref(CONTINUATION);
    code.checkcast(continuation);
    let argument_words: u16 = shape
        .parameters
        .iter()
        .map(|parameter| slot_words(parameter.physical))
        .sum::<u16>()
        + 1;
    let invoke = cw.methodref(&shape.class, "invoke", &shape.invoke_desc());
    code.invokevirtual(invoke, argument_words as i32, 1);
    code.areturn();
    finish_code::<0x1041>(cw, "invoke", &descriptor, &mut code, locals_count);
    let erased = vec![Ty::obj("java/lang/Object"); usize::from(shape.arity)];
    let names = crate::jvm::parameter_names::suspend_lambda_member(
        ir,
        &lambda.member_parameters,
        SuspendLambdaMember::Invoke,
        &erased,
    );
    let locals = std::iter::once(("this".to_string(), shape.self_desc(), 0u16))
        .chain(
            names
                .into_iter()
                .zip(1u16..)
                .map(|(name, slot)| (name, OBJECT_DESC.to_string(), slot)),
        )
        .collect::<Vec<_>>();
    cw.set_method_debug("invoke", &descriptor, None, &locals);
}
