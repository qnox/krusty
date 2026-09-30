//! Integer-constant provenance used by call applicability and contextual coercion.

use crate::ast::{BinOp, Expr, ExprId, File, UnOp};
use crate::types::Ty;

use super::CallArgKind;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FoldedIntegerLiteral {
    Signed(i32),
    Unsigned(i32),
}

impl FoldedIntegerLiteral {
    pub(super) fn value(self) -> i32 {
        match self {
            Self::Signed(value) | Self::Unsigned(value) => value,
        }
    }

    fn binary(self, right: Self, operation: impl FnOnce(i32, i32) -> Option<i32>) -> Option<Self> {
        match (self, right) {
            (Self::Signed(left), Self::Signed(right)) => operation(left, right).map(Self::Signed),
            (Self::Unsigned(left), Self::Unsigned(right)) => {
                operation(left, right).map(Self::Unsigned)
            }
            _ => None,
        }
    }
}

/// One value whose primitive range is exactly the range shared by every branch value.
///
/// `200` does not fit in `Byte` and `-129` does not fit in `Byte`, so a conditional that contains
/// either value must not adapt to `Byte`. The hardest constituent has that same refusal, and any
/// constituent works once every one fits.
fn representative_integer_constant(
    values: &[FoldedIntegerLiteral],
) -> Option<FoldedIntegerLiteral> {
    if values.is_empty() {
        return None;
    }
    let signed = values
        .iter()
        .all(|value| matches!(value, FoldedIntegerLiteral::Signed(_)));
    let unsigned = values
        .iter()
        .all(|value| matches!(value, FoldedIntegerLiteral::Unsigned(_)));
    if signed {
        let hardest = values
            .iter()
            .map(|value| value.value())
            .find(|value| i16::try_from(*value).is_err())
            .or_else(|| {
                values
                    .iter()
                    .map(|value| value.value())
                    .find(|value| i8::try_from(*value).is_err())
            })
            .unwrap_or_else(|| values[0].value());
        Some(FoldedIntegerLiteral::Signed(hardest))
    } else if unsigned {
        let hardest = values
            .iter()
            .map(|value| value.value())
            .find(|value| u16::try_from(*value).is_err())
            .or_else(|| {
                values
                    .iter()
                    .map(|value| value.value())
                    .find(|value| u8::try_from(*value).is_err())
            })
            .unwrap_or_else(|| values[0].value());
        Some(FoldedIntegerLiteral::Unsigned(hardest))
    } else {
        None
    }
}

/// Branch values of a conditional integer constant: an `if` that has an `else`, a `when` that has
/// an `else` arm, or a block through its trailing expression. A missing `else` is not a value.
fn integer_constant_branches(file: &File, expression: ExprId) -> Option<Vec<ExprId>> {
    match file.expr(expression) {
        Expr::If {
            then_branch,
            else_branch: Some(else_branch),
            ..
        } => Some(vec![*then_branch, *else_branch]),
        Expr::When { arms, .. } if arms.iter().any(|arm| arm.conditions.is_empty()) => {
            Some(arms.iter().map(|arm| arm.body).collect())
        }
        Expr::Block {
            trailing: Some(trailing),
            ..
        } => Some(vec![*trailing]),
        _ => None,
    }
}

