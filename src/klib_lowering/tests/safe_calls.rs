//! Safe-call temporaries, their null proofs and guarded selectors, against checked FIR lowering.

use super::super::DependencyBodyUnit;
use super::demo_package::{demo, eqeq, not, source_body_with_stdlib, Demo, T};
use super::{lowered_body, public_symbol, validated};
use crate::metadata::klib_ir::tree::{
    KlibIrArguments, KlibIrBranch, KlibIrExprKind, KlibIrMemberAccess, KlibIrStatement,
    KlibIrTypeOperator,
};
use crate::metadata::klib_ir::{KlibIrConstant, KlibIrSymbolKind};

#[derive(Clone, Copy, Debug, PartialEq)]
enum Defect {
    None,
    SourceVariable,
    ConditionReadsParameter,
    NonNullAbsentResult,
    ExtraStatement,
    SelectorReadType,
}

/// kotlinc's `fun f(a: Boolean?): Boolean? = a?.not()`.
fn fixture(defect: Defect) -> Demo {
    demo(
        &[("f", &[T::NullableBoolean], T::NullableBoolean)],
        move |_, body| {
            let value = body.param(0);
            let origin = if defect == Defect::SourceVariable {
                "DEFINED"
            } else {
                "IR_TEMPORARY_VARIABLE"
            };
            let (temporary, symbol) =
                body.variable_with(T::NullableBoolean, Some(value), 0, origin);
            let read = if defect == Defect::ConditionReadsParameter {
                body.param(0)
            } else {
                body.get(&symbol, T::NullableBoolean)
            };
            let null = body.constant(T::NullableNothing, KlibIrConstant::Null);
            let condition = body.expr(
                T::Boolean,
                KlibIrExprKind::Call {
                    access: KlibIrMemberAccess {
                        symbol: public_symbol(KlibIrSymbolKind::Function, eqeq().0),
                        arguments: KlibIrArguments::Flat(vec![Some(read), Some(null)]),
                        type_arguments: vec![],
                        origin: Some("EQEQ".to_owned()),
                    },
                    super_qualifier: None,
                },
            );
            let absent = if defect == Defect::NonNullAbsentResult {
                body.boolean(false)
            } else {
                body.constant(T::NullableNothing, KlibIrConstant::Null)
            };
            let receiver = if defect == Defect::SelectorReadType {
                body.get(&symbol, T::NullableString)
            } else {
                body.get(&symbol, T::NullableBoolean)
            };
            let receiver =
                body.type_operator(KlibIrTypeOperator::ImplicitCast, receiver, T::Boolean);
            let selector = body.builtin(not(), vec![receiver], None);
            let otherwise = body.boolean(true);
            let conditional = body.expr(
                T::NullableBoolean,
                KlibIrExprKind::When {
                    branches: vec![
                        KlibIrBranch {
                            condition,
                            result: absent,
                        },
                        KlibIrBranch {
                            condition: otherwise,
                            result: selector,
                        },
                    ],
                    origin: None,
                },
            );
            let mut statements = vec![temporary, KlibIrStatement::Expression(conditional)];
            if defect == Defect::ExtraStatement {
                let extra = body.param(0);
                statements.insert(1, KlibIrStatement::Expression(extra));
            }
            let block = body.expr(
                T::NullableBoolean,
                KlibIrExprKind::Block {
                    statements,
                    origin: Some("SAFE_CALL".to_owned()),
                },
            );
            Some(vec![body.ret(block)])
        },
    )
}

#[test]
fn a_safe_call_lowers_as_checked_fir_lowers_it() {
    let fixture = fixture(Defect::None);
    let mut unit = DependencyBodyUnit::default();
    let function = fixture.lower(&mut unit, "f", &[]).unwrap();
    validated(unit.ir());
    assert_eq!(
        lowered_body(&unit, function),
        source_body_with_stdlib(
            "fun f(a: Boolean?): Boolean? {\n return a?.not()\n}\n",
            "f",
            1
        )
    );
    assert_eq!(unit.ir().safe_calls.len(), 1);
}

#[test]
fn a_malformed_safe_call_declines_by_its_exact_boundary() {
    let mismatch = "its serialized declaration disagrees with the selected one: ";
    for (defect, reason) in [
        (
            Defect::SourceVariable,
            "a safe call does not declare a compiler temporary",
        ),
        (
            Defect::ConditionReadsParameter,
            "a safe-call null proof does not name its exact temporary",
        ),
        (
            Defect::NonNullAbsentResult,
            "a safe-call null proof does not name its exact temporary",
        ),
        (
            Defect::ExtraStatement,
            "a safe call does not contain one temporary and a conditional",
        ),
        (
            Defect::SelectorReadType,
            "a safe-call receiver read is typed apart from its temporary",
        ),
    ] {
        assert_eq!(
            fixture(defect).declined("f", &[]),
            format!("the KLIB body of `demo.f` ({mismatch}{reason})"),
            "{defect:?}"
        );
    }
}
