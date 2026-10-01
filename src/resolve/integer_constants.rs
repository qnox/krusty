//! Integer-constant provenance used by call applicability and contextual coercion.

use crate::ast::{BinOp, Expr, ExprId, File, UnOp};
use crate::integer_constant::{IntegerConstant, IntegerConstantOp};
use crate::types::Ty;

use super::{
    checked_constant_expression, BuiltinUnaryOperation, CallArgKind, CheckedConstantExpression,
};

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
/// checker. Every operation is evaluated in the expression's ordinary width (`Int` or `UInt`), not
/// in a wider scratch type: Kotlin wraps integral overflow before any call-boundary coercion, so the
/// recorded constant must carry that wrapped value.
/// Keeping this outside either phase also prevents the two call paths from drifting on which
/// expressions carry literal provenance. An `if`, `when`, or block is the same kind of constant
/// when every branch value is: adaptation uses one representative that fits a target only when
/// every branch does, and the branches themselves still evaluate as `Int`.
pub(super) fn folded_integer_literal(file: &File, expression: ExprId) -> Option<IntegerConstant> {
    integer_constant(file, expression)
}

/// Evaluate an integer constant from the exact operator/call decisions already recorded by checking.
/// Conditional and block nodes contribute one representative only when every value branch is a
/// checked constant. Unsigned arithmetic deliberately remains an ordinary `UInt` value: Kotlin does
/// not contextually adapt `1u + 2u` to another unsigned width.
pub(super) fn checked_integer_constant(
    context: CheckedConstantExpression<'_>,
    expression: ExprId,
) -> Option<IntegerConstant> {
    let branches = integer_constant_branches(context.file, expression);
    if let Some(branches) = branches {
        let values = branches
            .into_iter()
            .map(|branch| checked_integer_constant(context, branch))
            .collect::<Option<Vec<_>>>()?;
        return IntegerConstant::representative(&values);
    }

    let ty = *context.expression_types.get(expression.0 as usize)?;
    let constant = checked_constant_expression(context, expression, ty)?;
    match (ty.non_null(), constant.value) {
        (Ty::Int, crate::libraries::LibConst::Int(value)) => Some(IntegerConstant::Signed(value)),
        (Ty::UInt, crate::libraries::LibConst::Int(value))
            if matches!(context.file.expr(expression), Expr::UIntLit(_))
                || context.resolved_constants.contains_key(&expression) =>
        {
            Some(IntegerConstant::Unsigned(u64::from(value as u32)))
        }
        _ => None,
    }
}

fn integer_constant(file: &File, expression: ExprId) -> Option<IntegerConstant> {
    match file.expr(expression) {
        Expr::IntLit(value) => i32::try_from(*value).ok().map(IntegerConstant::Signed),
        Expr::UIntLit(value) => u64::try_from(*value).ok().and_then(|magnitude| {
            let constant = IntegerConstant::Unsigned(magnitude);
            constant.fits(Ty::UInt).then_some(constant)
        }),
        Expr::Unary {
            op: UnOp::Plus,
            operand,
        } => integer_constant(file, *operand),
        Expr::Unary {
            op: UnOp::Neg,
            operand,
        } => match integer_constant(file, *operand)? {
            IntegerConstant::Signed(value) => value.checked_neg().map(IntegerConstant::Signed),
            IntegerConstant::Unsigned(_) => None,
        },
        Expr::Binary { op, lhs, rhs, .. }
            if matches!(
                op,
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem
            ) =>
        {
            let left = integer_constant(file, *lhs)?;
            let right = integer_constant(file, *rhs)?;
            let operation = match op {
                BinOp::Add => IntegerConstantOp::Add,
                BinOp::Sub => IntegerConstantOp::Subtract,
                BinOp::Mul => IntegerConstantOp::Multiply,
                BinOp::Div => IntegerConstantOp::Divide,
                BinOp::Rem => IntegerConstantOp::Remainder,
                _ => unreachable!("guarded integer constant operator"),
            };
            let folded = left.fold(operation, right)?;
            // Unsigned arithmetic stays `UInt`. The reference compiler does not adapt
            // `1u + 2u` to `UByte` or `65535u + 1u` to `ULong`.
            (!folded.is_unsigned()).then_some(folded)
        }
        _ => {
            let branches = integer_constant_branches(file, expression)?;
            let folded = branches
                .into_iter()
                .map(|branch| integer_constant(file, branch))
                .collect::<Option<Vec<_>>>()?;
            IntegerConstant::representative(&folded)
        }
    }
}

