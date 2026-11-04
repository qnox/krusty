use super::*;
use crate::ir::{Callee, IrConst, IrFunction};

fn function(params: Vec<Ty>, ret: Ty, body: Option<ExprId>) -> IrFunction {
    IrFunction {
        name: "f".to_string(),
        param_checks: vec![None; params.len()],
        params,
        ret,
        body,
        is_static: true,
        dispatch_receiver: None,
    }
}

fn any() -> Ty {
    Ty::nullable(Ty::obj_name(wk::any()))
}

/// `suspend fun f(x: Int): Int { val y = x; y }`: the continuation takes the index the local had,
/// and the body returns its value as `Any?`.
#[test]
fn a_body_that_does_not_suspend_returns_its_value_through_the_new_signature() {
    let mut ir = IrFile::default();
    let read_parameter = ir.add_expr(IrExpr::GetValue(0));
    let local = ir.add_expr(IrExpr::Variable {
        index: 1,
        ty: Ty::Int,
        init: Some(read_parameter),
        named: true,
    });
    let read_local = ir.add_expr(IrExpr::GetValue(1));
    let body = ir.add_expr(IrExpr::Block {
        stmts: vec![local],
        value: Some(read_local),
    });
    let f = ir.add_fun(function(vec![Ty::Int], Ty::Int, Some(body)));
    ir.suspend_funs.push(f);

    assert_eq!(add_continuation_parameters(&mut ir), Ok(()));

    let declaration = &ir.functions[f as usize];
    assert_eq!(
        declaration.params,
        [Ty::Int, Ty::obj_args_name(wk::continuation(), &[Ty::Int])]
    );
    assert_eq!(declaration.param_checks, [None, None]);
    assert_eq!(declaration.ret, any());
    assert_eq!(ir.suspend_declared_sigs[&f], (vec![Ty::Int], Ty::Int));
    assert!(matches!(
        ir.exprs[local as usize],
        IrExpr::Variable { index: 2, .. }
    ));
    assert!(matches!(ir.exprs[read_local as usize], IrExpr::GetValue(2)));
    assert!(matches!(
        ir.exprs[read_parameter as usize],
        IrExpr::GetValue(0)
    ));
    let IrExpr::Block { stmts, value: None } = &ir.exprs[body as usize] else {
        panic!("the body completes by a return");
    };
    let [first, returned] = stmts.as_slice() else {
        panic!("the local, then the return: {stmts:?}");
    };
    assert_eq!(*first, local);
    let IrExpr::Return(Some(completed)) = ir.exprs[*returned as usize] else {
        panic!("a return");
    };
    assert!(matches!(
        ir.exprs[completed as usize],
        IrExpr::TypeOp { op: IrTypeOp::ImplicitCoercion, arg, type_operand }
            if arg == read_local && type_operand == any()
    ));
}

/// `suspend fun f() { return }` completes with `Unit` from an early return and from its end.
#[test]
fn a_unit_body_completes_with_unit() {
    let mut ir = IrFile::default();
    let early = ir.add_expr(IrExpr::Return(None));
    let flag = ir.add_expr(IrExpr::Const(IrConst::Boolean(true)));
    let guard = ir.add_expr(IrExpr::When {
        branches: vec![(Some(flag), early)],
    });
    let body = ir.add_expr(IrExpr::Block {
        stmts: vec![guard],
        value: None,
    });
    let f = ir.add_fun(function(Vec::new(), Ty::Unit, Some(body)));
    ir.suspend_funs.push(f);

    assert_eq!(add_continuation_parameters(&mut ir), Ok(()));

    let IrExpr::Return(Some(early_unit)) = ir.exprs[early as usize] else {
        panic!("the early return completes with a value");
    };
    assert!(matches!(
        ir.exprs[early_unit as usize],
        IrExpr::UnitInstance
    ));
    let IrExpr::Block { stmts, value: None } = &ir.exprs[body as usize] else {
        panic!("the body completes by a return");
    };
    let [first, returned] = stmts.as_slice() else {
        panic!("the guard, then the return: {stmts:?}");
    };
    assert_eq!(*first, guard);
    let IrExpr::Return(Some(end_unit)) = ir.exprs[*returned as usize] else {
        panic!("a return");
    };
    assert!(matches!(ir.exprs[end_unit as usize], IrExpr::UnitInstance));
}

/// A body that calls a suspend function needs the state machine, and nothing is rewritten.
#[test]
fn a_body_that_suspends_is_declined_before_any_signature_changes() {
    let mut ir = IrFile::default();
    let callee = ir.add_fun(function(Vec::new(), Ty::Int, None));
    let call = ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::Local(callee)),
        dispatch_receiver: None,
        args: Vec::new(),
    });
    let caller = ir.add_fun(function(Vec::new(), Ty::Int, Some(call)));
    ir.suspend_funs.extend([callee, caller]);

    assert_eq!(
        add_continuation_parameters(&mut ir),
        Err(ContinuationDecline::Suspends)
    );
    assert!(ir
        .functions
        .iter()
        .all(|function| function.params.is_empty()));
    assert!(ir.suspend_declared_sigs.is_empty());
}
