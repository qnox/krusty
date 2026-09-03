//! Semantic evaluation of Kotlin compile-time constant expressions.
//!
//! The parser contributes only expression structure. Operator meaning comes from the exact callable
//! selected by the checker, and constant reads arrive as declaration-owned payloads. This keeps
//! `const val` publication from growing a second syntax-only resolver and prevents an overloaded
//! `plus`/`compareTo` from being mistaken for a builtin constant operation.

use std::collections::HashMap;

use crate::ast::{BinOp, Expr, ExprId, File, TemplatePart, UnOp};
use crate::kt_string::KtStringBuf;
use crate::libraries::{CompilerIntrinsic, LibConst, LibraryConst, PrimitiveBinaryIntrinsic};
use crate::types::Ty;

use super::{ResolvedCall, SyntheticOperatorCall};

/// Extract a literal's value after checking has fixed its semantic type.
///
/// This deliberately remains narrower than [`checked_constant_expression`]. Ordinary immutable
/// properties use it for Kotlin metadata's literal `HAS_CONSTANT` fact, while only a declaration
/// carrying `const` may publish a folded expression such as `2 + 2`.
pub(super) fn source_literal_constant(
    file: &File,
    expression: ExprId,
    ty: Ty,
) -> Option<LibraryConst> {
    let mut ty = ty.canonical_semantic();
    if ty.mentions_error() {
        return None;
    }
    // An inferred literal has an intrinsic Kotlin type even while its declaration's compact
    // signature is still provisional. Use that concrete literal type rather than placing
    // `Ty::Pending` in the constant payload; projection may later narrow an explicitly declared
    // integer constant, while every published payload remains a real semantic value.
    if ty.mentions_pending() {
        ty = match file.expr(expression) {
            Expr::IntLit(_) => Ty::Int,
            Expr::LongLit(_) => Ty::Long,
            Expr::UIntLit(_) => Ty::UInt,
            Expr::ULongLit(_) => Ty::ULong,
            Expr::FloatLit(_) => Ty::Float,
            Expr::DoubleLit(_) => Ty::Double,
            Expr::BoolLit(_) => Ty::Boolean,
            Expr::CharLit(_) => Ty::Char,
            Expr::StringLit(_) => Ty::String,
            Expr::Unary {
                op: UnOp::Plus | UnOp::Neg,
                operand,
            } => source_literal_constant(file, *operand, Ty::Pending)?.ty,
            _ => return None,
        };
    }
    let value = match file.expr(expression) {
        Expr::IntLit(value) => LibConst::Int(*value as i32),
        Expr::LongLit(value) => LibConst::Long(*value),
        Expr::UIntLit(value) => match ty {
            Ty::ULong => LibConst::Long(*value),
            _ => LibConst::Int(*value as i32),
        },
        Expr::ULongLit(value) => LibConst::Long(*value),
        Expr::FloatLit(value) => LibConst::Float(*value),
        Expr::DoubleLit(value) => LibConst::Double(*value),
        Expr::BoolLit(value) => LibConst::Int(i32::from(*value)),
        Expr::CharLit(value) => LibConst::Int(i32::from(*value)),
        Expr::StringLit(value) => LibConst::Str(value.clone()),
        Expr::Unary {
            op: UnOp::Plus,
            operand,
        } if ty.non_null().is_numeric() => {
            return source_literal_constant(file, *operand, ty);
        }
        Expr::Unary {
            op: UnOp::Neg,
            operand,
        } => {
            let constant = source_literal_constant(file, *operand, ty)?;
            match constant.value {
                LibConst::Int(value) => LibConst::Int(value.checked_neg()?),
                LibConst::Long(value) => LibConst::Long(value.checked_neg()?),
                LibConst::Float(value) => LibConst::Float(-value),
                LibConst::Double(value) => LibConst::Double(-value),
                LibConst::Str(_) => return None,
            }
        }
        _ => return None,
    };
    Some(LibraryConst { ty, value })
}

/// Checked facts needed to evaluate one `const val` initializer.
pub(super) struct CheckedConstantExpression<'a> {
    pub file: &'a File,
    pub expression_types: &'a [Ty],
    pub resolved_constants: &'a HashMap<ExprId, LibraryConst>,
    pub resolved_calls: &'a HashMap<ExprId, ResolvedCall>,
    pub resolved_operator_calls: &'a HashMap<(ExprId, SyntheticOperatorCall), ResolvedCall>,
}

