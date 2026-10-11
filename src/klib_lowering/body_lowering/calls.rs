//! Calls of compiler built-in operators and of dependency functions.
//!
//! A built-in operator lowers as checked FIR lowers the source operator the KLIB serializes as a
//! call of it; a dependency function lowers as a call of its function in the same unit.

use super::super::ir_builtins::{BuiltinOperator, BuiltinRelation};
use super::{mismatch, unsupported, BodyLowering, Lowered};
use crate::ir::{Callee, ExprId, IrBinOp, IrExpr};
use crate::metadata::id_signature::KlibPublicIdSignature;
use crate::metadata::klib_ir::tree::{
    KlibIrArguments, KlibIrExprId, KlibIrExprKind, KlibIrMemberAccess,
};
use crate::metadata::klib_ir::{KlibIrSignature, KlibIrSymbolKind};
use crate::types::{EqualityMode, Ty};

use super::super::decline::KlibBodyDeclineReason;

impl BodyLowering<'_, '_, '_> {
    /// A call of a compiler built-in operator or of a dependency function. A built-in operator
    /// lowers as checked FIR lowers the source operator it serializes; a dependency function
    /// lowers as a call of its function in the same unit.
    pub(super) fn call(&mut self, access: &KlibIrMemberAccess, ty: Ty) -> Lowered<ExprId> {
        if access.symbol.kind != KlibIrSymbolKind::Function {
            return Err(mismatch("a call target's symbol is not a function"));
        }
        let KlibIrSignature::Public(callee) = &access.symbol.signature else {
            return unsupported("a call of a file-private declaration");
        };
        let Some(operator) = self.builtins.operator(callee) else {
            if !access.type_arguments.is_empty() {
                // Only a generic declaration takes type arguments, and lowering declines those.
                return unsupported("a call of a generic declaration");
            }
            return self.dependency_call(callee, access, ty);
        };
        if !access.type_arguments.is_empty() {
            return Err(mismatch(
                "a built-in operator is called with type arguments",
            ));
        }
        match operator {
            BuiltinOperator::Relation(relation) => self.relation(access, relation, ty),
            BuiltinOperator::Not => self.negation(access, ty),
            equality => self.equality(access, equality, ty, false),
        }
    }

    /// A primitive relation over two operands of one primitive type. On `Float` and `Double` that
    /// relation is the IEEE one, as the built-in is.
    fn relation(
        &mut self,
        access: &KlibIrMemberAccess,
        relation: BuiltinRelation,
        ty: Ty,
    ) -> Lowered<ExprId> {
        let (lhs, rhs) = binary_operands(access, ty)?;
        let lhs = self.operand(lhs, relation.operand)?;
        let rhs = self.operand(rhs, relation.operand)?;
        Ok(self.ir.add_expr(IrExpr::PrimitiveBinOp {
            op: relation.operator,
            lhs,
            rhs,
        }))
    }

    /// `==` (`EQEQ`, or `ieee754equals` on two floating-point operands) or `===` (`EQEQEQ`), or
    /// their negation when `negated`. Checked FIR lowering keeps a source `==` as an equality whose
    /// mode the checker derives from the operand types, and a source `===` as the reference
    /// comparison; the KLIB serializer chose its built-in from the same operand types, so the two
    /// must agree.
    fn equality(
        &mut self,
        access: &KlibIrMemberAccess,
        operator: BuiltinOperator,
        ty: Ty,
        negated: bool,
    ) -> Lowered<ExprId> {
        let (lhs, rhs) = binary_operands(access, ty)?;
        let lhs = self.expression(lhs)?;
        let rhs = self.expression(rhs)?;
        let mode = EqualityMode::of_source_operands(self.lowered_type(lhs), self.lowered_type(rhs));
        let op = match (negated, operator) {
            (false, BuiltinOperator::Identical) => IrBinOp::RefEq,
            (true, BuiltinOperator::Identical) => IrBinOp::RefNe,
            (false, _) => IrBinOp::Eq,
            (true, _) => IrBinOp::Ne,
        };
        match (operator, mode) {
            (BuiltinOperator::Identical, _) => {
                Ok(self.ir.add_expr(IrExpr::PrimitiveBinOp { op, lhs, rhs }))
            }
            (BuiltinOperator::Equals, EqualityMode::Ieee754) => Err(mismatch(
                "`EQEQ` compares two operands of one floating-point type",
            )),
            (
                BuiltinOperator::Ieee754Equals { .. },
                EqualityMode::Primitive | EqualityMode::Structural,
            ) => unsupported("an IEEE 754 equality of operands other than one floating-point type"),
            (BuiltinOperator::Ieee754Equals { operand }, EqualityMode::Ieee754)
                if self.lowered_type(lhs) != operand =>
            {
                Err(mismatch(
                    "`ieee754equals` compares operands of another type than its own",
                ))
            }
            (BuiltinOperator::Equals | BuiltinOperator::Ieee754Equals { .. }, mode) => {
                Ok(self.ir.add_expr(IrExpr::Equality { op, mode, lhs, rhs }))
            }
            (BuiltinOperator::Relation(_) | BuiltinOperator::Not, _) => {
                unreachable!("only an equality operator is lowered as an equality")
            }
        }
    }

    /// `!` on a `Boolean`. A KLIB serializes a source `!=` or `!==` as the negation of the
    /// equality, marked with the source operator's origin; checked FIR lowering keeps that source
    /// operator as one negated equality, so it lowers as one.
    fn negation(&mut self, access: &KlibIrMemberAccess, ty: Ty) -> Lowered<ExprId> {
        let (&[Some(operand)], true) = (arguments(access).as_slice(), ty == Ty::Boolean) else {
            return Err(mismatch(
                "`Boolean.not` is called with another shape than its declaration's",
            ));
        };
        match access.origin.as_deref() {
            None => {
                let operand = self.operand(operand, Ty::Boolean)?;
                Ok(self.ir.add_negation(operand))
            }
            Some("EXCLEQ" | "EXCLEQEQ") => {
                let expression = self.arena.expr(operand);
                let equality = match &expression.kind {
                    KlibIrExprKind::Call {
                        access,
                        super_qualifier: None,
                    } => match &access.symbol.signature {
                        KlibIrSignature::Public(callee) => self
                            .builtins
                            .operator(callee)
                            .filter(|operator| {
                                !matches!(
                                    operator,
                                    BuiltinOperator::Relation(_) | BuiltinOperator::Not
                                )
                            })
                            .map(|operator| (access, operator)),
                        _ => None,
                    },
                    _ => None,
                };
                let Some((access, operator)) = equality else {
                    return Err(mismatch("a `!=` negates no built-in equality"));
                };
                let inner = self.ty(expression, "call")?;
                self.equality(access, operator, inner, true)
            }
            Some(_) => unsupported("a negation of another form than `!`, `!=` or `!==`"),
        }
    }

    /// A call of a dependency function, which the unit lowers too.
    fn dependency_call(
        &mut self,
        callee: &KlibPublicIdSignature,
        access: &KlibIrMemberAccess,
        ty: Ty,
    ) -> Lowered<ExprId> {
        let linked = self.linker.link(self.ir, callee)?;
        if linked.context_count != 0 && matches!(access.arguments, KlibIrArguments::Split { .. }) {
            return unsupported("a context argument in the pre-2.4 argument layout");
        }
        if ty != linked.ret {
            return Err(mismatch("a call is typed apart from its callee's result"));
        }
        let arguments = arguments(access);
        if arguments.len() != linked.params.len() {
            return Err(mismatch(
                "a call passes another number of arguments than its callee declares",
            ));
        }
        let mut args = Vec::with_capacity(arguments.len());
        for (argument, parameter) in arguments.into_iter().zip(linked.params) {
            let argument = argument.ok_or(KlibBodyDeclineReason::UnsupportedOperation(
                "a call that leaves a parameter to its default",
            ))?;
            args.push(self.operand(argument, parameter)?);
        }
        Ok(self.ir.add_expr(IrExpr::Call {
            callee: Callee::Local(linked.function),
            dispatch_receiver: None,
            args,
        }))
    }
}