/// Recognize and safely fold the integer-constant syntax accepted at call sites.
///
/// This is deliberately the one AST walk used by both lightweight signature inference and the full
/// checker. Every operation is checked in the expression's ordinary `Int` representation, not in a
/// wider scratch type: lowering evaluates the same `Int` operations before any call-boundary
/// coercion, so accepting an expression that overflows here would silently change Kotlin semantics.
/// Keeping this outside either phase also prevents the two call paths from drifting on which
/// expressions carry literal provenance. An `if`, `when`, or block is the same kind of constant
/// when every branch value is: adaptation uses one representative that fits a target only when
/// every branch does, and the branches themselves still evaluate as `Int`.
pub(super) fn folded_integer_literal(
    file: &File,
    expression: ExprId,
) -> Option<FoldedIntegerLiteral> {
    match file.expr(expression) {
        Expr::IntLit(value) => i32::try_from(*value).ok().map(FoldedIntegerLiteral::Signed),
        Expr::UIntLit(value) => i32::try_from(*value)
            .ok()
            .map(FoldedIntegerLiteral::Unsigned),
        Expr::Unary {
            op: UnOp::Plus,
            operand,
        } => folded_integer_literal(file, *operand),
        Expr::Unary {
            op: UnOp::Neg,
            operand,
        } => match folded_integer_literal(file, *operand)? {
            FoldedIntegerLiteral::Signed(value) => {
                value.checked_neg().map(FoldedIntegerLiteral::Signed)
            }
            FoldedIntegerLiteral::Unsigned(_) => None,
        },
        Expr::Binary { op, lhs, rhs, .. }
            if matches!(
                op,
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem
            ) =>
        {
            let left = folded_integer_literal(file, *lhs)?;
            let right = folded_integer_literal(file, *rhs)?;
            left.binary(right, |left, right| match op {
                BinOp::Add => left.checked_add(right),
                BinOp::Sub => left.checked_sub(right),
                BinOp::Mul => left.checked_mul(right),
                BinOp::Div => left.checked_div(right),
                BinOp::Rem => left.checked_rem(right),
                _ => unreachable!("guarded integer constant operator"),
            })
        }
        Expr::Call { callee, args } if args.len() == 1 => {
            let Expr::Member { receiver, name } = file.expr(*callee) else {
                return None;
            };
            let left = folded_integer_literal(file, *receiver)?;
            let right = folded_integer_literal(file, args[0])?;
            left.binary(right, |left, right| match name.as_str() {
                "plus" => left.checked_add(right),
                "minus" => left.checked_sub(right),
                "times" => left.checked_mul(right),
                "div" => left.checked_div(right),
                "rem" => left.checked_rem(right),
                _ => None,
            })
        }
        Expr::Call { callee, args } if args.is_empty() => {
            let Expr::Member { receiver, name } = file.expr(*callee) else {
                return None;
            };
            let value = folded_integer_literal(file, *receiver)?;
            match (name.as_str(), value) {
                ("unaryPlus", value) => Some(value),
                ("unaryMinus", FoldedIntegerLiteral::Signed(value)) => {
                    value.checked_neg().map(FoldedIntegerLiteral::Signed)
                }
                _ => None,
            }
        }
        _ => {
            let branches = integer_constant_branches(file, expression)?;
            let folded = branches
                .into_iter()
                .map(|branch| folded_integer_literal(file, branch))
                .collect::<Option<Vec<_>>>()?;
            representative_integer_constant(&folded)
        }
    }
}