/// Evaluate the Kotlin compile-time expression rooted at `expression`.
///
/// `declared_ty` is authoritative at the root (`const val b: Byte = 1`); nested nodes use the types
/// assigned by ordinary checking. A result is produced only for operations whose selected callable
/// carries the corresponding provider-normalized builtin intrinsic.
pub(super) fn checked_constant_expression(
    context: CheckedConstantExpression<'_>,
    expression: ExprId,
    declared_ty: Ty,
) -> Option<LibraryConst> {
    Evaluator { context }.evaluate(expression, Some(declared_ty), 0)
}

struct Evaluator<'a> {
    context: CheckedConstantExpression<'a>,
}

impl Evaluator<'_> {
    fn expression_ty(&self, expression: ExprId, requested: Option<Ty>) -> Option<Ty> {
        let ty = requested.unwrap_or(*self.context.expression_types.get(expression.0 as usize)?);
        (!ty.mentions_error() && !ty.mentions_pending()).then(|| ty.canonical_semantic())
    }

    fn selected_intrinsic(
        &self,
        expression: ExprId,
        operator: SyntheticOperatorCall,
    ) -> Option<Option<CompilerIntrinsic>> {
        self.context
            .resolved_operator_calls
            .get(&(expression, operator))
            .map(ResolvedCall::compiler_intrinsic)
    }

    fn evaluate(
        &self,
        expression: ExprId,
        requested: Option<Ty>,
        depth: u32,
    ) -> Option<LibraryConst> {
        if depth > 64 {
            return None;
        }
        let ty = self.expression_ty(expression, requested)?;
        if let Some(constant) = self.context.resolved_constants.get(&expression) {
            return retype_constant(constant.clone(), ty);
        }
        if let Some(literal) = source_literal_constant(self.context.file, expression, ty) {
            return Some(literal);
        }
        match self.context.file.expr(expression) {
            Expr::Unary { op, operand } => {
                let operand = self.evaluate(*operand, None, depth + 1)?;
                evaluate_unary(*op, operand, ty)
            }
            Expr::Binary { op, lhs, rhs, .. } => {
                let left = self.evaluate(*lhs, None, depth + 1)?;
                let right = self.evaluate(*rhs, None, depth + 1)?;
                self.evaluate_binary(expression, *op, left, right, ty)
            }
            Expr::Template(parts) => {
                let mut output = KtStringBuf::new();
                for part in parts {
                    match part {
                        TemplatePart::Str(value) => output.push_kt(value),
                        TemplatePart::Expr(value) => {
                            let value = self.evaluate(*value, None, depth + 1)?;
                            push_constant_string(&value, &mut output)?;
                        }
                    }
                }
                Some(LibraryConst {
                    ty,
                    value: LibConst::Str(output.finish()),
                })
            }
            Expr::Call { callee, args }
                if args.is_empty()
                    && self
                        .context
                        .resolved_calls
                        .get(&expression)
                        .and_then(ResolvedCall::compiler_intrinsic)
                        == Some(CompilerIntrinsic::NumericConversion) =>
            {
                let Expr::Member { receiver, .. } = self.context.file.expr(*callee) else {
                    return None;
                };
                let operand = self.evaluate(*receiver, None, depth + 1)?;
                evaluate_numeric_conversion(operand, ty)
            }
            _ => None,
        }
    }

    fn evaluate_binary(
        &self,
        expression: ExprId,
        operation: BinOp,
        left: LibraryConst,
        right: LibraryConst,
        result_ty: Ty,
    ) -> Option<LibraryConst> {
        let operator = match operation {
            BinOp::Add => SyntheticOperatorCall::Plus,
            BinOp::Sub => SyntheticOperatorCall::Minus,
            BinOp::Mul => SyntheticOperatorCall::Times,
            BinOp::Div => SyntheticOperatorCall::Div,
            BinOp::Rem => SyntheticOperatorCall::Rem,
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => SyntheticOperatorCall::CompareTo,
            BinOp::Eq | BinOp::Ne | BinOp::And | BinOp::Or | BinOp::RefEq | BinOp::RefNe => {
                return evaluate_non_call_binary(operation, left, right);
            }
        };
        let selected_intrinsic = self.selected_intrinsic(expression, operator);
        if operation == BinOp::Add
            && result_ty.non_null() == Ty::String
            && matches!(
                selected_intrinsic,
                None | Some(Some(CompilerIntrinsic::StringPlus))
            )
        {
            let mut output = KtStringBuf::new();
            push_constant_string(&left, &mut output)?;
            push_constant_string(&right, &mut output)?;
            return Some(LibraryConst {
                ty: result_ty,
                value: LibConst::Str(output.finish()),
            });
        }
        if matches!(operation, BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge) {
            matches!(
                selected_intrinsic,
                None | Some(Some(CompilerIntrinsic::PrimitiveCompare))
            )
            .then_some(())?;
            return compare_numeric(operation, left, right);
        }
        let expected = match operation {
            BinOp::Add => PrimitiveBinaryIntrinsic::Add,
            BinOp::Sub => PrimitiveBinaryIntrinsic::Subtract,
            BinOp::Mul => PrimitiveBinaryIntrinsic::Multiply,
            BinOp::Div => PrimitiveBinaryIntrinsic::Divide,
            BinOp::Rem => PrimitiveBinaryIntrinsic::Remainder,
            _ => return None,
        };
        match selected_intrinsic {
            None => true,
            Some(Some(CompilerIntrinsic::PrimitiveBinary(operation))) => operation == expected,
            Some(Some(_)) | Some(None) => false,
        }
        .then_some(())?;
        arithmetic_numeric(expected, left, right, result_ty)
    }
}

