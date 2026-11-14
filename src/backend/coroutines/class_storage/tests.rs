use super::*;

fn runtime() -> crate::ir::IrCoroutineRuntime {
    use crate::fir::{ExternalCallableId, ExternalPropertyId};
    use crate::types::type_name;

    crate::ir::IrCoroutineRuntime {
        suspended: ExternalPropertyId::from_raw(0),
        get_or_throw: ExternalCallableId::from_raw(0),
        resume_with: ExternalCallableId::from_raw(1),
        resume_with_declared_parameter: Ty::Unit,
        resume_with_declared_result: Ty::Unit,
        context: ExternalPropertyId::from_raw(1),
        context_getter: ExternalCallableId::from_raw(2),
        context_ty: Ty::obj_name(crate::types::wk::any()),
        context_receiver_ty: Ty::obj_name(crate::types::wk::any()),
        result_success: ExternalCallableId::from_raw(3),
        result_companion: type_name("kotlin/Result/Companion"),
        result_failure: ExternalCallableId::from_raw(4),
    }
}

#[test]
fn suspend_function_value_keeps_source_result_and_gets_one_machine_continuation() {
    let class_name = crate::types::type_name("machine/GeneratedContinuation");
    let class_ty = Ty::obj_name(class_name);
    let mut ir = IrFile::default();
    let class = ir.add_class(crate::ir::IrClass::synthetic(class_name));

    for result in [
        Ty::String,
        Ty::ty_param("T", Ty::nullable(Ty::obj_name(crate::types::wk::any()))),
    ] {
        let function = ir.add_expr(IrExpr::GetValue(1));
        let argument = ir.add_expr(IrExpr::GetValue(2));
        let source = ir.add_expr(IrExpr::InvokeFunction {
            func: function,
            args: vec![argument],
            params: vec![Ty::Int],
            ret: result,
        });
        ir.suspend_calls.insert(source, result);
        let fields = ContinuationFields {
            class,
            machine_ty: class_ty,
            label: 0,
            result: 1,
            runtime: runtime(),
            suspend_functions: HashSet::default(),
        };

        let lowered = fields
            .call_with_continuation(&mut ir, source)
            .expect("a selected function-value suspension has a CPS call shape");
        let IrExpr::InvokeFunction {
            args, params, ret, ..
        } = ir.expr(lowered)
        else {
            panic!("a CPS function-value call remains an invocation")
        };
        assert_eq!(args.len(), 2);
        let continuation = continuation_passing(result).0;
        assert_eq!(params, &[Ty::Int, continuation]);
        assert!(matches!(ir.expr(args[1]), IrExpr::GetValue(0)));
        assert_eq!(ir.checked_type(args[1]), Some(continuation));
        assert_eq!(*ret, Ty::nullable(Ty::obj_name(crate::types::wk::any())));
        assert_eq!(ir.suspend_calls.get(&lowered), Some(&result));
        assert_eq!(ir.call_declared_ret.get(&lowered), Some(ret));

        // Relocation leaves the source node's operands and semantic result untouched.
        let IrExpr::InvokeFunction {
            args, params, ret, ..
        } = ir.expr(source)
        else {
            unreachable!("the source expression remains an invocation")
        };
        assert_eq!(args.len(), 1);
        assert_eq!(params, &[Ty::Int]);
        assert_eq!(*ret, result);
        assert!(!ir.suspend_calls.contains_key(&source));
    }
}
