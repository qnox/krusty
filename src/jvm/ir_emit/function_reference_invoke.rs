//! Emission details owned by a callable-reference class's `invoke` methods.
//!
//! Carrier construction and target dispatch remain orchestrated by the parent emitter. This module
//! owns the two physical `invoke` forms and their frame/debug contracts: loading erased FunctionN
//! arguments for the dispatching body, and emitting the erased bridge to a carrier's own
//! specialized `invoke`.

use super::*;

/// kotlinc's `LocalVariableTable` for a callable-reference constructor: `this`, then each
/// constructor parameter under its generated name. The entries intern after the constructor's
/// code, where a writer visits them, and attach to the finished `<init>`.
pub(super) fn reference_constructor_locals(
    cw: &mut ClassWriter,
    class: &str,
    parameters: &[&str],
) -> Vec<(String, String, u16)> {
    let mut locals = vec![("this".to_string(), format!("L{class};"), 0u16)];
    for (index, name) in parameters.iter().enumerate() {
        locals.push((
            name.to_string(),
            "Ljava/lang/Object;".to_string(),
            index as u16 + 1,
        ));
    }
    cw.reserve_method_lvt(&locals);
    locals
}

/// The erased `FunctionN.invoke(Object…)Object` bridge to a carrier's own specialized `invoke`:
/// each argument is cast or unboxed to the specialized parameter, and the result is returned as
/// an object (`Unit` for a `void` specialization). Flagged, lined and tabled as kotlinc's bridge.
pub(super) fn emit_reference_invoke_bridge(
    ir: &IrFile,
    cw: &mut ClassWriter,
    class: &str,
    fr: &crate::ir::FuncRef,
    invoke: u32,
    arity: u8,
) {
    let function = &ir.functions[invoke as usize];
    let parameters = jvm_function_params(ir, invoke);
    let result = jvm_declared_ty(&function.ret);
    let specialized = method_descriptor(&parameters, result);
    let erased = jvm_function_invoke_descriptor(arity);
    // A specialization that already erases to `FunctionN.invoke` is that method; nothing bridges.
    if specialized == erased {
        return;
    }
    cw.seed_utf8("invoke");
    cw.seed_utf8(&erased);
    let mut code = CodeBuilder::new(1 + u16::from(arity));
    code.aload(0);
    for (index, parameter) in parameters.iter().enumerate() {
        code.aload(1 + index as u16);
        if parameter.is_jvm_scalar() {
            let semantic = fr.param_tys.get(index).copied().unwrap_or(*parameter);
            unbox_prim(cw, &mut code, semantic_scalar_adapter(semantic, *parameter));
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
    } else if result.is_jvm_scalar() {
        box_prim_free(cw, &mut code, semantic_scalar_adapter(fr.ret_ty, result));
    }
    code.areturn();
    finish_code::<0x1041>(cw, "invoke", &erased, &mut code, 1 + u16::from(arity));
    let this_desc = format!("L{class};");
    let mut locals = vec![("this".to_string(), this_desc, 0u16)];
    for index in 0..arity as u16 {
        locals.push((
            crate::jvm::parameter_names::reference_invoke_bridge_parameter(index),
            "Ljava/lang/Object;".to_string(),
            index + 1,
        ));
    }
    let line = ir
        .fn_decl_lines
        .get(&invoke)
        .copied()
        .filter(|&line| line != 0);
    cw.set_method_debug("invoke", &erased, line.map(|line| (0, line)), &locals);
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

pub(super) fn func_ref_invoke_locals(
    cw: &mut ClassWriter,
    self_class: &str,
    arity: u16,
    high_arity: bool,
) -> Vec<VerifType> {
    let mut locals = vec![VerifType::Object(cw.class_ref(self_class))];
    let obj = VerifType::Object(cw.class_ref("java/lang/Object"));
    if high_arity {
        locals.push(VerifType::Object(cw.class_ref("[Ljava/lang/Object;")));
    } else {
        locals.extend(std::iter::repeat_n(obj, arity as usize));
    }
    locals
}

pub(super) fn func_ref_call_stack_prefix(
    cw: &mut ClassWriter,
    dispatch: &crate::ir::FrDispatch,
    call_owner: &str,
    target_param_tys: &[Ty],
    field_capture_tys: &[Ty],
    field_capture_count: usize,
) -> Vec<VerifType> {
    let mut prefix = field_capture_tys
        .iter()
        .map(|capture| verif_for_jvm_free(cw, ir_ty_to_jvm(capture)))
        .collect::<Vec<_>>();
    prefix.extend(match dispatch {
        crate::ir::FrDispatch::Static => Vec::new(),
        crate::ir::FrDispatch::StaticBound => target_param_tys
            .get(field_capture_count)
            .map(|target| verif_for_jvm_free(cw, ir_ty_to_jvm(target)))
            .into_iter()
            .collect(),
        crate::ir::FrDispatch::VirtualBound
        | crate::ir::FrDispatch::VirtualUnbound
        | crate::ir::FrDispatch::SuspendConvert => {
            vec![VerifType::Object(cw.class_ref(call_owner))]
        }
    });
    prefix
}