fn retype_constant(mut constant: LibraryConst, ty: Ty) -> Option<LibraryConst> {
    let ty = ty.canonical_semantic();
    match (&constant.value, ty.non_null()) {
        (LibConst::Int(_), Ty::Boolean | Ty::Byte | Ty::Short | Ty::Int | Ty::Char)
        | (LibConst::Int(_), Ty::UByte | Ty::UShort | Ty::UInt)
        | (LibConst::Long(_), Ty::Long | Ty::ULong)
        | (LibConst::Float(_), Ty::Float)
        | (LibConst::Double(_), Ty::Double)
        | (LibConst::Str(_), Ty::String) => {
            constant.ty = ty;
            Some(constant)
        }
        _ => None,
    }
}

fn evaluate_unary(operation: UnOp, operand: LibraryConst, ty: Ty) -> Option<LibraryConst> {
    let value = match (operation, operand.value) {
        (
            UnOp::Plus,
            value @ (LibConst::Int(_)
            | LibConst::Long(_)
            | LibConst::Float(_)
            | LibConst::Double(_)),
        ) => value,
        (UnOp::Neg, LibConst::Int(value)) => LibConst::Int(value.wrapping_neg()),
        (UnOp::Neg, LibConst::Long(value)) => LibConst::Long(value.wrapping_neg()),
        (UnOp::Neg, LibConst::Float(value)) => LibConst::Float(-value),
        (UnOp::Neg, LibConst::Double(value)) => LibConst::Double(-value),
        (UnOp::Not, LibConst::Int(value)) if operand.ty.non_null() == Ty::Boolean => {
            LibConst::Int(i32::from(value == 0))
        }
        _ => return None,
    };
    Some(LibraryConst { ty, value })
}

fn evaluate_numeric_conversion(operand: LibraryConst, ty: Ty) -> Option<LibraryConst> {
    let target = ty.non_null();
    let value = match target {
        Ty::Byte | Ty::Short | Ty::Int | Ty::Char | Ty::UByte | Ty::UShort | Ty::UInt => {
            let value = match operand.value {
                LibConst::Int(value) => value,
                LibConst::Long(value) => value as i32,
                LibConst::Float(value) => value as i32,
                LibConst::Double(value) => value as i32,
                LibConst::Str(_) => return None,
            };
            LibConst::Int(match target {
                Ty::Byte | Ty::UByte => i32::from(value as i8),
                Ty::Short | Ty::UShort => i32::from(value as i16),
                Ty::Char => i32::from(value as u16),
                Ty::Int | Ty::UInt => value,
                _ => unreachable!("guard admits only 32-bit constant representations"),
            })
        }
        Ty::Long | Ty::ULong => LibConst::Long(match operand.value {
            LibConst::Int(value) => i64::from(value),
            LibConst::Long(value) => value,
            LibConst::Float(value) => value as i64,
            LibConst::Double(value) => value as i64,
            LibConst::Str(_) => return None,
        }),
        Ty::Float => LibConst::Float(match operand.value {
            LibConst::Int(value) => value as f32,
            LibConst::Long(value) => value as f32,
            LibConst::Float(value) => value,
            LibConst::Double(value) => value as f32,
            LibConst::Str(_) => return None,
        }),
        Ty::Double => LibConst::Double(match operand.value {
            LibConst::Int(value) => f64::from(value),
            LibConst::Long(value) => value as f64,
            LibConst::Float(value) => f64::from(value),
            LibConst::Double(value) => value,
            LibConst::Str(_) => return None,
        }),
        _ => return None,
    };
    Some(LibraryConst { ty, value })
}