/// A call's arguments in the order of its callee's parameters: dispatch receiver, context
/// parameters, extension receiver, regular parameters. A KLIB older than Kotlin 2.4 keeps the
/// receivers apart and the context arguments among the values; its context arguments are not
/// placed here, and a caller of a declaration with context parameters declines that layout.
pub(super) fn arguments(access: &KlibIrMemberAccess) -> Vec<Option<KlibIrExprId>> {
    match &access.arguments {
        KlibIrArguments::Flat(arguments) => arguments.clone(),
        KlibIrArguments::Split {
            dispatch_receiver,
            extension_receiver,
            values,
        } => dispatch_receiver
            .iter()
            .chain(extension_receiver)
            .map(|receiver| Some(*receiver))
            .chain(values.iter().copied())
            .collect(),
    }
}

/// The two operands of a built-in binary operator answering `ty`, which must be `Boolean`.
fn binary_operands(access: &KlibIrMemberAccess, ty: Ty) -> Lowered<(KlibIrExprId, KlibIrExprId)> {
    match (arguments(access).as_slice(), ty == Ty::Boolean) {
        (&[Some(lhs), Some(rhs)], true) => Ok((lhs, rhs)),
        _ => Err(mismatch(
            "a built-in operator is called with another shape than its declaration's",
        )),
    }
}
