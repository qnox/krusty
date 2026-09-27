//! A lambda's SAM target carries the abstract method the checker selected, and says whether the
//! conversion wraps an existing function value. A backend reads both; it never finds the method by
//! its name or recognizes a wrapper by its generated spelling. Which method is selected is the
//! checker's decision (`fir::body_check::lambda_tests`); these tests show lowering carries it.

use super::tests::lower_single_source;
use crate::fir::{FirSamMethod, ResolvedFunctionOverrideTarget};
use crate::ir::{IrExpr, IrFile, IrSamTarget};
use crate::types::{type_name, Ty};

/// Every SAM target in the file, in expression order.
fn sam_targets(ir: &IrFile) -> Vec<&IrSamTarget> {
    ir.exprs
        .iter()
        .filter_map(|expression| match expression {
            IrExpr::Lambda { sam: Some(sam), .. } => Some(sam),
            _ => None,
        })
        .collect()
}

/// The checked callable of the member `name(parameters)` that `owner` declares in this file.
fn member(ir: &IrFile, owner: &str, name: &str, parameters: &[Ty]) -> FirSamMethod {
    let owner = type_name(owner);
    let matches = ir
        .checked_callable_functions
        .iter()
        .filter(|(_, function)| {
            let function = &ir.functions[**function as usize];
            function.dispatch_receiver == Some(owner)
                && function.name == name
                && function.params == parameters
        })
        .map(|(callable, _)| {
            FirSamMethod::Declared(ResolvedFunctionOverrideTarget::Module(*callable))
        })
        .collect::<Vec<_>>();
    let [target] = matches[..] else {
        panic!("one {name}{parameters:?} in {owner:?}, found {matches:?}")
    };
    target
}

const PICK: &str = "fun interface Pick {
    fun choose(value: Int): Int
    fun choose(value: String): Int = value.length
}
fun pick(p: Pick): Int = p.choose(2)
";

#[test]
fn a_lambda_carries_the_selected_method_and_implements_it_itself() {
    let ir = lower_single_source(
        &format!("{PICK}fun direct(): Int = pick {{ it + 1 }}\n"),
        "Sam",
    );
    let [target] = sam_targets(&ir)[..] else {
        panic!("one SAM conversion")
    };
    assert_eq!(
        (
            target.classifier,
            target.method_target,
            target.wraps_function_value,
            target.function_adapter
        ),
        (
            type_name("Pick"),
            member(&ir, "Pick", "choose", &[Ty::Int]),
            false,
            false
        )
    );
}

#[test]
fn a_function_value_conversion_wraps_the_value_and_carries_the_same_method() {
    let ir = lower_single_source(
        &format!("{PICK}fun wrapped(g: (Int) -> Int): Int = pick(Pick(g))\n"),
        "Sam",
    );
    let [target] = sam_targets(&ir)[..] else {
        panic!("one SAM conversion")
    };
    assert_eq!(
        (target.method_target, target.wraps_function_value),
        (member(&ir, "Pick", "choose", &[Ty::Int]), true)
    );
}

#[test]
fn a_function_type_invoke_target_survives_lowering() {
    let ir = lower_single_source(
        "fun interface Task : () -> Unit\nfun task(): Task = Task { }\n",
        "Sam",
    );
    let [target] = sam_targets(&ir)[..] else {
        panic!("one SAM conversion")
    };
    assert_eq!(
        (target.method_target, target.wraps_function_value),
        (FirSamMethod::FunctionTypeInvoke, false)
    );
}