fn arithmetic_numeric(
    operation: PrimitiveBinaryIntrinsic,
    left: LibraryConst,
    right: LibraryConst,
    ty: Ty,
) -> Option<LibraryConst> {
    let target = ty.non_null();
    let value = match target {
        Ty::Byte | Ty::Short | Ty::Int | Ty::Char | Ty::UByte | Ty::UShort | Ty::UInt => {
            let left = constant_i32(&left)?;
            let right = constant_i32(&right)?;
            LibConst::Int(match operation {
                PrimitiveBinaryIntrinsic::Add => left.wrapping_add(right),
                PrimitiveBinaryIntrinsic::Subtract => left.wrapping_sub(right),
                PrimitiveBinaryIntrinsic::Multiply => left.wrapping_mul(right),
                PrimitiveBinaryIntrinsic::Divide if target == Ty::UInt => {
                    (left as u32).checked_div(right as u32)? as i32
                }
                PrimitiveBinaryIntrinsic::Remainder if target == Ty::UInt => {
                    (left as u32).checked_rem(right as u32)? as i32
                }
                PrimitiveBinaryIntrinsic::Divide => left
                    .checked_div(right)
                    .or_else(|| (left == i32::MIN && right == -1).then_some(i32::MIN))?,
                PrimitiveBinaryIntrinsic::Remainder => left
                    .checked_rem(right)
                    .or_else(|| (left == i32::MIN && right == -1).then_some(0))?,
            })
        }
        Ty::Long | Ty::ULong => {
            let left = constant_i64(&left)?;
            let right = constant_i64(&right)?;
            LibConst::Long(match operation {
                PrimitiveBinaryIntrinsic::Add => left.wrapping_add(right),
                PrimitiveBinaryIntrinsic::Subtract => left.wrapping_sub(right),
                PrimitiveBinaryIntrinsic::Multiply => left.wrapping_mul(right),
                PrimitiveBinaryIntrinsic::Divide if target == Ty::ULong => {
                    (left as u64).checked_div(right as u64)? as i64
                }
                PrimitiveBinaryIntrinsic::Remainder if target == Ty::ULong => {
                    (left as u64).checked_rem(right as u64)? as i64
                }
                PrimitiveBinaryIntrinsic::Divide => left
                    .checked_div(right)
                    .or_else(|| (left == i64::MIN && right == -1).then_some(i64::MIN))?,
                PrimitiveBinaryIntrinsic::Remainder => left
                    .checked_rem(right)
                    .or_else(|| (left == i64::MIN && right == -1).then_some(0))?,
            })
        }
        Ty::Float => {
            let left = constant_f32(&left)?;
            let right = constant_f32(&right)?;
            LibConst::Float(match operation {
                PrimitiveBinaryIntrinsic::Add => left + right,
                PrimitiveBinaryIntrinsic::Subtract => left - right,
                PrimitiveBinaryIntrinsic::Multiply => left * right,
                PrimitiveBinaryIntrinsic::Divide => left / right,
                PrimitiveBinaryIntrinsic::Remainder => left % right,
            })
        }
        Ty::Double => {
            let left = constant_f64(&left)?;
            let right = constant_f64(&right)?;
            LibConst::Double(match operation {
                PrimitiveBinaryIntrinsic::Add => left + right,
                PrimitiveBinaryIntrinsic::Subtract => left - right,
                PrimitiveBinaryIntrinsic::Multiply => left * right,
                PrimitiveBinaryIntrinsic::Divide => left / right,
                PrimitiveBinaryIntrinsic::Remainder => left % right,
            })
        }
        _ => return None,
    };
    Some(LibraryConst { ty, value })
}

fn compare_numeric(
    operation: BinOp,
    left: LibraryConst,
    right: LibraryConst,
) -> Option<LibraryConst> {
    let order = match (left.value, right.value) {
        (LibConst::Int(left), LibConst::Int(right)) => left.partial_cmp(&right),
        (LibConst::Long(left), LibConst::Long(right)) => left.partial_cmp(&right),
        (LibConst::Float(left), LibConst::Float(right)) => left.partial_cmp(&right),
        (LibConst::Double(left), LibConst::Double(right)) => left.partial_cmp(&right),
        _ => None,
    }?;
    let value = match operation {
        BinOp::Lt => order.is_lt(),
        BinOp::Le => order.is_le(),
        BinOp::Gt => order.is_gt(),
        BinOp::Ge => order.is_ge(),
        _ => return None,
    };
    Some(boolean_constant(value))
}