/// Combine an already-computed runtime type with syntax-only call-argument provenance.
pub(super) fn call_arg_kind(file: &File, expression: ExprId, ty: Ty) -> CallArgKind {
    if file.is_spread_arg(expression) {
        CallArgKind::Spread(ty)
    } else if matches!(file.expr(expression), Expr::Lambda { .. }) {
        CallArgKind::LambdaLiteral(ty)
    } else if let Some(value) = folded_integer_literal(file, expression) {
        CallArgKind::integer_literal(ty, value.value())
    } else {
        CallArgKind::Typed(ty)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::DiagSink;
    use crate::lexer::lex;
    use crate::parser::parse;

    fn signed(values: &[i32]) -> CallArgKind {
        let values = values
            .iter()
            .copied()
            .map(FoldedIntegerLiteral::Signed)
            .collect::<Vec<_>>();
        let representative =
            representative_integer_constant(&values).expect("signed representative");
        CallArgKind::integer_literal(Ty::Int, representative.value())
    }

    fn unsigned(values: &[i32]) -> CallArgKind {
        let values = values
            .iter()
            .copied()
            .map(FoldedIntegerLiteral::Unsigned)
            .collect::<Vec<_>>();
        let representative =
            representative_integer_constant(&values).expect("unsigned representative");
        CallArgKind::integer_literal(Ty::UInt, representative.value())
    }

    #[test]
    fn branch_representative_preserves_the_narrowest_shared_range() {
        assert!(signed(&[-128, 127]).adapts_integer_literal_to(Ty::Byte));

        let short = signed(&[-129, 128]);
        assert!(!short.adapts_integer_literal_to(Ty::Byte));
        assert!(short.adapts_integer_literal_to(Ty::Short));

        let int = signed(&[-32_769, 32_768]);
        assert!(!int.adapts_integer_literal_to(Ty::Short));
        assert!(int.adapts_integer_literal_to(Ty::Long));

        assert!(unsigned(&[0, 255]).adapts_integer_literal_to(Ty::UByte));

        let ushort = unsigned(&[0, 256]);
        assert!(!ushort.adapts_integer_literal_to(Ty::UByte));
        assert!(ushort.adapts_integer_literal_to(Ty::UShort));

        let uint = unsigned(&[0, 65_536]);
        assert!(!uint.adapts_integer_literal_to(Ty::UShort));
        assert!(uint.adapts_integer_literal_to(Ty::ULong));
    }

    #[test]
    fn signed_and_unsigned_branches_do_not_share_literal_provenance() {
        assert_eq!(
            representative_integer_constant(&[
                FoldedIntegerLiteral::Signed(1),
                FoldedIntegerLiteral::Unsigned(1),
            ]),
            None
        );
    }

    #[test]
    fn literal_provenance_is_call_local_and_range_aware() {
        let source = "fun sample() { target(127, 128, -129, 1 + 2, 1 / 0, 2_000_000_000 + 2_000_000_000, 255u, 256u, 65536u, 1u + 2u) }";
        let mut diagnostics = DiagSink::new();
        let tokens = lex(source, &mut diagnostics);
        let file = parse(source, &tokens, &mut diagnostics);
        assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
        let arguments = file
            .expr_arena
            .iter()
            .find_map(|expression| match expression {
                Expr::Call { callee, args }
                    if matches!(file.expr(*callee), Expr::Name(name) if name == "target") =>
                {
                    Some(args)
                }
                _ => None,
            })
            .expect("target call");
        let kinds = arguments
            .iter()
            .enumerate()
            .map(|(index, argument)| {
                let ty = if index >= 6 { Ty::UInt } else { Ty::Int };
                call_arg_kind(&file, *argument, ty)
            })
            .collect::<Vec<_>>();

        assert!(kinds[0].adapts_integer_literal_to(Ty::Byte));
        assert!(!kinds[1].adapts_integer_literal_to(Ty::Byte));
        assert!(kinds[1].adapts_integer_literal_to(Ty::Short));
        assert!(!kinds[2].adapts_integer_literal_to(Ty::Byte));
        assert!(kinds[3].adapts_integer_literal_to(Ty::Byte));
        assert!(!kinds[4].adapts_integer_literal_to(Ty::Short));
        assert!(!kinds[4].adapts_integer_literal_to(Ty::Long));
        assert!(!kinds[5].adapts_integer_literal_to(Ty::Long));
        assert!(kinds[6].adapts_integer_literal_to(Ty::UByte));
        assert!(!kinds[7].adapts_integer_literal_to(Ty::UByte));
        assert!(kinds[7].adapts_integer_literal_to(Ty::UShort));
        assert!(!kinds[8].adapts_integer_literal_to(Ty::UShort));
        assert!(kinds[8].adapts_integer_literal_to(Ty::ULong));
        assert!(kinds[9].adapts_integer_literal_to(Ty::UByte));
        assert!(kinds[..6].iter().all(|argument| argument.ty() == Ty::Int));
        assert!(kinds[6..].iter().all(|argument| argument.ty() == Ty::UInt));
    }
}
