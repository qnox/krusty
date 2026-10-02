//! A generic call instantiated only at `Nothing` stays `Nothing` for the checked expression.
//!
//! An expected result that is a valid type argument (`select(Context<Any>()): String`) fixes
//! that argument, so the call returns it. An argument that admits only bottom
//! (`Context<out T>` passed to `Context<in U>`) does not: the call's type is `Nothing` even
//! when a safe call or elvis returns it as an outer type parameter. Codegen then throws
//! `KotlinNothingValueException` instead of leaking the erased value.

use super::test_support::checked_function_body;
use super::*;
use crate::types::Ty;

fn call_report(body: &FirBody, index: &ResolvedModuleIndex) -> String {
    let mut lines = Vec::new();
    for raw in 0..body.expression_count() {
        let expression = body
            .expr(FirExprId::from_raw(raw as u32))
            .expect("expression index");
        let FirExprKind::Call(call) = &expression.kind else {
            continue;
        };
        let name = match &call.target {
            FirCallTarget::Module(target) => index
                .callable(*target)
                .and_then(|callable| index.callable_name(callable.id))
                .unwrap_or("?")
                .to_string(),
            FirCallTarget::External { .. } => "external".to_string(),
            FirCallTarget::Intrinsic { operation, .. } => format!("intrinsic:{operation:?}"),
            FirCallTarget::Classifier { .. } => "classifier".to_string(),
            FirCallTarget::Super { name, .. } => name.clone(),
        };
        let substitutions = call
            .substitutions
            .iter()
            .map(|substitution| format!("{:?}", substitution.value.get()))
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!(
            "{name}: ty={:?} subst=[{substitutions}]",
            expression.ty.get()
        ));
    }
    lines.join("\n")
}

fn calls_named<'a>(body: &'a FirBody, index: &ResolvedModuleIndex, name: &str) -> Vec<&'a FirExpr> {
    (0..body.expression_count())
        .filter_map(|raw| body.expr(FirExprId::from_raw(raw as u32)))
        .filter(|expression| {
            let FirExprKind::Call(call) = &expression.kind else {
                return false;
            };
            let FirCallTarget::Module(target) = &call.target else {
                return false;
            };
            index
                .callable(*target)
                .and_then(|callable| index.callable_name(callable.id))
                == Some(name)
        })
        .collect()
}

#[test]
fn a_safe_call_of_an_out_projected_argument_stays_nothing() {
    let (body, index) = checked_function_body(
        "class Context<T>\n\
         fun <T> something(): T = null as T\n\
         fun <T> Any.decodeIn(typeFrom: Context<in T>): T = something()\n\
         fun <T> Any?.decodeOut(typeFrom: Context<out T>): T =\n\
             this?.decodeIn(typeFrom) ?: null!!\n",
        "decodeOut",
    );
    let decode_in = calls_named(&body, &index, "decodeIn");
    assert_eq!(decode_in.len(), 1, "calls:\n{}", call_report(&body, &index));
    assert_eq!(
        decode_in[0].ty.get(),
        Ty::Nothing,
        "calls:\n{}",
        call_report(&body, &index)
    );
    let safe = (0..body.expression_count())
        .filter_map(|raw| body.expr(FirExprId::from_raw(raw as u32)))
        .find(|expression| matches!(expression.kind, FirExprKind::SafeCall { .. }))
        .expect("safe call");
    assert_eq!(
        safe.ty.get(),
        Ty::nullable(Ty::Nothing),
        "safe call type\n{}",
        call_report(&body, &index)
    );
    let elvis = (0..body.expression_count())
        .filter_map(|raw| body.expr(FirExprId::from_raw(raw as u32)))
        .find(|expression| matches!(expression.kind, FirExprKind::Elvis { .. }))
        .expect("elvis");
    assert_eq!(elvis.ty.get(), Ty::Nothing, "elvis type");
}

#[test]
fn a_concrete_expected_type_fixes_a_contravariant_argument() {
    let (body, index) = checked_function_body(
        "class Context<T>\n\
         fun <T> select(value: Context<in T>): T = null as T\n\
         fun box(): String = select(Context<Any>())\n",
        "box",
    );
    let select = calls_named(&body, &index, "select");
    assert_eq!(select.len(), 1, "calls:\n{}", call_report(&body, &index));
    assert_eq!(
        select[0].ty.get(),
        Ty::String,
        "calls:\n{}",
        call_report(&body, &index)
    );
}

#[test]
fn an_out_projected_argument_instantiates_the_call_at_nothing() {
    let (body, index) = checked_function_body(
        "class Context<T>\n\
         fun <T> something(): T = null as T\n\
         fun <T> decodeIn(typeFrom: Context<in T>): T = something()\n\
         fun <T> decodeOut(typeFrom: Context<out T>): T = decodeIn(typeFrom)\n",
        "decodeOut",
    );
    let decode_in = calls_named(&body, &index, "decodeIn");
    assert_eq!(decode_in.len(), 1, "calls:\n{}", call_report(&body, &index));
    assert_eq!(
        decode_in[0].ty.get(),
        Ty::Nothing,
        "calls:\n{}",
        call_report(&body, &index)
    );
}
