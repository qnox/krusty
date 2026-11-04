//! Elvis temporary identity, null proofs, and parity with checked FIR lowering.

use super::super::DependencyBodyUnit;
use super::demo_package::{demo, eqeq, relation, source_body, Demo, T};
use super::{lowered_body, public_symbol, validated};
use crate::metadata::klib_ir::tree::{
    KlibIrArguments, KlibIrBranch, KlibIrExprKind, KlibIrMemberAccess, KlibIrStatement,
};
use crate::metadata::klib_ir::{KlibIrConstant, KlibIrSymbolKind};

#[derive(Clone, Copy, Debug, PartialEq)]
enum Defect {
    None,
    ExtraStatement,
    SourceVariable,
    MutableTemporary,
    AnnotatedTemporary,
    MissingInitializer,
    WrongSymbolKind,
    WhenOrigin,
    WhenType,
    BranchCount,
    FalseElse,
    ConditionReadsParameter,
    PresentReadsParameter,
    PresentType,
    NonNullOperand,
    OtherBuiltin,
    TypeArgument,
    FallbackType,
}

fn fixture(input: T, result: T, defect: Defect) -> Demo {
    let parameters: &'static [T] = match (input, result) {
        (T::NullableString, T::String) => &[T::NullableString, T::String],
        (T::NullableInt, T::Int) => &[T::NullableInt, T::Int],
        (T::NullableAny, T::Any) => &[T::NullableAny, T::Any],
        _ => panic!("unsupported Elvis fixture types: {input:?}, {result:?}"),
    };
    demo(&[("f", parameters, result)], move |_, body| {
        let value = body.param(0);
        let initializer = (defect != Defect::MissingInitializer).then_some(value);
        let flags = match defect {
            Defect::MutableTemporary => 2,
            Defect::AnnotatedTemporary => 1,
            _ => 0,
        };
        let origin = if defect == Defect::SourceVariable {
            "DEFINED"
        } else {
            "IR_TEMPORARY_VARIABLE"
        };
        let (mut temporary, mut symbol) = body.variable_with(input, initializer, flags, origin);
        if defect == Defect::WrongSymbolKind {
            let KlibIrStatement::Variable(id) = temporary else {
                unreachable!()
            };
            let mut variable = body.arena.variable(id).clone();
            variable.base.symbol.kind = KlibIrSymbolKind::ValueParameter;
            symbol = variable.base.symbol.clone();
            temporary = KlibIrStatement::Variable(body.arena.push_variable(variable));
        }
        let read = if defect == Defect::ConditionReadsParameter {
            body.param(0)
        } else {
            body.get(&symbol, input)
        };
        let null = if defect == Defect::NonNullOperand {
            body.int(0)
        } else {
            body.constant(T::NullableNothing, KlibIrConstant::Null)
        };
        let operator = if defect == Defect::OtherBuiltin {
            relation("less")
        } else {
            eqeq()
        };
        let type_arguments = if defect == Defect::TypeArgument {
            vec![Some(body.ty(T::String))]
        } else {
            vec![]
        };
        let condition = body.expr(
            T::Boolean,
            KlibIrExprKind::Call {
                access: KlibIrMemberAccess {
                    symbol: public_symbol(KlibIrSymbolKind::Function, operator.0),
                    arguments: KlibIrArguments::Flat(vec![Some(read), Some(null)]),
                    type_arguments,
                    origin: None,
                },
                super_qualifier: None,
            },
        );
        let fallback = if defect == Defect::FallbackType {
            body.int(0)
        } else {
            body.param(1)
        };
        let otherwise = body.boolean(defect != Defect::FalseElse);
        let present = if defect == Defect::PresentReadsParameter {
            body.param(0)
        } else {
            body.get(
                &symbol,
                if defect == Defect::PresentType {
                    T::NullableInt
                } else {
                    input
                },
            )
        };
        let mut branches = vec![
            KlibIrBranch {
                condition,
                result: fallback,
            },
            KlibIrBranch {
                condition: otherwise,
                result: present,
            },
        ];
        if defect == Defect::BranchCount {
            branches.pop();
        }
        let conditional = body.expr(
            if defect == Defect::WhenType {
                input
            } else {
                result
            },
            KlibIrExprKind::When {
                branches,
                origin: (defect == Defect::WhenOrigin).then(|| "IF".to_owned()),
            },
        );
        let mut statements = vec![temporary, KlibIrStatement::Expression(conditional)];
        if defect == Defect::ExtraStatement {
            statements.insert(1, KlibIrStatement::Expression(value));
        }
        let block = body.expr(
            result,
            KlibIrExprKind::Block {
                statements,
                origin: Some("ELVIS".to_owned()),
            },
        );
        Some(vec![body.ret(block)])
    })
}