/// Apply a selected primitive unary operation to its syntax-only constant operand.
///
/// The operation is semantic provenance recorded by overload selection. The member spelling is
/// deliberately ignored: a source member named `unaryPlus` or `unaryMinus` is not a built-in
/// integer constant merely because it has the same name.
pub(super) fn selected_builtin_unary_integer_constant(
    file: &File,
    expression: ExprId,
    operation: BuiltinUnaryOperation,
) -> Option<IntegerConstant> {
    let Expr::Call { callee, args } = file.expr(expression) else {
        return None;
    };
    if !args.is_empty() {
        return None;
    }
    let Expr::Member { receiver, .. } = file.expr(*callee) else {
        return None;
    };
    let value = integer_constant(file, *receiver)?;
    match (operation, value) {
        (BuiltinUnaryOperation::Identity, value) => Some(value),
        (BuiltinUnaryOperation::Negate, IntegerConstant::Signed(value)) => {
            value.checked_neg().map(IntegerConstant::Signed)
        }
        (BuiltinUnaryOperation::Negate, IntegerConstant::Unsigned(_)) => None,
    }
}

/// Combine an already-computed runtime type with syntax-only call-argument provenance.
pub(super) fn call_arg_kind(file: &File, expression: ExprId, ty: Ty) -> CallArgKind {
    if file.is_spread_arg(expression) {
        CallArgKind::Spread(ty)
    } else if matches!(file.expr(expression), Expr::Lambda { .. }) {
        CallArgKind::LambdaLiteral(ty)
    } else if let Some(constant) = folded_integer_literal(file, expression) {
        CallArgKind::integer_constant(ty, constant)
    } else {
        CallArgKind::Typed(ty)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::DiagSink;
    use crate::lexer::lex;
    use crate::libraries::Origin;
    use crate::parser::parse;
    use crate::resolve::{
        check_file, signature_collection::collect_signatures, ExprLowering, ResolvedCall,
    };

    fn signed(values: &[i32]) -> CallArgKind {
        let values = values
            .iter()
            .copied()
            .map(IntegerConstant::Signed)
            .collect::<Vec<_>>();
        let representative =
            IntegerConstant::representative(&values).expect("signed representative");
        CallArgKind::integer_constant(Ty::Int, representative)
    }

    fn unsigned(values: &[u64]) -> CallArgKind {
        let values = values
            .iter()
            .copied()
            .map(IntegerConstant::Unsigned)
            .collect::<Vec<_>>();
        let representative =
            IntegerConstant::representative(&values).expect("unsigned representative");
        CallArgKind::integer_constant(Ty::UInt, representative)
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

        let above_i32 = unsigned(&[0, u64::from(i32::MAX as u32) + 1]);
        assert!(!above_i32.adapts_integer_literal_to(Ty::UShort));
        assert!(above_i32.adapts_integer_literal_to(Ty::ULong));

        let uint_max = unsigned(&[u64::from(u32::MAX)]);
        assert!(uint_max.adapts_integer_literal_to(Ty::ULong));
        assert!(!uint_max.adapts_integer_literal_to(Ty::UShort));
    }

    #[test]
    fn signed_and_unsigned_branches_do_not_share_literal_provenance() {
        assert_eq!(
            IntegerConstant::representative(&[
                IntegerConstant::Signed(1),
                IntegerConstant::Unsigned(1),
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
        assert!(kinds[5].adapts_integer_literal_to(Ty::Long));
        assert!(kinds[6].adapts_integer_literal_to(Ty::UByte));
        assert!(!kinds[7].adapts_integer_literal_to(Ty::UByte));
        assert!(kinds[7].adapts_integer_literal_to(Ty::UShort));
        assert!(!kinds[8].adapts_integer_literal_to(Ty::UShort));
        assert!(kinds[8].adapts_integer_literal_to(Ty::ULong));
        assert!(!kinds[9].adapts_integer_literal_to(Ty::UByte));
        assert!(!kinds[9].adapts_integer_literal_to(Ty::ULong));
        assert!(kinds[..6].iter().all(|argument| argument.ty() == Ty::Int));
        assert!(kinds[6..].iter().all(|argument| argument.ty() == Ty::UInt));
    }

    #[test]
    fn explicit_unary_call_requires_the_selected_builtin_operation() {
        let source = "fun sample() { target(1.unaryMinus()) }";
        let mut diagnostics = DiagSink::new();
        let tokens = lex(source, &mut diagnostics);
        let file = parse(source, &tokens, &mut diagnostics);
        let argument = file
            .expr_arena
            .iter()
            .find_map(|expression| match expression {
                Expr::Call { callee, args }
                    if matches!(file.expr(*callee), Expr::Name(name) if name == "target") =>
                {
                    args.first().copied()
                }
                _ => None,
            })
            .expect("unaryMinus argument");

        assert_eq!(folded_integer_literal(&file, argument), None);
        assert_eq!(
            selected_builtin_unary_integer_constant(&file, argument, BuiltinUnaryOperation::Negate,),
            Some(IntegerConstant::Signed(-1)),
        );
    }

    #[test]
    fn same_spelled_source_member_does_not_record_a_builtin_unary_operation() {
        let source = "class Counter {\n\
                          fun unaryMinus(): Int = 1\n\
                      }\n\
                      fun use(counter: Counter): Int = counter.unaryMinus()";
        let mut diagnostics = DiagSink::new();
        let tokens = lex(source, &mut diagnostics);
        let file = parse(source, &tokens, &mut diagnostics);
        let files = vec![file];
        let mut symbols = collect_signatures(&files, &mut diagnostics);
        let info = check_file(&files[0], &mut symbols, &mut diagnostics);
        assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

        let call = files[0]
            .expr_arena
            .iter()
            .enumerate()
            .find_map(|(index, expression)| match expression {
                Expr::Call { callee, .. }
                    if matches!(files[0].expr(*callee), Expr::Member { name, .. } if name == "unaryMinus") =>
                {
                    Some(ExprId(index as u32))
                }
                _ => None,
            })
            .expect("source unaryMinus call");
        assert!(!matches!(
            info.expr_lowers.get(&call),
            Some(ExprLowering::BuiltinUnaryCall { .. })
        ));
        assert!(matches!(
            info.resolved_calls.get(&call),
            Some(ResolvedCall::Member(member))
                if matches!(member.origin, Origin::Module { .. })
                    && member.member.name == "unaryMinus"
                    && member.member.stable_declaration.is_some()
        ));
    }

    #[test]
    fn an_unsigned_sum_is_a_call_constant_but_not_a_sibling_constant() {
        let source = "fun sample() { target(1u + 2u) }";
        let mut diagnostics = DiagSink::new();
        let tokens = lex(source, &mut diagnostics);
        let file = parse(source, &tokens, &mut diagnostics);
        let argument = file
            .expr_arena
            .iter()
            .find_map(|expression| match expression {
                Expr::Call { callee, args }
                    if matches!(file.expr(*callee), Expr::Name(name) if name == "target") =>
                {
                    args.first().copied()
                }
                _ => None,
            })
            .expect("unsigned sum");
        assert_eq!(folded_integer_literal(&file, argument), None);
        assert!(!call_arg_kind(&file, argument, Ty::UInt).adapts_integer_literal_to(Ty::UByte));
    }

    #[test]
    fn unsigned_literals_above_i32_max_keep_a_ulong_range() {
        let source = "fun sample() { target(2147483648u, 4294967295u, 2147483648u) }";
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
            .map(|argument| call_arg_kind(&file, *argument, Ty::UInt))
            .collect::<Vec<_>>();
        assert!(kinds[0].adapts_integer_literal_to(Ty::ULong));
        assert!(!kinds[0].adapts_integer_literal_to(Ty::UShort));
        assert!(kinds[1].adapts_integer_literal_to(Ty::ULong));
        assert!(!kinds[1].adapts_integer_literal_to(Ty::UShort));
        assert!(!kinds[2].adapts_integer_literal_to(Ty::UByte));
        assert!(kinds.iter().all(|argument| argument.ty() == Ty::UInt));
    }
}
