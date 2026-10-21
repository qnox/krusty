//! JVM emission of read-only collection stubs and their bridges.
//!
//! A stub is `public`, throws `UnsupportedOperationException` with kotlinc's fixed message, and
//! carries no line numbers: kotlinc builds it without a source offset. Its bridge to the mutable
//! declaration's erased descriptor is `ACC_PUBLIC|ACC_BRIDGE|ACC_SYNTHETIC` with the same body.

use super::super::{
    finish_code_sig, jvm_declared_ty, jvm_tys, local_variable_desc, method_descriptor, slot_words,
    ClassWriter, CodeBuilder, EmitEnv, JvmSignatureFormatter,
};
use crate::ir::{IrClass, IrFile};
use crate::types::Ty;

const MESSAGE: &str = "Operation is not supported for read-only collection";
const EXCEPTION: &str = "java/lang/UnsupportedOperationException";

/// The stubs follow the class's declared members, in the mutable declarations' member order.
pub(super) fn emit_stubs(ir: &IrFile, class: &IrClass, cw: &mut ClassWriter, env: &EmitEnv<'_>) {
    let formatter = JvmSignatureFormatter::new(ir, env);
    let stubs = env
        .collection_members
        .read_only_stubs
        .for_class(class.fq_name_id());
    for stub in stubs {
        let parameters = jvm_tys(&stub.parameters);
        let result = jvm_declared_ty(&stub.result);
        let descriptor = method_descriptor(&parameters, result);
        let signature = formatter
            .method_positions(&stub.parameters, &stub.result)
            .filter(|signature| *signature != descriptor);
        let method = ThrowingMethod {
            name: &stub.name,
            parameters: &parameters,
            parameter_names: &stub.parameter_names,
            descriptor: &descriptor,
            signature: signature.as_deref(),
        };
        emit_throwing::<0x0001>(class, cw, &method);
    }
}

/// The stubs' bridges follow the class's ordinary bridges: kotlinc appends the stubs last.
pub(super) fn emit_stub_bridges(class: &IrClass, cw: &mut ClassWriter, env: &EmitEnv<'_>) {
    let stubs = env
        .collection_members
        .read_only_stubs
        .for_class(class.fq_name_id());
    for stub in stubs {
        let Some((parameters, result)) = &stub.bridge else {
            continue;
        };
        let descriptor = method_descriptor(parameters, *result);
        if cw.has_method(&stub.name, &descriptor) {
            continue;
        }
        let method = ThrowingMethod {
            name: &stub.name,
            parameters,
            parameter_names: &stub.parameter_names,
            descriptor: &descriptor,
            signature: None,
        };
        emit_throwing::<{ 0x0001 | 0x0040 | 0x1000 }>(class, cw, &method);
    }
}

struct ThrowingMethod<'a> {
    name: &'a str,
    parameters: &'a [Ty],
    parameter_names: &'a [String],
    descriptor: &'a str,
    signature: Option<&'a str>,
}

fn emit_throwing<const ACCESS: u16>(
    class: &IrClass,
    cw: &mut ClassWriter,
    method: &ThrowingMethod,
) {
    assert_eq!(
        method.parameters.len(),
        method.parameter_names.len(),
        "a read-only collection stub names every parameter"
    );
    // ASM visits the method header before its body, so the header's pool entries come first.
    cw.reserve_method_pool(method.name, method.descriptor, method.signature, &[]);
    let words: u16 = method.parameters.iter().map(|ty| slot_words(*ty)).sum();
    let mut code = CodeBuilder::new(1 + words);
    let exception = cw.class_ref(EXCEPTION);
    code.new_obj(exception);
    code.dup();
    code.push_string(MESSAGE, cw);
    let constructor = cw.methodref(EXCEPTION, "<init>", "(Ljava/lang/String;)V");
    code.invokespecial(constructor, 1, 0);
    code.athrow();
    finish_code_sig::<ACCESS>(
        cw,
        method.name,
        method.descriptor,
        &mut code,
        1 + words,
        method.signature,
    );
    let mut locals = vec![(String::from("this"), format!("L{};", class.fq_name()), 0)];
    let mut slot = 1u16;
    for (parameter, name) in method.parameters.iter().zip(method.parameter_names) {
        locals.push((name.clone(), local_variable_desc(*parameter), slot));
        slot += slot_words(*parameter);
    }
    cw.set_method_debug(method.name, method.descriptor, None, &locals);
}