#[test]
fn elvis_snapshots_and_branch_coercions_match_checked_fir() {
    for (input, result) in [
        (T::NullableString, T::String),
        (T::NullableInt, T::Int),
        (T::NullableAny, T::Any),
    ] {
        let fixture = fixture(input, result, Defect::None);
        let mut unit = DependencyBodyUnit::default();
        let function = fixture.lower(&mut unit, "f", &[]).unwrap();
        validated(unit.ir());
        let source = format!(
            "fun f(a: {}?, b: {}): {} {{\n return a ?: b\n}}\n",
            input.class(),
            result.class(),
            result.class()
        );
        assert_eq!(lowered_body(&unit, function), source_body(&source, "f", 2));
        assert_eq!(
            unit.ir()
                .whens
                .elvises
                .values()
                .copied()
                .collect::<Vec<_>>(),
            vec![0]
        );
    }
}

#[test]
fn malformed_elvis_contracts_decline_by_their_exact_boundary() {
    let mismatch = "its serialized declaration disagrees with the selected one: ";
    for (defect, reason) in [
        (
            Defect::ExtraStatement,
            format!("{mismatch}an Elvis block does not contain one temporary and a conditional"),
        ),
        (
            Defect::SourceVariable,
            format!("{mismatch}an Elvis block does not declare a compiler temporary"),
        ),
        (
            Defect::MutableTemporary,
            "it uses a mutable or annotated compiler temporary".to_owned(),
        ),
        (
            Defect::AnnotatedTemporary,
            "it uses a mutable or annotated compiler temporary".to_owned(),
        ),
        (
            Defect::MissingInitializer,
            format!("{mismatch}a compiler temporary has no initializer"),
        ),
        (
            Defect::WrongSymbolKind,
            format!("{mismatch}an Elvis block does not declare a compiler temporary"),
        ),
        (
            Defect::WhenOrigin,
            format!("{mismatch}an Elvis block does not end in its origin-less conditional"),
        ),
        (
            Defect::WhenType,
            format!("{mismatch}an Elvis conditional has another type or else condition"),
        ),
        (
            Defect::BranchCount,
            format!("{mismatch}an Elvis conditional does not have two branches"),
        ),
        (
            Defect::FalseElse,
            format!("{mismatch}an Elvis conditional has another type or else condition"),
        ),
        (
            Defect::ConditionReadsParameter,
            format!("{mismatch}an Elvis null proof does not name its exact temporary"),
        ),
        (
            Defect::PresentReadsParameter,
            format!("{mismatch}an Elvis null proof does not name its exact temporary"),
        ),
        (
            Defect::PresentType,
            format!("{mismatch}an Elvis present read has another type than its temporary"),
        ),
        (
            Defect::NonNullOperand,
            format!("{mismatch}an Elvis null proof does not name its exact temporary"),
        ),
        (
            Defect::OtherBuiltin,
            format!("{mismatch}an Elvis condition is not its compiler null comparison"),
        ),
        (
            Defect::TypeArgument,
            format!("{mismatch}an Elvis condition is not its compiler null comparison"),
        ),
        (
            Defect::FallbackType,
            "it converts a value implicitly".to_owned(),
        ),
    ] {
        assert_eq!(
            fixture(T::NullableString, T::String, defect).declined("f", &[]),
            format!("the KLIB body of `demo.f` ({reason})"),
            "{defect:?}"
        );
    }
}

#[test]
fn an_unrelated_originless_when_still_declines() {
    let fixture = demo(&[("f", &[T::Boolean], T::Int)], |_, body| {
        let condition = body.param(0);
        let one = body.int(1);
        let otherwise = body.otherwise();
        let zero = body.int(0);
        let conditional = body.expr(
            T::Int,
            KlibIrExprKind::When {
                branches: vec![
                    KlibIrBranch {
                        condition,
                        result: one,
                    },
                    KlibIrBranch {
                        condition: otherwise,
                        result: zero,
                    },
                ],
                origin: None,
            },
        );
        Some(vec![body.ret(conditional)])
    });
    assert_eq!(fixture.declined("f", &[]), "the KLIB body of `demo.f` (it uses a `when` of another origin than `if`, `when`, `&&` or `||`)");
}
