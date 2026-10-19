//! Calls of compiler built-in operators and of dependency functions.
//!
//! A built-in operator lowers as checked FIR lowers the source operator the KLIB serializes as a
//! call of it; a dependency function lowers as a call of its function in the same unit.

use super::super::ir_builtins::{BuiltinOperator, BuiltinRelation};
use super::super::klib_types::function_invocation_arity;
use super::{mismatch, unsupported, BodyLowering, LinkedCallee, Lowered};
use crate::ir::{ExprId, IrBinOp, IrExpr};
use crate::metadata::id_signature::KlibPublicIdSignature;
use crate::metadata::klib_ir::tree::{
    KlibIrArguments, KlibIrExprId, KlibIrExprKind, KlibIrMemberAccess,
};
use crate::metadata::klib_ir::{KlibIrSignature, KlibIrSymbolKind};
use crate::types::{EqualityMode, Ty};

use super::super::decline::KlibBodyDeclineReason;

impl BodyLowering<'_> {
    /// A call of a compiler built-in operator or of a dependency function. A built-in operator
    /// lowers as checked FIR lowers the source operator it serializes; a dependency function
    /// lowers as a call of its function in the same unit.
    pub(super) fn call(&mut self, access: &KlibIrMemberAccess, ty: Ty) -> Lowered<ExprId> {
        if access.symbol.kind != KlibIrSymbolKind::Function {
            return Err(mismatch("a call target's symbol is not a function"));
        }
        let callee = match &access.symbol.signature {
            KlibIrSignature::Public(callee) => callee,
            KlibIrSignature::FileLocal { .. } => {
                return unsupported("a call of a file-private declaration")
            }
            KlibIrSignature::Accessor(_) => {
                return unsupported("a call through a property accessor signature")
            }
        };
        if let Some(arity) = function_invocation_arity(callee) {
            if !access.type_arguments.is_empty() {
                return Err(mismatch(
                    "a function invocation is called with type arguments",
                ));
            }
            return self.function_invocation(access, arity, ty);
        }
        let operator = self.builtins.operator(callee);
        // A checked source call may select the same exact declaration that this support body
        // calls. Materialize that declaration into the closed unit so the source edge has target
        // code. Only an unselected compiler builtin is folded into its common-IR operation.
        if operator.is_some() && self.linker.is_selected(callee)? {
            if !access.type_arguments.is_empty() {
                return unsupported("a call of a generic declaration");
            }
            return self.dependency_call(callee, access, ty);
        }
        let Some(operator) = operator else {
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
            BuiltinOperator::IsNan { operand } => self.is_nan(access, operand, ty),
            BuiltinOperator::NumericConversion { from, to } => {
                self.numeric_conversion(access, from, to, ty)
            }
            equality => self.equality(access, equality, ty, false),
        }
    }

    /// `Float.isNaN()` / `Double.isNaN()` as the IEEE fact `value != value`. The dependency body
    /// form admitted here reads a value parameter, so sharing that lowered read between both sides
    /// preserves single evaluation while keeping the operation in common semantic IR.
    fn is_nan(&mut self, access: &KlibIrMemberAccess, operand: Ty, ty: Ty) -> Lowered<ExprId> {
        let &[Some(receiver)] = arguments(access).as_slice() else {
            return Err(mismatch(
                "`isNaN` is called with another shape than its declaration's",
            ));
        };
        if ty != Ty::Boolean
            || !matches!(
                self.arena.expr(receiver).kind,
                KlibIrExprKind::GetValue { .. }
            )
        {
            return Err(mismatch(
                "`isNaN` has another result or receiver form than the realized dependency body",
            ));
        }
        let receiver = self.operand(receiver, operand)?;
        Ok(self.ir.add_expr(IrExpr::Equality {
            op: IrBinOp::Ne,
            mode: EqualityMode::Ieee754,
            lhs: receiver,
            rhs: receiver,
        }))
    }

    /// A numeric conversion member called on its dispatch receiver, lowered as checked FIR lowers
    /// the source `value.toInt()`: a coercion of the receiver to the conversion's result.
    fn numeric_conversion(
        &mut self,
        access: &KlibIrMemberAccess,
        from: Ty,
        to: Ty,
        ty: Ty,
    ) -> Lowered<ExprId> {
        let (&[Some(receiver)], true) = (arguments(access).as_slice(), ty == to) else {
            return Err(mismatch(
                "a numeric conversion is called with another shape than its declaration's",
            ));
        };
        let receiver = self.operand(receiver, from)?;
        Ok(self.ir.add_expr(IrExpr::TypeOp {
            op: crate::ir::IrTypeOp::ImplicitCoercion,
            arg: receiver,
            type_operand: to,
        }))
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
            (
                BuiltinOperator::Relation(_)
                | BuiltinOperator::Not
                | BuiltinOperator::IsNan { .. }
                | BuiltinOperator::NumericConversion { .. },
                _,
            ) => {
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

    /// `f.invoke(arguments)` of a function value `f`, lowered as checked FIR lowers the source
    /// `f(arguments)`: an invocation of the value with the parameter types its function type
    /// declares, typed with the call's result.
    fn function_invocation(
        &mut self,
        access: &KlibIrMemberAccess,
        arity: usize,
        ty: Ty,
    ) -> Lowered<ExprId> {
        let arguments = arguments(access);
        let Some((Some(function), arguments)) = arguments.split_first() else {
            return Err(mismatch("a function invocation has no function value"));
        };
        if arguments.len() != arity {
            return Err(mismatch(
                "a function invocation passes another number of arguments than its arity",
            ));
        }
        let func = self.expression(*function)?;
        let Ty::Fun(signature) = self.lowered_type(func).non_null() else {
            return Err(mismatch(
                "a function invocation's value has no function type",
            ));
        };
        if self.lowered_type(func) != Ty::Fun(signature) {
            return unsupported("an invocation of a nullable function value");
        }
        // A context or extension function type's parameters lead with its context parameters and
        // receiver, as the function class's invocation takes them.
        if signature.params.len() != arity || signature.suspend {
            return Err(mismatch(
                "a function invocation's value has another shape than its function class",
            ));
        }
        // An erased body uses the erasure of the function value's result.
        if signature.ret != ty && self.header.erased(signature.ret) != ty {
            return Err(mismatch(
                "a function invocation is typed apart from its result",
            ));
        }
        let params = signature.params.to_vec();
        let mut args = Vec::with_capacity(arity);
        for (argument, parameter) in arguments.iter().zip(&params) {
            let argument = argument.ok_or(KlibBodyDeclineReason::UnsupportedOperation(
                "a function invocation that omits an argument",
            ))?;
            args.push(self.declared_operand(argument, *parameter)?);
        }
        let invocation = self.ir.add_expr(IrExpr::InvokeFunction {
            func,
            args,
            params,
            ret: signature.ret,
        });
        Ok(if signature.ret == ty {
            invocation
        } else {
            self.ir.logical_types.insert(invocation, signature.ret);
            let erased = self.coercion(invocation, ty);
            self.erasure_coercions.insert(erased);
            erased
        })
    }

    /// A call of a dependency function, which the unit lowers too.
    fn dependency_call(
        &mut self,
        callee: &KlibPublicIdSignature,
        access: &KlibIrMemberAccess,
        ty: Ty,
    ) -> Lowered<ExprId> {
        let linked = self.linker.link(self.ir, callee)?;
        self.linked_call(linked, access, ty)
    }

    fn linked_call(
        &mut self,
        linked: LinkedCallee,
        access: &KlibIrMemberAccess,
        ty: Ty,
    ) -> Lowered<ExprId> {
        if linked.context_count != 0 && matches!(access.arguments, KlibIrArguments::Split { .. }) {
            return unsupported("a context argument in the pre-2.4 argument layout");
        }
        if ty != linked.ret {
            return Err(mismatch("a call is typed apart from its callee's result"));
        }
        let arguments = arguments(access);
        if arguments.len() != linked.params.len() + usize::from(linked.receiver.is_some()) {
            return Err(mismatch(
                "a call passes another number of arguments than its callee declares",
            ));
        }
        let mut arguments = arguments.into_iter();
        let dispatch_receiver = linked
            .receiver
            .map(|expected| {
                let argument = arguments.next().flatten().ok_or(
                    KlibBodyDeclineReason::UnsupportedOperation(
                        "a call that omits its dispatch receiver",
                    ),
                )?;
                self.operand(argument, expected).map(Some)
            })
            .transpose()?
            .flatten();
        let mut args = Vec::with_capacity(linked.params.len());
        for (argument, parameter) in arguments.zip(linked.params) {
            let argument = argument.ok_or(KlibBodyDeclineReason::UnsupportedOperation(
                "a call that leaves a parameter to its default",
            ))?;
            args.push(self.operand(argument, parameter)?);
        }
        let singleton_dispatch = linked.singleton_dispatch;
        let call = self.ir.add_expr(IrExpr::Call {
            callee: linked.callee,
            dispatch_receiver,
            args,
        });
        if let Some(classifier) = singleton_dispatch {
            self.ir
                .external_singleton_dispatches
                .insert(call, classifier);
        }
        Ok(call)
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
