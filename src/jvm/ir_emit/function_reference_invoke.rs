//! Emission details owned by a callable-reference class's `invoke` methods.
//!
//! Carrier construction and target dispatch remain orchestrated by the parent emitter. This module
//! owns the two physical `invoke` forms and their frame/debug contracts: loading erased FunctionN
//! arguments for the dispatching body, and emitting the erased bridge to a carrier's own
//! specialized `invoke`.

use super::*;

/// kotlinc's `LocalVariableTable` for a callable-reference constructor: `this`, then each
/// constructor parameter under its name, with its descriptor. The entries intern after the
/// constructor's code, where a writer visits them, and attach to the finished `<init>`.
pub(super) fn reference_constructor_locals(
    cw: &mut ClassWriter,
    class: &str,
    parameters: &[(&str, &str)],
) -> Vec<(String, String, u16)> {
    let mut locals = vec![("this".to_string(), format!("L{class};"), 0u16)];
    let mut slot = 1u16;
    for (name, descriptor) in parameters {
        locals.push((name.to_string(), descriptor.to_string(), slot));
        slot += if matches!(*descriptor, "J" | "D") {
            2
        } else {
            1
        };
    }
    cw.reserve_method_lvt(&locals);
    locals
}

/// What a callable-reference carrier's bridge adapts.
pub(super) fn reference_invoke_bridge(fr: &crate::ir::FuncRef) -> crate::ir::IrInvokeBridge {
    crate::ir::IrInvokeBridge {
        param_tys: fr.param_tys.clone(),
        ret_ty: fr.ret_ty,
        unbox_params: fr.unbox_params.clone(),
        unbox_param_nullable: fr.unbox_param_nullable.clone(),
        box_ret: fr.box_ret,
        invoke_renamed: fr.invoke_renamed,
    }
}

