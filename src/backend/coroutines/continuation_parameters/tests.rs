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

/// A call to a suspend function that completes in place passes a continuation and is read at the
/// declared result. The callee's expression body stays its own node.
#[test]
fn a_call_to_a_suspend_function_that_completes_in_place_passes_a_continuation() {
    let mut ir = IrFile::default();
    let seven = ir.add_expr(IrExpr::Const(IrConst::Int(7)));
    let leaf = ir.add_fun(function(Vec::new(), Ty::Int, Some(seven)));
    let call = ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::Local(leaf)),
        dispatch_receiver: None,
        args: Vec::new(),
    });
    let caller = ir.add_fun(function(Vec::new(), Ty::Int, Some(call)));
    ir.suspend_funs.extend([leaf, caller]);

    assert_eq!(add_continuation_parameters(&mut ir), Ok(()));

    assert_eq!(ir.functions[leaf as usize].params.len(), 1);
    assert_eq!(ir.functions[leaf as usize].ret, any());
    assert!(matches!(
        ir.exprs[seven as usize],
        IrExpr::Const(IrConst::Int(7))
    ));
    let IrExpr::TypeOp {
        op: IrTypeOp::ImplicitCoercion,
        arg: invoked,
        type_operand,
    } = ir.exprs[call as usize]
    else {
        panic!("the call is read at its declared result");
    };
    assert_eq!(type_operand, Ty::Int);
    let IrExpr::Call { args, .. } = &ir.exprs[invoked as usize] else {
        panic!("the call");
    };
    let [continuation] = args.as_slice() else {
        panic!("the continuation is the only argument: {args:?}");
    };
    assert!(matches!(
        ir.exprs[*continuation as usize],
        IrExpr::Const(IrConst::Null)
    ));
}

/// A call to a suspend function with no body needs the state machine, and nothing is rewritten.
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

/// Whether the expression graph under `root` has no cycle.
fn acyclic(ir: &IrFile, root: ExprId) -> bool {
    fn visit(
        ir: &IrFile,
        node: ExprId,
        open: &mut HashSet<ExprId>,
        done: &mut HashSet<ExprId>,
    ) -> bool {
        if done.contains(&node) {
            return true;
        }
        if !open.insert(node) {
            return false;
        }
        let mut children = Vec::new();
        for_each_child(&ir.exprs, node, &mut |child| children.push(child));
        let ok = children
            .into_iter()
            .all(|child| visit(ir, child, open, done));
        open.remove(&node);
        done.insert(node);
        ok
    }
    visit(ir, root, &mut HashSet::default(), &mut HashSet::default())
}

/// `suspend fun f(): Int = 7`: an expression body stays its own node, returned by a new block.
#[test]
fn an_expression_body_is_returned_by_a_new_block_that_keeps_it() {
    let mut ir = IrFile::default();
    let seven = ir.add_expr(IrExpr::Const(IrConst::Int(7)));
    let f = ir.add_fun(function(Vec::new(), Ty::Int, Some(seven)));
    ir.suspend_funs.push(f);

    assert_eq!(add_continuation_parameters(&mut ir), Ok(()));

    assert!(matches!(
        ir.exprs[seven as usize],
        IrExpr::Const(IrConst::Int(7))
    ));
    let body = ir.functions[f as usize].body.expect("a body");
    assert_ne!(body, seven);
    assert!(acyclic(&ir, body));
    let IrExpr::Block { stmts, value: None } = &ir.exprs[body as usize] else {
        panic!("the body completes by a return");
    };
    let [returned] = stmts.as_slice() else {
        panic!("one return: {stmts:?}");
    };
    let IrExpr::Return(Some(completed)) = ir.exprs[*returned as usize] else {
        panic!("a return");
    };
    assert!(matches!(
        ir.exprs[completed as usize],
        IrExpr::TypeOp { op: IrTypeOp::ImplicitCoercion, arg, type_operand }
            if arg == seven && type_operand == any()
    ));
}

/// A non-`Unit` `tailrec` body is a loop whose value leaves by `return`, then `break`. There is no
/// result after the loop, and the return's value stays its own node.
#[test]
fn a_non_unit_loop_completes_through_the_return_it_already_has() {
    let mut ir = IrFile::default();
    let seven = ir.add_expr(IrExpr::Const(IrConst::Int(7)));
    let returned = ir.add_expr(IrExpr::Return(Some(seven)));
    let exit = ir.add_expr(IrExpr::Break { label: None });
    let loop_body = ir.add_expr(IrExpr::Block {
        stmts: vec![returned, exit],
        value: None,
    });
    let condition = ir.add_expr(IrExpr::Const(IrConst::Boolean(true)));
    let loop_expression = ir.add_expr(IrExpr::While {
        cond: condition,
        body: loop_body,
        update: None,
        post_test: true,
        label: None,
        inline_return_frame: false,
    });
    let body = ir.add_expr(IrExpr::Block {
        stmts: vec![loop_expression],
        value: None,
    });
    let f = ir.add_fun(function(Vec::new(), Ty::Int, Some(body)));
    ir.suspend_funs.push(f);

    assert_eq!(add_continuation_parameters(&mut ir), Ok(()));

    assert_eq!(ir.functions[f as usize].body, Some(body));
    assert!(acyclic(&ir, body));
    let IrExpr::Block { stmts, value: None } = &ir.exprs[body as usize] else {
        panic!("the loop stays the body");
    };
    assert_eq!(stmts.as_slice(), [loop_expression]);
    let IrExpr::Return(Some(completed)) = ir.exprs[returned as usize] else {
        panic!("the return completes with a value");
    };
    assert!(matches!(
        ir.exprs[completed as usize],
        IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg,
            type_operand,
        } if arg == seven && type_operand == any()
    ));
    assert!(matches!(
        ir.exprs[seven as usize],
        IrExpr::Const(IrConst::Int(7))
    ));
}

