//! Checked-FIR contracts for inferred reified intersections.

use super::test_support::checked_function_body;
use super::*;

#[test]
fn flow_intersection_type_argument_preserves_all_bounds_in_checked_fir() {
    let (body, index) = checked_function_body(
        "// LANGUAGE: -ProhibitIntersectionReifiedTypeParameter\n\
         interface A\n\
         interface B\n\
         inline fun <reified K> select(x: K): K where K : A, K : B = x\n\
         @Suppress(\"TYPE_INTERSECTION_AS_REIFIED_WARNING\")\n\
         fun test(value: Any) { if (value is A && value is B) select(value) }\n",
        "test",
    );
    let call = (0..body.expression_count())
        .find_map(|raw| {
            let FirExprKind::Call(call) = &body.expr(FirExprId::from_raw(raw as u32))?.kind else {
                return None;
            };
            let target = call.target.module()?;
            (index.callable_name(target) == Some("select")).then_some(call)
        })
        .expect("intersection-constrained generic call");
    let [substitution] = call.substitutions.as_ref() else {
        panic!("select must publish its inferred reified type argument")
    };
    assert_eq!(substitution.value.get(), Ty::obj("A"));
    assert_eq!(
        substitution
            .additional_bounds
            .iter()
            .map(|bound| bound.get())
            .collect::<Vec<_>>(),
        vec![Ty::obj("B")]
    );
    let [FirCallArgument::Expression {
        conversion:
            Some(FirConversion {
                kind: FirConversionKind::SmartCast { to },
                ..
            }),
        ..
    }] = call.arguments.as_ref()
    else {
        panic!("the argument must retain its primary intersection projection")
    };
    assert_eq!(to.get(), Ty::obj("A"));
}

#[test]
fn reified_intersection_argument_records_its_common_supertype() {
    let (body, index) = checked_function_body(
        "// LANGUAGE: -ProhibitIntersectionReifiedTypeParameter\n\
         interface X\n\
         interface Y\n\
         object A : X, Y\n\
         object B : X, Y\n\
         fun <T> sel(left: T, right: T): T = left\n\
         inline fun <reified T> T.keep(): T = this\n\
         @Suppress(\"TYPE_INTERSECTION_AS_REIFIED_WARNING\")\n\
         fun run() { sel(A, B).keep() }\n",
        "run",
    );
    let call = (0..body.expression_count())
        .find_map(|raw| {
            let FirExprKind::Call(call) = &body.expr(FirExprId::from_raw(raw as u32))?.kind else {
                return None;
            };
            let target = call.target.module()?;
            (index.callable_name(target) == Some("keep")).then_some(call)
        })
        .expect("reified extension call");
    let [substitution] = call.substitutions.as_ref() else {
        panic!("keep must publish its reified type argument")
    };
    assert!(substitution.reified);
    assert!(
        matches!(substitution.value.get(), Ty::Intersection(parts) if parts.len() == 2),
        "the semantic argument stays the intersection, got {:?}",
        substitution.value.get()
    );
    assert_eq!(substitution.reified_runtime.get(), Ty::obj("kotlin/Any"));
}
