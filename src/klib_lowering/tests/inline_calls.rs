//! A call of an inline function in a dependency body is expanded from the function's lowered
//! body, as kotlinc expands it when it links a library compiled before Kotlin 2.4.20.

use super::demo_package::{demo_inlining, demo_suspend_inlining, operator, Body, T};
use super::{base, local_symbol, validated, DependencyBodyUnit};
use crate::ir::{Callee, FunId, IrConst, IrExpr, IrFile};
use crate::metadata::klib_ir::tree::{
    KlibIrBody, KlibIrExprId, KlibIrExprKind, KlibIrFunction, KlibIrStatement,
};
use crate::metadata::klib_ir::{KlibIrSymbol, KlibIrSymbolKind};

/// The functions of `ir` whose body calls `callee`.
fn callers(ir: &IrFile, callee: FunId) -> Vec<FunId> {
    let mut found = Vec::new();
    for (function, declaration) in ir.functions.iter().enumerate() {
        let Some(body) = declaration.body else {
            continue;
        };
        let mut pending = vec![body];
        while let Some(expression) = pending.pop() {
            if let IrExpr::Call { callee: called, .. } = ir.expr(expression) {
                if **called == Callee::Local(callee) {
                    found.push(function as FunId);
                    break;
                }
            }
            crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
        }
    }
    found
}

fn function_named(ir: &IrFile, name: &str) -> FunId {
    let matching = ir
        .functions
        .iter()
        .enumerate()
        .filter(|(_, function)| function.name == name)
        .map(|(id, _)| id as FunId)
        .collect::<Vec<_>>();
    let [function] = matching[..] else {
        panic!("one function `{name}` in {matching:?}")
    };
    function
}

