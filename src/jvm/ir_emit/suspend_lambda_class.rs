//! The class kotlinc writes for a suspend lambda (`SuspendLambdaLowering`, then `ClassCodegen`).
//!
//! The lambda extends `SuspendLambda`, implements its `FunctionN` and is its own continuation.
//! Its members, in kotlinc's order: the constructor over the captured values and the completion,
//! `invokeSuspend` (the lambda's body, which kotlinc's coroutine transformer turns into the state
//! machine when the class is written), `create` for a lambda of at most one parameter, the typed
//! `invoke`, and the erased `FunctionN.invoke` bridge to it. A fresh copy of the lambda, built by
//! `create` or inline in `invoke`, keeps each parameter the body reads in its field.

use super::*;
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

/// A captured value's field, and the name the constructor's parameter takes.
struct Capture {
    field: Field,
    parameter: String,
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
                ty: jvm_declared_ty(&field.ty),
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
            .map(|(index, &(ty, stored))| Parameter {
                semantic: signature.params.get(index).copied().unwrap_or(ty),
                physical: jvm_declared_ty(&ty),
                field: stored.map(field),
            })
            .collect::<Vec<_>>();
        Shape {
            class: c.fq_name(),
            captures: lambda
                .captures
                .iter()
                .map(|capture| {
                    let field = field(capture.field);
                    let parameter = if capture.receiver {
                        "$receiver".to_string()
                    } else {
                        field.name.clone()
                    };
                    Capture { field, parameter }
                })
                .collect(),
            arity: u8::try_from(parameters.len() + 1)
                .expect("a suspend lambda's arity fits its FunctionN"),
            parameters,
            result: signature.ret,
        }
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
    cw.set_access(0x0010 | 0x0020); // FINAL | SUPER
    if let Some((owner, method)) = class_enclosure(ir, c, facade) {
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
    for Capture { field, .. } in &shape.captures {
        cw.add_field_late(0x1010, &field.name, &field.descriptor, None, None);
    }

    emit_constructor(&mut cw, &formatter, &shape, lambda);
    emit_method(
        ir,
        lambda.invoke_suspend,
        &shape.class,
        facade,
        &mut cw,
        true,
        env,
    );
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
        emit_create(&mut cw, &shape);
    }
    emit_invoke(&mut cw, &formatter, &shape, &result);
    emit_bridge(&mut cw, &shape);
    // A lambda class is local to the scope it was written in.
    cw.set_kotlin_metadata(3, &[2, 4, 0], synthetic_class_xi(SYNTHETIC_LOCAL), &[], &[]);
    env.run.finish_class(cw)
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
/// `SuspendLambda(arity, completion)`.
fn emit_constructor(
    cw: &mut ClassWriter,
    formatter: &JvmSignatureFormatter,
    shape: &Shape,
    lambda: &SuspendLambdaClass,
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
    let mut locals = vec![("this".to_string(), shape.self_desc(), 0u16)];
    let mut slot = 1u16;
    for Capture { field, parameter } in &shape.captures {
        code.aload(0);
        load(field.ty, slot, &mut code);
        let reference = cw.fieldref(&shape.class, &field.name, &field.descriptor);
        code.putfield(reference, slot_words(field.ty) as i32);
        locals.push((parameter.clone(), field.descriptor.clone(), slot));
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
    locals.push((
        "$completion".to_string(),
        CONTINUATION_DESC.to_string(),
        completion,
    ));
    cw.reserve_method_lvt(&locals);
    finish_code_sig::<0x0000>(
        cw,
        "<init>",
        &descriptor,
        &mut code,
        completion + 1,
        Some(&signature),
    );
    cw.set_method_debug("<init>", &descriptor, None, &locals);
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
fn emit_create(cw: &mut ClassWriter, shape: &Shape) {
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
    let mut locals = vec![("this".to_string(), shape.self_desc(), 0u16)];
    if parameter.is_some() {
        locals.push(("value".to_string(), OBJECT_DESC.to_string(), 1));
    }
    locals.push((
        "$completion".to_string(),
        CONTINUATION_DESC.to_string(),
        completion,
    ));
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
}

/// The typed `invoke(parameters…, Continuation)`: start a fresh copy of the lambda with `Unit`.
/// With `create`, the copy is `create`'s; otherwise it is built and filled in place.
fn emit_invoke(
    cw: &mut ClassWriter,
    formatter: &JvmSignatureFormatter,
    shape: &Shape,
    result: &str,
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
    let mut locals = vec![("this".to_string(), shape.self_desc(), 0u16)];
    for (index, (parameter, &slot)) in shape.parameters.iter().zip(&slots).enumerate() {
        locals.push((
            crate::jvm::parameter_names::suspend_lambda_invoke_parameter(index as u16),
            type_descriptor(parameter.physical),
            slot,
        ));
    }
    locals.push((
        crate::jvm::parameter_names::suspend_lambda_invoke_parameter(shape.parameters.len() as u16),
        CONTINUATION_DESC.to_string(),
        completion,
    ));
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
}

/// The erased `FunctionN.invoke(Object…)` bridge to the typed `invoke`.
fn emit_bridge(cw: &mut ClassWriter, shape: &Shape) {
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
    let mut locals = vec![("this".to_string(), shape.self_desc(), 0u16)];
    for index in 0..u16::from(shape.arity) {
        locals.push((
            crate::jvm::parameter_names::suspend_lambda_invoke_parameter(index),
            OBJECT_DESC.to_string(),
            index + 1,
        ));
    }
    cw.set_method_debug("invoke", &descriptor, None, &locals);
}