fn evaluate_non_call_binary(
    operation: BinOp,
    left: LibraryConst,
    right: LibraryConst,
) -> Option<LibraryConst> {
    match operation {
        BinOp::And | BinOp::Or
            if left.ty.non_null() == Ty::Boolean && right.ty.non_null() == Ty::Boolean =>
        {
            let left = constant_i32(&left)? != 0;
            let right = constant_i32(&right)? != 0;
            Some(boolean_constant(if operation == BinOp::And {
                left && right
            } else {
                left || right
            }))
        }
        BinOp::Eq | BinOp::Ne => {
            let equal = left.value == right.value;
            Some(boolean_constant(if operation == BinOp::Eq {
                equal
            } else {
                !equal
            }))
        }
        _ => None,
    }
}

fn boolean_constant(value: bool) -> LibraryConst {
    LibraryConst {
        ty: Ty::Boolean,
        value: LibConst::Int(i32::from(value)),
    }
}

fn constant_i32(constant: &LibraryConst) -> Option<i32> {
    match constant.value {
        LibConst::Int(value) => Some(value),
        _ => None,
    }
}

fn constant_i64(constant: &LibraryConst) -> Option<i64> {
    match constant.value {
        LibConst::Int(value) => Some(i64::from(value)),
        LibConst::Long(value) => Some(value),
        _ => None,
    }
}

fn constant_f32(constant: &LibraryConst) -> Option<f32> {
    match constant.value {
        LibConst::Int(value) => Some(value as f32),
        LibConst::Long(value) => Some(value as f32),
        LibConst::Float(value) => Some(value),
        _ => None,
    }
}

fn constant_f64(constant: &LibraryConst) -> Option<f64> {
    match constant.value {
        LibConst::Int(value) => Some(f64::from(value)),
        LibConst::Long(value) => Some(value as f64),
        LibConst::Float(value) => Some(f64::from(value)),
        LibConst::Double(value) => Some(value),
        _ => None,
    }
}

fn push_constant_string(constant: &LibraryConst, output: &mut KtStringBuf) -> Option<()> {
    match &constant.value {
        LibConst::Str(value) => output.push_kt(value),
        LibConst::Int(value) => match constant.ty.non_null() {
            Ty::Boolean => output.push_str(if *value == 0 { "false" } else { "true" }),
            Ty::Char => output.push_unit(u16::try_from(*value).ok()?),
            Ty::UInt => output.push_str(&(*value as u32).to_string()),
            _ => output.push_str(&value.to_string()),
        },
        LibConst::Long(value) => {
            if constant.ty.non_null() == Ty::ULong {
                output.push_str(&(*value as u64).to_string());
            } else {
                output.push_str(&value.to_string());
            }
        }
        LibConst::Float(value) => push_float_string(f64::from(*value), output),
        LibConst::Double(value) => push_float_string(*value, output),
    }
    Some(())
}

fn push_float_string(value: f64, output: &mut KtStringBuf) {
    if value.is_nan() {
        output.push_str("NaN");
    } else if value == f64::INFINITY {
        output.push_str("Infinity");
    } else if value == f64::NEG_INFINITY {
        output.push_str("-Infinity");
    } else {
        output.push_str(&value.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_extraction_remains_narrow() {
        let file = crate::frontend::parse_source(
            "val ordinary = 2 + 2",
            &crate::features::LangFeatures::new(),
            &mut crate::diag::DiagSink::new(),
        );
        let crate::ast::Decl::Property(property) = file.decl(file.decls[0]) else {
            panic!("property declaration")
        };
        assert!(source_literal_constant(&file, property.init.unwrap(), Ty::Int).is_none());
    }

    #[test]
    fn numeric_constant_conversion_uses_kotlin_narrowing() {
        assert_eq!(
            evaluate_numeric_conversion(
                LibraryConst {
                    ty: Ty::Int,
                    value: LibConst::Int(0x1_0002),
                },
                Ty::Short,
            ),
            Some(LibraryConst {
                ty: Ty::Short,
                value: LibConst::Int(2),
            }),
        );
        assert_eq!(
            evaluate_numeric_conversion(
                LibraryConst {
                    ty: Ty::Double,
                    value: LibConst::Double(2.75),
                },
                Ty::Float,
            ),
            Some(LibraryConst {
                ty: Ty::Float,
                value: LibConst::Float(2.75),
            }),
        );
    }
}