/// `suspend fun f(): Unit = g()`: the expression runs for effect, then the body completes with
/// `Unit`.
#[test]
fn a_unit_expression_body_runs_and_completes_with_unit() {
    let mut ir = IrFile::default();
    let g = ir.add_fun(function(Vec::new(), Ty::Unit, None));
    let call = ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::Local(g)),
        dispatch_receiver: None,
        args: Vec::new(),
    });
    let f = ir.add_fun(function(Vec::new(), Ty::Unit, Some(call)));
    ir.suspend_funs.push(f);

    assert_eq!(add_continuation_parameters(&mut ir), Ok(()));

    let body = ir.functions[f as usize].body.expect("a body");
    assert_ne!(body, call);
    assert!(acyclic(&ir, body));
    let IrExpr::Block { stmts, value: None } = &ir.exprs[body as usize] else {
        panic!("the body completes by a return");
    };
    let [returned] = stmts.as_slice() else {
        panic!("one return: {stmts:?}");
    };
    let IrExpr::Return(Some(completed)) = ir.exprs[*returned as usize] else {
        panic!("a return");
    };
    let IrExpr::Block {
        stmts: effects,
        value: Some(unit),
    } = &ir.exprs[completed as usize]
    else {
        panic!("the call, then `Unit`");
    };
    assert_eq!(effects.as_slice(), [call]);
    assert!(matches!(ir.exprs[*unit as usize], IrExpr::UnitInstance));
}

/// `open class Base { open suspend fun value(): Int = 7 }`, its `override` in `Derived`, and a
/// caller `suspend fun read(base: Base) = base.value()`. The method call names `Base`'s slot,
/// with the open method in it as its static function and the override as a second
/// implementation. Answers the file and the call.
fn method_call_through_base(open_method: bool, open_class: bool) -> (IrFile, ExprId) {
    let mut ir = IrFile::default();
    let base_value = {
        let seven = ir.add_expr(IrExpr::Const(IrConst::Int(7)));
        let mut method = function(Vec::new(), Ty::Int, Some(seven));
        method.is_static = false;
        method.dispatch_receiver = Some("Base".into());
        ir.add_fun(method)
    };
    let derived_value = {
        let eight = ir.add_expr(IrExpr::Const(IrConst::Int(8)));
        let mut method = function(Vec::new(), Ty::Int, Some(eight));
        method.is_static = false;
        method.dispatch_receiver = Some("Derived".into());
        ir.add_fun(method)
    };
    let mut base = crate::ir::test_support::blank_class("Base");
    base.is_open = open_class;
    base.methods.push(base_value);
    let base = ir.add_class(base);
    let mut derived = crate::ir::test_support::blank_class("Derived");
    derived.superclass = "Base".into();
    derived.methods.push(derived_value);
    ir.add_class(derived);
    if open_method {
        ir.open_methods.insert(base_value);
    }
    let receiver = ir.add_expr(IrExpr::Const(IrConst::Null));
    let call = ir.add_expr(IrExpr::MethodCall {
        class: base,
        index: 0,
        receiver,
        args: Vec::new(),
    });
    let caller = ir.add_fun(function(
        vec![Ty::obj_name("Base".into())],
        Ty::Int,
        Some(call),
    ));
    ir.suspend_funs.extend([base_value, derived_value, caller]);
    (ir, call)
}

/// An open suspend method of an open class may run as any override, including one in another
/// module; its slot does not prove the call completes in place, and the file is declined.
#[test]
fn a_call_to_an_overridable_suspend_method_is_declined() {
    let (mut ir, call) = method_call_through_base(true, true);
    assert_eq!(
        add_continuation_parameters(&mut ir),
        Err(ContinuationDecline::Suspends)
    );
    let IrExpr::MethodCall { args, .. } = &ir.exprs[call as usize] else {
        panic!("the call is left as it was");
    };
    assert!(args.is_empty());
    assert!(ir.suspend_declared_sigs.is_empty());
}

/// The same slot when nothing can replace its function: a final method of an open class, and an
/// open method of a class that can have no subclass. The call passes a continuation.
#[test]
fn a_call_to_a_closed_suspend_method_passes_a_continuation() {
    for (open_method, open_class) in [(false, true), (true, false)] {
        let (mut ir, call) = method_call_through_base(open_method, open_class);

        assert_eq!(
            add_continuation_parameters(&mut ir),
            Ok(()),
            "{open_method} {open_class}"
        );

        let IrExpr::TypeOp { arg: invoked, .. } = ir.exprs[call as usize] else {
            panic!("the call is read at its declared result");
        };
        let IrExpr::MethodCall { args, .. } = &ir.exprs[invoked as usize] else {
            panic!("the method call");
        };
        let [Some(continuation)] = args.as_slice() else {
            panic!("the continuation is the only argument: {args:?}");
        };
        assert!(matches!(
            ir.exprs[*continuation as usize],
            IrExpr::Const(IrConst::Null)
        ));
    }
}
