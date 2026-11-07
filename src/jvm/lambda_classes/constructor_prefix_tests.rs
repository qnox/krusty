use super::*;
use crate::jvm::capture_names::constructor_prefix_tests::{identity, original_prefix};

#[test]
fn class_lambda_keeps_a_constructor_receiver_without_a_source_spelling() {
    let (mut ir, owner, class) = original_prefix();
    let body = ir.add_expr(IrExpr::UnitInstance);
    let operand = ir.add_expr(IrExpr::GetValue(1));
    let function = ir.add_fun(crate::ir::IrFunction {
        name: "helper".into(),
        params: vec![ir.classes[class as usize].ctor_args[0].ty],
        ret: Ty::Unit,
        body: Some(body),
        is_static: true,
        dispatch_receiver: None,
        param_checks: vec![None],
    });
    ir.fn_params.insert(
        function,
        crate::ir::FnParamInfo::identities(vec![identity(owner, 0)]),
    );
    ir.lambda_own_params_from.insert(function, 1);
    let site = Site {
        node: body,
        template_copies: vec![],
        class: crate::types::type_name("Lambda"),
        function_type: Ty::fun(vec![], Ty::Unit),
        captures: vec![operand],
    };
    let captures = captures(&ir, function, body, &site).expect("typed constructor receiver");
    assert_eq!(captures.len(), 1);
    assert_eq!(
        captures[0].capture,
        crate::ir::IrLambdaCapture::ConstructorReceiver {
            owner,
            parameter: 0
        }
    );
    assert_eq!(captures[0].ty, ir.classes[class as usize].ctor_args[0].ty);
    assert_eq!(captures[0].shared_holder, None);
    assert_eq!(
        ir.function_parameter_identities(function).unwrap()[0].source_name,
        None
    );
}
