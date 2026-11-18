//! A call of an inline function in a dependency body is expanded from the function's lowered
//! body, as kotlinc expands it when it links a library compiled before Kotlin 2.4.20.

use super::demo_package::{demo_inlining, Body, T};
use super::{validated, DependencyBodyUnit};
use crate::ir::{Callee, FunId, IrConst, IrExpr, IrFile};
use crate::metadata::klib_ir::tree::KlibIrStatement;

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
