//! The class kotlinc writes for function values converted to one fun interface (see
//! `crate::jvm::sam_wrappers`).
//!
//! `final synthetic class X implements I` and, for a Kotlin fun interface, `FunctionAdapter`.
//! Member order is kotlinc's: the constructor, which checks and stores the function value; the
//! interface method; and, when the class implements `FunctionAdapter`, `getFunctionDelegate`,
//! `equals` and `hashCode`, which compare the wrapped values. A Java interface under
//! `-Xsam-conversions=class` stops after the interface method.

use super::*;

const OBJECT: &str = "java/lang/Object";
const FUNCTION_ADAPTER: &str = "kotlin/jvm/internal/FunctionAdapter";
const GET_FUNCTION_DELEGATE: &str = "()Lkotlin/Function;";

/// Write the wrapper class `c`.
pub(super) fn emit_sam_wrapper_class(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    wrapper: &crate::ir::IrSamWrapperClass,
    facade: &str,
    env: &EmitEnv,
    opts: &EmitOptions,
) -> Vec<u8> {
    let class = c.fq_name();
    let interface = wrapper.interface.render();
    let field = c
        .fields
        .first()
        .expect("a SAM wrapper holds its function value");
    let field_descriptor = ir_type_desc(&field.ty);
    let (field_signature, constructor_signature) = wrapper
        .suspend_arity
        .map(|arity| suspend_function_signatures(&field_descriptor, arity))
        .unzip();
    let mut cw = new_writer(&class, OBJECT, opts);
    cw.set_access(0x1030 | u16::from(wrapper.public_inline)); // PUBLIC? | FINAL | SUPER | SYNTHETIC
    if let Some((owner, method)) = class_enclosure(ir, env.override_results, c, facade) {
        match method {
            Some((name, descriptor)) => cw.set_enclosing_method(&owner, &name, &descriptor),
            None => cw.set_enclosing_class(&owner),
        }
    }
    env.inner_classes.register(&mut cw);
    cw.add_interface(&interface);
    if wrapper.function_adapter {
        cw.add_interface(FUNCTION_ADAPTER);
    }

    // `<init>(FunctionN)`: check the value, then `Object()`, then store it.
    // kotlinc's writer interns a method's name, descriptor, signature and annotations before its
    // code, and its local names after.
    let constructor = format!("({field_descriptor})V");
    cw.seed_utf8("<init>");
    cw.seed_utf8(&constructor);
    let mut code = CodeBuilder::new(2);
    code.aload(1);
    code.push_string("function", &mut cw);
    let check = cw.methodref(
        "kotlin/jvm/internal/Intrinsics",
        "checkNotNullParameter",
        "(Ljava/lang/Object;Ljava/lang/String;)V",
    );
    code.invokestatic(check, 2, 0);
    code.aload(0);
    let object = cw.methodref(OBJECT, "<init>", "()V");
    code.invokespecial(object, 0, 0);
    code.aload(0);
    code.aload(1);
    let function = cw.fieldref(&class, &field.name, &field_descriptor);
    code.putfield(function, 2);
    code.ret_void();
    let locals = function_reference_invoke::reference_constructor_locals(
        &mut cw,
        &class,
        &[(field.name.as_str(), field_descriptor.as_str())],
    );
    if wrapper.public_inline {
        finish_code_sig::<0x0001>(
            &mut cw,
            "<init>",
            &constructor,
            &mut code,
            2,
            constructor_signature.as_deref(),
        );
    } else {
        finish_code_sig::<0x0000>(
            &mut cw,
            "<init>",
            &constructor,
            &mut code,
            2,
            constructor_signature.as_deref(),
        );
    }
    cw.set_method_debug("<init>", &constructor, None, &locals);
    cw.add_field_late_sig(
        0x1012, // PRIVATE | FINAL | SYNTHETIC
        &field.name,
        &field_descriptor,
        field_signature.as_deref(),
        None,
        None,
    );

    emit_method(
        ir,
        wrapper.method,
        StaticOwner::Class(c.fq_name),
        &class,
        facade,
        &mut cw,
        true,
        env,
    );

    if wrapper.function_adapter {
        emit_function_adapter(&mut cw, &class, &interface, function);
    }

    bridge_emission::emit_bridges(ir, c, &mut cw, env);

    finish_local_synthetic_class(cw, env)
}