/// The labels of the loops `function`'s body declares.
fn loop_labels(ir: &IrFile, function: FunId) -> Vec<String> {
    let mut labels = Vec::new();
    let mut pending = vec![ir.functions[function as usize]
        .body
        .expect("a lowered body")];
    while let Some(expression) = pending.pop() {
        if let IrExpr::While {
            label: Some(label), ..
        } = ir.expr(expression)
        {
            labels.push(label.clone());
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    labels
}

fn sole_return(body: &mut Body) -> Option<Vec<KlibIrStatement>> {
    let value = body.param(0);
    Some(vec![body.ret(value)])
}

#[test]
fn a_call_of_an_inline_function_is_expanded_from_its_lowered_body() {
    let demo = demo_inlining(
        &[("same", &[T::Int], T::Int), ("caller", &[T::Int], T::Int)],
        &["same"],
        |name, body| match name {
            "same" => sole_return(body),
            _ => {
                let argument = body.param(0);
                let call = body.call("same", vec![argument], T::Int);
                Some(vec![body.ret(call)])
            }
        },
    );
    let mut unit = DependencyBodyUnit::default();
    demo.lower(&mut unit, "caller", &["caller", "same"])
        .expect("the inline call expands");
    let ir = unit.ir();
    validated(ir);
    let same = function_named(ir, "same");
    let caller = function_named(ir, "caller");
    assert!(ir.inline_only_fns.contains(&same));
    assert!(!ir.inline_only_fns.contains(&caller));
    // The function's own body still makes no call, and no emitted body calls it.
    assert_eq!(callers(ir, same), Vec::<FunId>::new());
    // A sole tail return is the expansion's value: no frame loop is left behind.
    assert_eq!(loop_labels(ir, caller), Vec::<String>::new());
    assert_eq!(
        unit.callable_functions()
            .map(|(_, function)| function)
            .collect::<Vec<_>>(),
        vec![caller],
        "an inline function is not exported"
    );
}

#[test]
fn an_inline_body_returning_early_leaves_through_its_expansion_frame() {
    let demo = demo_inlining(
        &[
            ("choose", &[T::Boolean], T::Int),
            ("caller", &[T::Boolean], T::Int),
        ],
        &["choose"],
        |name, body| match name {
            "choose" => {
                let condition = body.param(0);
                let one = body.int(1);
                let early = body.return_(one);
                let conditional = body.branches("IF", vec![(condition, early)], T::Unit);
                let two = body.int(2);
                Some(vec![
                    KlibIrStatement::Expression(conditional),
                    body.ret(two),
                ])
            }
            _ => {
                let argument = body.param(0);
                let call = body.call("choose", vec![argument], T::Int);
                Some(vec![body.ret(call)])
            }
        },
    );
    let mut unit = DependencyBodyUnit::default();
    demo.lower(&mut unit, "caller", &["caller", "choose"])
        .expect("the inline call expands");
    let ir = unit.ir();
    validated(ir);
    let choose = function_named(ir, "choose");
    let caller = function_named(ir, "caller");
    assert_eq!(callers(ir, choose), Vec::<FunId>::new());
    let labels = loop_labels(ir, caller);
    let [label] = labels.as_slice() else {
        panic!("one expansion frame: {labels:?}")
    };
    assert!(label.starts_with("$klib_inline$"), "{label}");
    // Both returns store the frame's result before leaving it.
    let stored = ir
        .exprs
        .iter()
        .filter(|expression| matches!(expression, IrExpr::SetValue { .. }))
        .count();
    assert_eq!(stored, 2);
    assert!(ir
        .exprs
        .iter()
        .any(|expression| matches!(expression, IrExpr::Const(IrConst::Int(2)))));
}

/// The expressions `function`'s body reaches.
fn reached(ir: &IrFile, function: FunId) -> Vec<&IrExpr> {
    let mut reached = Vec::new();
    let mut pending = vec![ir.functions[function as usize]
        .body
        .expect("a lowered body")];
    while let Some(expression) = pending.pop() {
        reached.push(ir.expr(expression));
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    reached
}

/// A `() -> Int` lambda literal whose body `build` makes, given the lambda's own return target.
fn lambda(
    body: &mut Body,
    build: impl FnOnce(&mut Body, &KlibIrSymbol) -> Vec<KlibIrStatement>,
) -> KlibIrExprId {
    body.next_slot += 1;
    let symbol = local_symbol(KlibIrSymbolKind::Function, body.next_slot);
    let statements = build(body, &symbol);
    let return_type = body.ty(T::Int);
    let function = body.arena.push_function(KlibIrFunction {
        base: base(symbol),
        name: "<anonymous>".to_owned(),
        constructor: false,
        type_parameters: Vec::new(),
        dispatch_receiver: None,
        context_parameters: Vec::new(),
        extension_receiver: None,
        regular_parameters: Vec::new(),
        return_type,
        overridden: Vec::new(),
        companion_extension_class: None,
        prepared_inline_file: None,
        body: Some(KlibIrBody::Block(statements)),
    });
    body.expr(
        T::Function0Int,
        KlibIrExprKind::FunctionExpression {
            function,
            origin: Some("LAMBDA".to_owned()),
        },
    )
}

/// `return@lambda value` of the lambda `target`.
fn lambda_return(body: &mut Body, target: &KlibIrSymbol, value: KlibIrExprId) -> KlibIrExprId {
    body.expr(
        T::Nothing,
        KlibIrExprKind::Return {
            target: target.clone(),
            value,
        },
    )
}

/// `block()` of the parameter `index`, a `() -> Int`.
fn invoke(body: &mut Body, index: usize) -> KlibIrExprId {
    let block = body.param(index);
    let invoke = operator(&["kotlin"], Some("Function0"), "invoke", &[]);
    body.call_signature(invoke.0, vec![block], None, T::Int)
}

/// No lambda is created and no function value is invoked where `function` runs.
fn assert_no_function_value(ir: &IrFile, function: FunId) {
    let values = reached(ir, function)
        .into_iter()
        .filter(|expression| {
            matches!(
                expression,
                IrExpr::Lambda { .. } | IrExpr::InvokeFunction { .. }
            )
        })
        .count();
    assert_eq!(values, 0, "the lambda is expanded, not created");
}

#[test]
fn a_lambda_argument_is_expanded_where_the_inline_body_invokes_it() {
    let demo = demo_inlining(
        &[
            ("run", &[T::Function0Int], T::Int),
            ("caller", &[T::Int], T::Int),
        ],
        &["run"],
        |name, body| match name {
            "run" => {
                let invocation = invoke(body, 0);
                Some(vec![body.ret(invocation)])
            }
            _ => {
                let block = lambda(body, |body, target| {
                    let captured = body.param(0);
                    vec![KlibIrStatement::Expression(lambda_return(
                        body, target, captured,
                    ))]
                });
                let call = body.call("run", vec![block], T::Int);
                Some(vec![body.ret(call)])
            }
        },
    );
    let mut unit = DependencyBodyUnit::default();
    demo.lower(&mut unit, "caller", &["caller", "run"])
        .expect("the inline call and its lambda expand");
    let ir = unit.ir();
    validated(ir);
    let run = function_named(ir, "run");
    let caller = function_named(ir, "caller");
    assert_eq!(callers(ir, run), Vec::<FunId>::new());
    assert_no_function_value(ir, caller);
    // Both bodies end in a sole tail return, so no frame is left behind.
    assert_eq!(loop_labels(ir, caller), Vec::<String>::new());
    assert_eq!(
        unit.callable_functions()
            .map(|(_, function)| function)
            .collect::<Vec<_>>(),
        vec![caller],
        "neither the inline function nor the lambda is exported"
    );
}

#[test]
fn a_lambda_invoked_twice_is_expanded_at_each_invocation_with_its_own_frame() {
    let demo = demo_inlining(
        &[
            ("twice", &[T::Function0Int], T::Int),
            ("caller", &[T::Boolean], T::Int),
        ],
        &["twice"],
        |name, body| match name {
            "twice" => {
                let first = invoke(body, 0);
                let second = invoke(body, 0);
                Some(vec![KlibIrStatement::Expression(first), body.ret(second)])
            }
            _ => {
                let block = lambda(body, |body, target| {
                    let condition = body.param(0);
                    let one = body.int(1);
                    let early = lambda_return(body, target, one);
                    let conditional = body.branches("IF", vec![(condition, early)], T::Unit);
                    let two = body.int(2);
                    vec![
                        KlibIrStatement::Expression(conditional),
                        KlibIrStatement::Expression(lambda_return(body, target, two)),
                    ]
                });
                let call = body.call("twice", vec![block], T::Int);
                Some(vec![body.ret(call)])
            }
        },
    );
    let mut unit = DependencyBodyUnit::default();
    demo.lower(&mut unit, "caller", &["caller", "twice"])
        .expect("each invocation expands");
    let ir = unit.ir();
    validated(ir);
    let caller = function_named(ir, "caller");
    assert_no_function_value(ir, caller);
    let labels = loop_labels(ir, caller);
    assert_eq!(labels.len(), 2, "one frame for each invocation: {labels:?}");
    assert!(labels
        .iter()
        .all(|label| label.starts_with("$klib_inline$")));
    assert_ne!(labels[0], labels[1]);
}

#[test]
fn a_lambda_passed_on_to_another_inline_function_is_expanded_there() {
    let demo = demo_inlining(
        &[
            ("run", &[T::Function0Int], T::Int),
            ("outer", &[T::Function0Int], T::Int),
            ("caller", &[T::Int], T::Int),
        ],
        &["run", "outer"],
        |name, body| match name {
            "run" => {
                let invocation = invoke(body, 0);
                Some(vec![body.ret(invocation)])
            }
            "outer" => {
                let block = body.param(0);
                let call = body.call("run", vec![block], T::Int);
                Some(vec![body.ret(call)])
            }
            _ => {
                let block = lambda(body, |body, target| {
                    let captured = body.param(0);
                    vec![KlibIrStatement::Expression(lambda_return(
                        body, target, captured,
                    ))]
                });
                let call = body.call("outer", vec![block], T::Int);
                Some(vec![body.ret(call)])
            }
        },
    );
    let mut unit = DependencyBodyUnit::default();
    demo.lower(&mut unit, "caller", &["caller", "outer", "run"])
        .expect("the lambda expands inside the nested expansion");
    let ir = unit.ir();
    validated(ir);
    let caller = function_named(ir, "caller");
    assert_no_function_value(ir, caller);
    assert_eq!(callers(ir, function_named(ir, "run")), Vec::<FunId>::new());
    assert_eq!(
        callers(ir, function_named(ir, "outer")),
        Vec::<FunId>::new()
    );
}

#[test]
fn a_lambda_parameter_the_inline_body_keeps_as_a_value_declines() {
    let demo = demo_inlining(
        &[
            ("keep", &[T::Function0Int], T::Function0Int),
            ("caller", &[T::Int], T::Function0Int),
        ],
        &["keep"],
        |name, body| match name {
            "keep" => {
                let block = body.param(0);
                Some(vec![body.ret(block)])
            }
            _ => {
                let block = lambda(body, |body, target| {
                    let captured = body.param(0);
                    vec![KlibIrStatement::Expression(lambda_return(
                        body, target, captured,
                    ))]
                });
                let call = body.call("keep", vec![block], T::Function0Int);
                Some(vec![body.ret(call)])
            }
        },
    );
    let mut unit = DependencyBodyUnit::default();
    let decline = demo
        .lower(&mut unit, "caller", &["caller", "keep"])
        .expect_err("the lambda would have to be created");
    assert_eq!(
        decline.reason(),
        &crate::klib_lowering::KlibBodyDeclineReason::UnsupportedOperation(
            "an inline function's lambda parameter used other than by invoking it"
        )
    );
}

#[test]
fn a_call_of_a_suspend_inline_function_without_a_prepared_copy_declines() {
    let demo = demo_suspend_inlining(
        &[
            ("suspends", &[T::Int], T::Int),
            ("caller", &[T::Int], T::Int),
        ],
        &["suspends"],
        |name, body| match name {
            "suspends" => sole_return(body),
            _ => {
                let argument = body.param(0);
                let call = body.call("suspends", vec![argument], T::Int);
                Some(vec![body.ret(call)])
            }
        },
    );
    let mut unit = DependencyBodyUnit::default();
    let decline = demo
        .lower(&mut unit, "caller", &["caller", "suspends"])
        .expect_err("the unlowered suspension would be expanded");
    assert_eq!(
        decline.reason(),
        &crate::klib_lowering::KlibBodyDeclineReason::UnsupportedOperation(
            "a call of a suspend inline function without a prepared copy"
        )
    );
    assert!(unit.ir().inline_fns.is_empty());
    assert!(unit.ir().suspend_funs.is_empty());
}

#[test]
fn a_call_of_an_inline_function_leaving_an_argument_to_its_default_declines() {
    let demo = demo_inlining(
        &[
            ("g", &[T::Int, T::Int], T::Int),
            ("caller", &[T::Int], T::Int),
        ],
        &["g"],
        |name, body| match name {
            "g" => {
                let earlier = body.param(0);
                body.default(1, earlier);
                let result = body.param(1);
                Some(vec![body.ret(result)])
            }
            _ => {
                let argument = body.param(0);
                let call = body.call_omitting("g", vec![Some(argument), None], T::Int);
                Some(vec![body.ret(call)])
            }
        },
    );
    let mut unit = DependencyBodyUnit::default();
    let decline = demo
        .lower(&mut unit, "caller", &["caller", "g"])
        .expect_err("the defaulted call would escape expansion");
    assert_eq!(
        decline.reason(),
        &crate::klib_lowering::KlibBodyDeclineReason::UnsupportedOperation(
            "a call of an inline function that leaves arguments to its defaults"
        )
    );
    assert!(unit.ir().inline_fns.is_empty());
    assert!(unit.ir().inline_only_fns.is_empty());
}

#[test]
fn a_declined_inline_expansion_leaves_its_function_id_free_for_an_ordinary_callable() {
    let demo = demo_inlining(
        &[
            ("loops", &[T::Int], T::Int),
            ("caller", &[T::Int], T::Int),
            ("plain", &[T::Int], T::Int),
        ],
        &["loops"],
        |name, body| match name {
            "loops" => {
                let argument = body.param(0);
                let call = body.call("loops", vec![argument], T::Int);
                Some(vec![body.ret(call)])
            }
            "caller" => {
                let argument = body.param(0);
                let call = body.call("loops", vec![argument], T::Int);
                Some(vec![body.ret(call)])
            }
            _ => sole_return(body),
        },
    );
    let mut unit = DependencyBodyUnit::default();
    demo.lower(&mut unit, "caller", &["caller", "loops"])
        .expect_err("a recursive inline function cannot be expanded");
    assert!(unit.ir().inline_fns.is_empty());
    assert!(unit.ir().inline_only_fns.is_empty());
    // The retry reuses the function id the declined lowering declared.
    let plain = demo
        .lower(&mut unit, "plain", &["plain"])
        .expect("an ordinary callable lowers");
    validated(unit.ir());
    assert!(!unit.ir().inline_only_fns.contains(&plain));
    assert_eq!(
        unit.callable_functions()
            .map(|(_, function)| function)
            .collect::<Vec<_>>(),
        vec![plain],
        "the ordinary callable is still emitted"
    );
}

#[test]
fn a_generic_inline_function_is_expanded_at_the_call_s_type_arguments() {
    let demo = demo_inlining(
        &[("id", &[T::P], T::P), ("caller", &[T::String], T::String)],
        &["id"],
        |name, body| match name {
            "id" => sole_return(body),
            _ => {
                let argument = body.param(0);
                let call = body.generic_call("id", vec![argument], T::String, T::String);
                Some(vec![body.ret(call)])
            }
        },
    );
    let mut unit = DependencyBodyUnit::default();
    demo.lower(&mut unit, "caller", &["caller", "id"])
        .expect("the generic inline call expands");
    let ir = unit.ir();
    validated(ir);
    let id = function_named(ir, "id");
    let caller = function_named(ir, "caller");
    assert_eq!(callers(ir, id), Vec::<FunId>::new());
    // The provider mints the formal's identity; the binding is keyed by it, not by its spelling.
    let crate::types::TyKind::TyParam(formal, _) = ir.functions[id as usize].params[0].kind()
    else {
        panic!("the declared parameter names the type parameter")
    };
    assert_eq!(
        ir.inline_call_type_arguments.values().collect::<Vec<_>>(),
        vec![&vec![(
            formal.as_str().to_owned(),
            crate::types::Ty::String
        )]],
        "the call binds the function's type parameter by its selected identity"
    );
    assert_eq!(
        ir.signatures[&id].type_params[0].semantic_name,
        formal.as_str()
    );
    // The copied body names no type parameter any more: its operand is held at the argument type.
    let unspecialized = reached(ir, caller)
        .into_iter()
        .filter(|expression| {
            matches!(expression, IrExpr::Variable { ty, .. }
                if matches!(ty.kind(), crate::types::TyKind::TyParam(..)))
        })
        .count();
    assert_eq!(unspecialized, 0);
    assert_eq!(
        unit.callable_functions()
            .map(|(_, function)| function)
            .collect::<Vec<_>>(),
        vec![caller]
    );
}

#[test]
fn a_nested_generic_inline_call_binds_the_outer_formals_to_the_calls_type_arguments() {
    // caller(String) -> outer<P>(x) -> id<P>(x): the inner call binds id's formal to outer's formal,
    // which the outer splice must specialize to String before the inner call is expanded.
    let demo = demo_inlining(
        &[
            ("id", &[T::P], T::P),
            ("outer", &[T::P], T::P),
            ("caller", &[T::String], T::String),
        ],
        &["id", "outer"],
        |name, body| match name {
            "id" => sole_return(body),
            "outer" => {
                let argument = body.param(0);
                let call = body.generic_call("id", vec![argument], T::P, T::P);
                Some(vec![body.ret(call)])
            }
            _ => {
                let argument = body.param(0);
                let call = body.generic_call("outer", vec![argument], T::String, T::String);
                Some(vec![body.ret(call)])
            }
        },
    );
    let mut unit = DependencyBodyUnit::default();
    demo.lower(&mut unit, "caller", &["caller", "outer", "id"])
        .expect("the nested generic inline calls expand");
    let ir = unit.ir();
    validated(ir);
    let caller = function_named(ir, "caller");
    let opaque = |ty: &crate::types::Ty| matches!(ty.kind(), crate::types::TyKind::TyParam(..));
    let unspecialized = reached(ir, caller)
        .into_iter()
        .filter(|expression| matches!(expression, IrExpr::Variable { ty, .. } if opaque(ty)))
        .count();
    assert_eq!(unspecialized, 0, "no copied operand keeps an opaque formal");
    let bound = ir
        .inline_call_type_arguments
        .values()
        .flatten()
        .map(|(_, ty)| *ty)
        .collect::<Vec<_>>();
    // `outer`'s own lowered body keeps the one binding written in terms of its formal; the copies in
    // `caller` bind String, including the nested call's.
    let (strings, opaque_bindings): (Vec<_>, Vec<_>) = bound
        .iter()
        .partition(|ty| **ty == crate::types::Ty::String);
    assert_eq!(
        strings.len(),
        2,
        "the call and its nested call bind String: {bound:?}"
    );
    assert_eq!(
        opaque_bindings.len(),
        1,
        "only the inline callee's own body is opaque: {bound:?}"
    );
    assert!(opaque_bindings.iter().all(opaque));
}

#[test]
fn a_declined_generic_inline_expansion_leaves_no_signature_on_a_reused_function_id() {
    let demo = demo_inlining(
        &[
            ("loops", &[T::P], T::P),
            ("caller", &[T::String], T::String),
            ("plain", &[T::Int], T::Int),
        ],
        &["loops"],
        |name, body| match name {
            "loops" | "caller" => {
                let argument = body.param(0);
                let ty = if name == "loops" { T::P } else { T::String };
                let call = body.generic_call("loops", vec![argument], T::String, ty);
                Some(vec![body.ret(call)])
            }
            _ => sole_return(body),
        },
    );
    let mut unit = DependencyBodyUnit::default();
    demo.lower(&mut unit, "caller", &["caller", "loops"])
        .expect_err("a recursive inline function cannot be expanded");
    assert!(unit.ir().signatures.is_empty());
    // The retry reuses the function id the declined lowering declared.
    let plain = demo
        .lower(&mut unit, "plain", &["plain"])
        .expect("an ordinary callable lowers");
    validated(unit.ir());
    assert!(!unit.ir().signatures.contains_key(&plain));
    assert_eq!(
        unit.callable_functions()
            .map(|(_, function)| function)
            .collect::<Vec<_>>(),
        vec![plain],
        "the ordinary callable is still emitted"
    );
}
