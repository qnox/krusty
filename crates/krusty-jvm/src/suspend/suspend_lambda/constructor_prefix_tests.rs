use super::*;
use crate::capture_names::constructor_prefix_tests::{identity, original_prefix};

#[test]
fn suspend_lambda_layout_accepts_exact_prefix_receiver_identity() {
    let (mut ir, owner, class) = original_prefix();
    let body = ir.add_expr(IrExpr::UnitInstance);
    let operand = ir.add_expr(IrExpr::GetValue(1));
    let function = ir.add_fun(krusty::ir::IrFunction {
        name: "suspendHelper".into(),
        params: vec![ir.classes[class as usize].ctor_args[0].ty],
        ret: Ty::Unit,
        body: Some(body),
        is_static: true,
        dispatch_receiver: None,
        param_checks: vec![None],
    });
    ir.fn_params.insert(
        function,
        krusty::ir::FnParamInfo::identities(vec![identity(owner, 0)]),
    );
    ir.lambda_own_params_from.insert(function, 1);
    let site = Site {
        node: body,
        template_copies: vec![],
        class: krusty::types::type_name("SuspendLambda"),
        function_type: Ty::fun_suspend(vec![], Ty::Unit),
        captures: vec![operand],
    };
    let (captures, parameters, member_parameters) = layout(&ir, function, body, &site)
        .expect("recorded constructor receiver survives suspend layout validation");
    assert_eq!(captures.len(), 1);
    assert_eq!(captures[0].name, "this$0");
    assert!(!captures[0].crossinline);
    assert_eq!(parameters.len(), 0);
    assert_eq!(member_parameters.captures(), &[identity(owner, 0)]);
}