/// `FunctionAdapter` members a Kotlin fun-interface wrapper adds after its own method.
fn emit_function_adapter(cw: &mut ClassWriter, class: &str, interface: &str, function: u16) {
    let this = [("this".to_string(), format!("L{class};"), 0u16)];
    // `getFunctionDelegate()`: the wrapped value.
    for constant in [
        "getFunctionDelegate",
        GET_FUNCTION_DELEGATE,
        "()Lkotlin/Function<*>;",
        "Lorg/jetbrains/annotations/NotNull;",
    ] {
        cw.seed_utf8(constant);
    }
    let mut code = CodeBuilder::new(1);
    code.aload(0);
    code.getfield(function, 1);
    let kotlin_function = cw.class_ref("kotlin/Function");
    code.checkcast(kotlin_function);
    code.areturn();
    cw.reserve_method_lvt(&this);
    cw.add_method_sig(
        0x0011,
        "getFunctionDelegate",
        GET_FUNCTION_DELEGATE,
        &code,
        Some("()Lkotlin/Function<*>;"),
    );
    cw.set_method_debug("getFunctionDelegate", GET_FUNCTION_DELEGATE, None, &this);
    cw.set_method_nullability(
        "getFunctionDelegate",
        GET_FUNCTION_DELEGATE,
        Some("Lorg/jetbrains/annotations/NotNull;"),
        &[],
    );

    emit_equals(cw, class, interface);

    // `hashCode()`: the wrapped value's.
    cw.seed_utf8("hashCode");
    cw.seed_utf8("()I");
    let mut code = CodeBuilder::new(1);
    code.aload(0);
    let adapter = cw.class_ref(FUNCTION_ADAPTER);
    code.checkcast(adapter);
    let delegate = cw.interface_methodref(
        FUNCTION_ADAPTER,
        "getFunctionDelegate",
        GET_FUNCTION_DELEGATE,
    );
    code.invokeinterface(delegate, 0, 1);
    let hash = cw.methodref(OBJECT, "hashCode", "()I");
    code.invokevirtual(hash, 0, 1);
    code.ireturn();
    cw.reserve_method_lvt(&this);
    cw.add_method(0x0011, "hashCode", "()I", &code);
    cw.set_method_debug("hashCode", "()I", None, &this);
}

/// Generic signatures kotlinc puts on a suspend wrapper's function field and constructor. The
/// synthetic class retains the function-shape variables (`P1..Pn`, `R`) even though its erased
/// descriptor is the numbered `Function{n + 1}` interface.
fn suspend_function_signatures(function_descriptor: &str, arity: u8) -> (String, String) {
    let owner = function_descriptor
        .strip_suffix(';')
        .expect("a function field has an object descriptor");
    let mut field = format!("{owner}<");
    let mut constructor = format!("({owner}<");
    for parameter in 1..=arity {
        field.push_str(&format!("TP{parameter};"));
        constructor.push_str(&format!("-TP{parameter};"));
    }
    field.push_str("Lkotlin/coroutines/Continuation<-TR;>;Ljava/lang/Object;>;");
    constructor.push_str("-Lkotlin/coroutines/Continuation<-TR;>;+Ljava/lang/Object;>;)V");
    (field, constructor)
}

/// `equals(Object)`: another wrapper of the same interface whose wrapped value equals this one's.
fn emit_equals(cw: &mut ClassWriter, class: &str, interface: &str) {
    for constant in [
        "equals",
        "(Ljava/lang/Object;)Z",
        "Lorg/jetbrains/annotations/Nullable;",
    ] {
        cw.seed_utf8(constant);
    }
    let mut code = CodeBuilder::new(2);
    let not_adapter = code.new_label();
    let not_interface = code.new_label();
    let done = code.new_label();
    code.aload(1);
    let interface = cw.class_ref(interface);
    code.instance_of(interface);
    code.ifeq(not_interface);
    code.aload(1);
    let adapter = cw.class_ref(FUNCTION_ADAPTER);
    code.instance_of(adapter);
    code.ifeq(not_adapter);
    let delegate = cw.interface_methodref(
        FUNCTION_ADAPTER,
        "getFunctionDelegate",
        GET_FUNCTION_DELEGATE,
    );
    code.aload(0);
    code.checkcast(adapter);
    code.invokeinterface(delegate, 0, 1);
    code.aload(1);
    code.checkcast(adapter);
    code.invokeinterface(delegate, 0, 1);
    let equal = cw.methodref(
        "kotlin/jvm/internal/Intrinsics",
        "areEqual",
        "(Ljava/lang/Object;Ljava/lang/Object;)Z",
    );
    code.invokestatic(equal, 2, 1);
    code.goto(done);
    code.bind(not_adapter);
    code.push_int(0, cw);
    code.goto(done);
    code.bind(not_interface);
    code.push_int(0, cw);
    code.bind(done);
    code.ireturn();
    code.link();
    let locals = [
        ("this".to_string(), format!("L{class};"), 0u16),
        ("other".to_string(), "Ljava/lang/Object;".to_string(), 1),
    ];
    cw.reserve_method_lvt(&locals);
    cw.add_method(0x0011, "equals", "(Ljava/lang/Object;)Z", &code);
    cw.set_method_debug("equals", "(Ljava/lang/Object;)Z", None, &locals);
    cw.set_method_nullability(
        "equals",
        "(Ljava/lang/Object;)Z",
        None,
        &[Some("Lorg/jetbrains/annotations/Nullable;")],
    );
}
