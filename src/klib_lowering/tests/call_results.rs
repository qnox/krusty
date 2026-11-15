//! The declared result a call of a function of the same unit keeps, as a source call of a
//! function of its own file does (`IrFile::call_declared_ret`).

use super::super::*;
use super::demo_package::{demo, T};
use super::validated;
use crate::ir::{Callee, IrExpr, IrFile, IrTypeOp};

/// The declared result each call of the function `callee` keeps in `ir`, and whether its value
/// reaches its use through a declaration-result coercion.
fn declared_results(ir: &IrFile, callee: &str) -> Vec<(Option<crate::types::Ty>, bool)> {
    let coerced = |call: u32| {
        ir.declaration_result_coercions.iter().any(|&coercion| {
            matches!(
                ir.exprs[coercion as usize],
                IrExpr::TypeOp { op: IrTypeOp::ImplicitCoercion, arg, .. } if arg == call
            )
        })
    };
    ir.exprs
        .iter()
        .enumerate()
        .filter_map(|(index, expression)| match expression {
            IrExpr::Call { callee: target, .. }
                if matches!(
                    &**target,
                    Callee::Local(function)
                        if ir.functions[*function as usize].name == callee
                ) =>
            {
                let call = index as u32;
                Some((ir.call_declared_ret.get(&call).copied(), coerced(call)))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn a_generic_local_call_keeps_its_declared_result_as_a_source_call_does() {
    // `fun <P> id(a: P): P` called as `id(a)` at `P = String`: the call's declared result is
    // `P`, and its `String` value reaches the return through the declaration-result coercion.
    let demo = demo(
        &[("id", &[T::P], T::P), ("f", &[T::String], T::String)],
        |name, body| match name {
            "f" => {
                let a = body.param(0);
                let call = body.generic_call("id", vec![a], T::String, T::String);
                Some(vec![body.ret(call)])
            }
            _ => {
                let a = body.param(0);
                Some(vec![body.ret(a)])
            }
        },
    );
    let facts = demo.callees(&["f", "id"]);
    let mut unit = DependencyBodyUnit::default();
    unit.lower_function(demo.callable("f"), &demo.bodies, &facts)
        .unwrap_or_else(|decline| panic!("{decline}"));
    validated(unit.ir());
    let id = unit
        .ir()
        .functions
        .iter()
        .find(|function| function.name == "id")
        .expect("`id` is linked into the unit");
    assert_eq!(declared_results(unit.ir(), "id"), [(Some(id.ret), true)],);

    let source = crate::fir_lower::tests::lower_single_source_with_platform(
        "fun <P> id(a: P): P = a\nfun f(a: String): String = id(a)\n",
        "KlibBodyEquivalent",
        Box::new(crate::libraries::EmptySymbolSource),
    );
    let source_id = source
        .functions
        .iter()
        .find(|function| function.name == "id")
        .expect("source `id`");
    assert_eq!(
        declared_results(&source, "id"),
        [(Some(source_id.ret), true)],
    );
}