/// The erased `FunctionN.invoke(Object…)Object` bridge to a class's own specialized `invoke`, a
/// callable-reference carrier's or a lambda class's: each argument is cast or unboxed to the
/// specialized parameter (a value class through its `unbox-impl`), and the result is returned as
/// an object (`Unit` for a `void` specialization, a value class through its `box-impl`). Past the
/// numbered interfaces the arguments arrive as one array, whose length is checked first. Flagged,
/// lined and tabled as kotlinc's bridge.
pub(super) fn emit_reference_invoke_bridge(
    ir: &IrFile,
    cw: &mut ClassWriter,
    class: &str,
    bridge: &crate::ir::IrInvokeBridge,
    suspend: bool,
    invoke: u32,
    arity: u8,
) {
    let function = &ir.functions[invoke as usize];
    let parameters = jvm_function_params(ir, invoke);
    let result = jvm_declared_ty(&function.ret);
    let specialized = method_descriptor(&parameters, result);
    let erased = jvm_function_invoke_descriptor(arity);
    // A specialization that already erases to `FunctionN.invoke` is that method; nothing bridges.
    // A renamed one over an `Any`-backed value class keeps the erased descriptor under its own name.
    if !bridge.invoke_renamed && specialized == erased {
        return;
    }
    let high_arity = is_high_arity_function(arity);
    let erased_words = if high_arity { 1 } else { u16::from(arity) };
    cw.seed_utf8("invoke");
    cw.seed_utf8(&erased);
    let mut code = CodeBuilder::new(1 + erased_words);
    if high_arity {
        check_argument_count(cw, &mut code, usize::from(arity));
    }
    code.aload(0);
    for (index, parameter) in parameters.iter().enumerate() {
        load_erased_function_argument(cw, &mut code, high_arity, index);
        if let Some(value_class) = bridge.unbox_params.get(index).copied().flatten() {
            emit_value_class_unbox_adapter(
                cw,
                &mut code,
                value_class,
                *parameter,
                // Recorded beside `unbox_params` by the pass that named the value class.
                bridge.unbox_param_nullable[index],
            );
        } else if parameter.is_jvm_scalar() {
            let semantic = bridge.param_tys.get(index).copied().unwrap_or(*parameter);
            unbox_prim_from(
                cw,
                &mut code,
                Ty::obj("java/lang/Object"),
                semantic_scalar_adapter(semantic, *parameter),
            );
        } else if let Some(internal) = checkcast_internal(*parameter) {
            let class_ref = cw.class_ref(&internal);
            code.checkcast(class_ref);
        }
    }
    let argument_words: u16 = parameters.iter().map(|ty| slot_words(*ty)).sum();
    let method = cw.methodref(class, &function.name, &specialized);
    let result_words = if matches!(result, Ty::Unit | Ty::Nothing) {
        0
    } else {
        slot_words(result) as i32
    };
    code.invokevirtual(method, argument_words as i32, result_words);
    if matches!(result, Ty::Unit | Ty::Nothing) {
        let unit = cw.fieldref("kotlin/Unit", "INSTANCE", "Lkotlin/Unit;");
        code.getstatic(unit, 1);
    } else if let Some(value_class) = bridge.box_ret {
        let nullable = bridge.ret_ty.is_nullable();
        emit_value_class_box_adapter(cw, &mut code, value_class, result, nullable);
    } else if result.is_jvm_scalar() {
        box_prim_free(
            cw,
            &mut code,
            semantic_scalar_adapter(bridge.ret_ty, result),
        );
    }
    code.areturn();
    let this_desc = format!("L{class};");
    let mut locals = vec![("this".to_string(), this_desc, 0u16)];
    if high_arity {
        locals.push(("args".to_string(), "[Ljava/lang/Object;".to_string(), 1));
        cw.reserve_method_lvt(&locals);
        finish_code::<0x1051>(cw, "invoke", &erased, &mut code, 2);
        // The array check has no source line, so kotlinc maps none of the bridge.
        cw.set_method_debug("invoke", &erased, None, &locals);
        return;
    }
    for index in 0..arity as u16 {
        locals.push((
            crate::jvm::parameter_names::reference_invoke_bridge_parameter(
                index,
                suspend && index + 1 == u16::from(arity),
            ),
            "Ljava/lang/Object;".to_string(),
            index + 1,
        ));
    }
    // The table's names intern after the code and ahead of its stack-map frames, as a writer
    // visits them.
    cw.reserve_method_lvt(&locals);
    finish_code::<0x1041>(cw, "invoke", &erased, &mut code, 1 + u16::from(arity));
    let line = ir
        .fn_decl_lines
        .get(&invoke)
        .copied()
        .filter(|&line| line != 0);
    cw.set_method_debug("invoke", &erased, line.map(|line| (0, line)), &locals);
}

/// `FunctionN.invoke(Object[])`'s guard: an argument array of any other length than `arity`
/// throws `IllegalArgumentException("Expected <arity> arguments")`.
pub(super) fn check_argument_count(cw: &mut ClassWriter, code: &mut CodeBuilder, arity: usize) {
    let counted = code.new_label();
    code.aload(1);
    code.arraylength();
    code.push_int(
        i32::try_from(arity).expect("function arity fits an int"),
        cw,
    );
    code.if_icmpeq(counted);
    let exception = cw.class_ref("java/lang/IllegalArgumentException");
    code.new_obj(exception);
    code.dup();
    code.push_string(&format!("Expected {arity} arguments"), cw);
    let constructor = cw.methodref(
        "java/lang/IllegalArgumentException",
        "<init>",
        "(Ljava/lang/String;)V",
    );
    code.invokespecial(constructor, 1, 0);
    code.athrow();
    code.bind(counted);
}

pub(super) fn load_erased_function_argument(
    cw: &mut ClassWriter,
    code: &mut CodeBuilder,
    high_arity: bool,
    index: usize,
) {
    code.aload(1 + u16::from(!high_arity) * index as u16);
    if high_arity {
        code.push_int(index as i32, cw);
        code.array_load(0x32, 1); // aaload
    }
}
