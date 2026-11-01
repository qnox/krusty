//! Calls of compiler built-in operators and of dependency functions.
//!
//! A built-in operator lowers as checked FIR lowers the source operator the KLIB serializes as a
//! call of it; a dependency function lowers as a call of its function in the same unit.

use super::super::ir_builtins::{BuiltinCallDisposition, BuiltinOperator, BuiltinRelation};
use super::super::klib_types::function_invocation_arity;
use super::super::klib_types::semantic_type;
use super::{mismatch, unsupported, BodyLowering, LinkedAccessor, LinkedCallee, Lowered};
use crate::fir::FirTypeParameterRef;
use crate::ir::{
    Callee, ExprId, IrBinOp, IrCheckedOperation, IrCheckedSubstitution, IrExpr, IrPropertyDispatch,
};
use crate::metadata::id_signature::{KlibAccessorIdSignature, KlibPublicIdSignature};
use crate::metadata::klib_ir::tree::{
    KlibIrArguments, KlibIrExprId, KlibIrExprKind, KlibIrMemberAccess,
};
use crate::metadata::klib_ir::{KlibIrSignature, KlibIrSymbolKind};
use crate::types::{EqualityMode, Ty, TyKind};

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
            KlibIrSignature::Accessor(accessor) => return self.accessor_call(accessor, access, ty),
        };
        if let Some(arity) = function_invocation_arity(callee) {
            if !access.type_arguments.is_empty() {
                return Err(mismatch(
                    "a function invocation is called with type arguments",
                ));
            }
            return self.function_invocation(access, arity, ty);
        }
        // A checked source call or retained external export needs the exact declaration edge.
        // Only a builtin without either obligation is folded into its common-IR operation.
        let operator = match self
            .builtins
            .call_disposition(callee, || self.linker.requires_declaration(callee))?
        {
            BuiltinCallDisposition::Common(operator) => operator,
            BuiltinCallDisposition::RetainedDeclaration => {
                if !access.type_arguments.is_empty() {
                    return unsupported("a call of a generic declaration");
                }
                return self.dependency_call(callee, access, ty);
            }
            BuiltinCallDisposition::Dependency => {
                let signature = crate::libraries::KlibDeclarationSignature::Public(callee.clone());
                if let Some(primitive) = self.linker.common_primitive(&signature)? {
                    return self.primitive_call(primitive, access, ty);
                }
                return self.dependency_call(callee, access, ty);
            }
        };
        if operator == BuiltinOperator::CheckNotNull {
            return self.not_null_assertion(access, ty);
        }
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

    fn primitive_call(
        &mut self,
        primitive: super::super::primitive_calls::CommonPrimitiveCall,
        access: &KlibIrMemberAccess,
        ty: Ty,
    ) -> Lowered<ExprId> {
        use super::super::primitive_calls::CommonPrimitive;
        if !access.type_arguments.is_empty() {
            return Err(mismatch("a common primitive is called with type arguments"));
        }
        let operands = self.call_operands(
            access,
            ty,
            CalleeShape {
                receiver: Some(primitive.receiver),
                params: &primitive.params,
                ret: primitive.ret,
                context_count: 0,
                class_type_parameters: &primitive.class_type_parameters,
            },
        )?;
        let receiver = operands
            .receiver
            .expect("a validated primitive has dispatch");
        Ok(match primitive.operation {
            CommonPrimitive::Unary(operation) => {
                // Match checked FIR's selected promotion before unary realization. In particular,
                // negate is an IEEE unary operation, never subtraction from positive zero.
                let operand = if primitive.receiver == primitive.ret {
                    receiver
                } else {
                    self.coercion(receiver, primitive.ret)
                };
                match operation {
                    crate::libraries::PrimitiveUnaryIntrinsic::Identity => operand,
                    crate::libraries::PrimitiveUnaryIntrinsic::Negate => {
                        self.ir.add_expr(IrExpr::PrimitiveNeg {
                            operand,
                            ty: primitive.ret,
                        })
                    }
                }
            }
            CommonPrimitive::Intrinsic(intrinsic) => self.ir.add_expr(IrExpr::Call {
                callee: Box::new(Callee::Intrinsic {
                    operation: intrinsic,
                    ret: primitive.ret,
                }),
                dispatch_receiver: Some(receiver),
                args: Vec::new(),
            }),
            // The source range operator over the receiver and the end bound, as checked FIR
            // lowers `start..end` once the provider's declaration fixed the range it builds.
            CommonPrimitive::RangeConstruction(operation) => {
                let (&[end], &[end_type]) = (operands.args.as_slice(), operands.params.as_slice())
                else {
                    return Err(mismatch(
                        "a range construction is called with another shape than its declaration's",
                    ));
                };
                self.ir.add_expr(IrExpr::Checked(Box::new(
                    IrCheckedOperation::RangeConstruction {
                        operation,
                        start: receiver,
                        start_type: primitive.receiver,
                        end,
                        end_type,
                        result: primitive.ret,
                    },
                )))
            }
        })
    }

    /// `operand!!`, lowered as checked FIR lowers the source assertion: the operand, checked for
    /// `null`, typed as its non-null type.
    fn not_null_assertion(&mut self, access: &KlibIrMemberAccess, ty: Ty) -> Lowered<ExprId> {
        let (&[Some(operand)], 1) = (arguments(access).as_slice(), access.type_arguments.len())
        else {
            return Err(mismatch(
                "`CHECK_NOT_NULL` is called with another shape than its declaration's",
            ));
        };
        let operand = self.expression(operand)?;
        if self.lowered_type(operand).non_null() != ty || ty.is_nullable() {
            return Err(mismatch(
                "`CHECK_NOT_NULL` is typed apart from its operand's non-null type",
            ));
        }
        Ok(self.ir.add_expr(IrExpr::NotNullAssert {
            operand,
            check: crate::ir::NullCheck::Source,
        }))
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
                | BuiltinOperator::NumericConversion { .. }
                | BuiltinOperator::CheckNotNull,
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
            Some("NOT_IN") => {
                let expression = self.arena.expr(operand);
                let membership = match super::super::range_membership::range_literal_membership(
                    self.arena, expression,
                ) {
                    Some(membership)
                        if membership.membership.origin.as_deref() == Some("NOT_IN") =>
                    {
                        let inner = self.ty(expression, "call")?;
                        self.range_literal_membership(membership, inner, true)?
                    }
                    _ => None,
                };
                membership.map_or_else(
                    || unsupported("a `!in` of another form than a membership in a range literal"),
                    Ok,
                )
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
        let TyKind::Fun(signature) = self.lowered_type(func).non_null().kind() else {
            return Err(mismatch(
                "a function invocation's value has no function type",
            ));
        };
        if self.lowered_type(func).kind() != TyKind::Fun(signature) {
            return unsupported("an invocation of a nullable function value");
        }
        // A context or extension function type's parameters lead with its context parameters and
        // receiver, as the function class's invocation takes them.
        if signature.params.len() != arity || signature.suspend {
            return Err(mismatch(
                "a function invocation's value has another shape than its function class",
            ));
        }
        // An erased body uses the erasure of the function value's result, or the bound an
        // already expanded inline function's type parameter erased to.
        if signature.ret != ty
            && self.header.erased(signature.ret) != ty
            && !self
                .linker
                .is_subtype(self.header.type_parameters(), signature.ret, ty)
        {
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
        let linked = self.substituted(linked, access)?;
        let operands = self.call_operands(
            access,
            ty,
            CalleeShape {
                receiver: linked.receiver,
                params: &linked.params,
                ret: linked.ret,
                context_count: linked.context_count,
                class_type_parameters: &linked.class_type_parameters,
            },
        )?;
        let mut callee = linked.callee;
        if let Callee::External { params, ret, .. } = &mut callee {
            (*params, *ret) = (operands.params, operands.ret);
        }
        let declared_ret = match callee {
            Callee::Local(function) => Some(self.ir.functions[function as usize].ret),
            _ => None,
        };
        let (dispatch_receiver, args) = (operands.receiver, operands.args);
        let singleton_dispatch = linked.singleton_dispatch;
        let call = self.ir.add_expr(IrExpr::Call {
            callee: Box::new(callee),
            dispatch_receiver,
            args,
        });
        if let Some(classifier) = singleton_dispatch {
            self.ir
                .external_singleton_dispatches
                .insert(call, classifier);
        }
        // The result of a generic local function is its declared type parameter; the call's
        // value is that result at the call's type arguments.
        Ok(match declared_ret {
            Some(declared) if declared != operands.ret => {
                let coerced = self.coercion(call, operands.ret);
                self.ir.logical_types.insert(coerced, operands.ret);
                coerced
            }
            _ => call,
        })
    }
}

impl BodyLowering<'_> {
    /// A call of a dependency property's accessor, lowered as checked FIR lowers a source access
    /// of the property: a read through its getter, a write through its setter.
    fn accessor_call(
        &mut self,
        accessor: &KlibAccessorIdSignature,
        access: &KlibIrMemberAccess,
        ty: Ty,
    ) -> Lowered<ExprId> {
        if !access.type_arguments.is_empty() {
            return unsupported("an access of a generic property");
        }
        let signature = crate::libraries::KlibDeclarationSignature::Accessor(accessor.clone());
        if let Some(primitive) = self.linker.common_primitive(&signature)? {
            return self.primitive_call(primitive, access, ty);
        }
        let linked = self.linker.link_accessor(accessor)?;
        let CallOperands {
            receiver,
            args: arguments,
            receiver_type: source_receiver,
            params: parameters,
            ret: result,
        } = self.call_operands(
            access,
            ty,
            CalleeShape {
                receiver: linked.receiver,
                params: &linked.params,
                ret: linked.ret,
                context_count: linked.context_count,
                class_type_parameters: &linked.class_type_parameters,
            },
        )?;
        let LinkedAccessor {
            property: target,
            write,
            ..
        } = linked;
        let dispatch = IrPropertyDispatch::Ordinary;
        Ok(self.ir.add_expr(IrExpr::Checked(Box::new(if write {
            IrCheckedOperation::ExternalPropertyWrite {
                target,
                dispatch,
                receiver,
                arguments,
                parameters,
                result,
                source_receiver,
            }
        } else {
            IrCheckedOperation::ExternalPropertyRead {
                target,
                dispatch,
                receiver,
                arguments,
                parameters,
                result,
                source_receiver,
            }
        }))))
    }

    /// A call's dispatch receiver and arguments, each read at the callee's declared type, after
    /// checking that the call has the callee's result type and passes every parameter. A member
    /// of a generic class is read at its class's type arguments, which the dispatch receiver's type
    /// applies to the class through its hierarchy, as a source call of the member is checked.
    fn call_operands(
        &mut self,
        access: &KlibIrMemberAccess,
        ty: Ty,
        callee: CalleeShape<'_>,
    ) -> Lowered<CallOperands> {
        if callee.context_count != 0 && matches!(access.arguments, KlibIrArguments::Split { .. }) {
            return unsupported("a context argument in the pre-2.4 argument layout");
        }
        let arguments = arguments(access);
        if arguments.len() != callee.params.len() + usize::from(callee.receiver.is_some()) {
            return Err(mismatch(
                "a call passes another number of arguments than its callee declares",
            ));
        }
        let mut arguments = arguments.into_iter();
        let mut bindings = crate::fx_hash::HashMap::default();
        let (receiver, receiver_type) = match callee.receiver {
            None => (None, None),
            Some(declared) => {
                let argument = arguments.next().flatten().ok_or(
                    KlibBodyDeclineReason::UnsupportedOperation(
                        "a call that omits its dispatch receiver",
                    ),
                )?;
                if callee.class_type_parameters.is_empty() {
                    (Some(self.operand(argument, declared)?), Some(declared))
                } else {
                    let lowered = self.expression(argument)?;
                    let applied = self.class_arguments(lowered, declared)?;
                    bindings.extend(
                        callee
                            .class_type_parameters
                            .iter()
                            .cloned()
                            .zip(applied.type_args().iter().copied()),
                    );
                    (Some(lowered), Some(applied))
                }
            }
        };
        // Without class bindings the shape is already at the call's type arguments, which may
        // name the caller's own type parameters; erasing them would lose the call's types.
        let substitute = |declared: Ty| {
            if bindings.is_empty() {
                declared
            } else {
                declared.substitute_erased(&bindings)
            }
        };
        // A result read through a projected receiver argument (`Array<out T>.get`) is that
        // projection's read type, as a source call's result is.
        let ret = substitute(callee.ret).projection_read_ty();
        if ty != ret {
            return Err(mismatch("a call is typed apart from its callee's result"));
        }
        let params = callee
            .params
            .iter()
            .map(|&param| substitute(param))
            .collect::<Vec<_>>();
        let mut args = Vec::with_capacity(params.len());
        for (argument, &parameter) in arguments.zip(&params) {
            let argument = argument.ok_or(KlibBodyDeclineReason::UnsupportedOperation(
                "a call that leaves a parameter to its default",
            ))?;
            args.push(self.operand(argument, parameter)?);
        }
        Ok(CallOperands {
            receiver,
            args,
            receiver_type,
            params,
            ret,
        })
    }

    /// The generic class `declared` names, applied to the type arguments the dispatch receiver
    /// `lowered` applies to it through its hierarchy.
    fn class_arguments(&self, lowered: ExprId, declared: Ty) -> Lowered<Ty> {
        let actual = self.lowered_type(lowered);
        let owner = declared
            .kotlin_class_internal()
            .ok_or_else(|| mismatch("a generic member's receiver is not a class"))?;
        if actual.is_nullable() {
            return Err(KlibBodyDeclineReason::ImplicitConversion);
        }
        let applied = self
            .linker
            .applied_supertype(actual, owner)
            .ok_or(KlibBodyDeclineReason::ImplicitConversion)?;
        if applied.type_args().len() != declared.type_args().len() {
            return Err(mismatch(
                "a generic member's receiver applies another number of type arguments",
            ));
        }
        Ok(applied)
    }

    /// `linked` at the call's type arguments: each of the callee's type parameters bound to the
    /// argument the call serializes for it, as checked FIR lowering records a source call of the
    /// generic declaration, with the same substituted parameters, result and substitutions.
    fn substituted(
        &self,
        mut linked: LinkedCallee,
        access: &KlibIrMemberAccess,
    ) -> Lowered<LinkedCallee> {
        if linked.type_parameters.is_empty() && !access.type_arguments.is_empty() {
            // A linker that does not model the callee's type parameters links no generic call.
            return unsupported("a call of a generic declaration");
        }
        if access.type_arguments.len() != linked.type_parameters.len() {
            return Err(mismatch(
                "a call passes another number of type arguments than its callee declares",
            ));
        }
        if linked.type_parameters.is_empty() {
            return Ok(linked);
        }
        if let Callee::Local(_) = linked.callee {
            // A call of a function the unit declares binds its type parameters as checked FIR
            // lowers a source call of a module generic function: the arguments are read at the
            // substituted parameters and the declared result is coerced to the substituted one,
            // with no substitutions recorded on the call.
            let mut bindings = crate::fx_hash::HashMap::default();
            for (parameter, argument) in linked.type_parameters.iter().zip(&access.type_arguments) {
                let argument = argument.ok_or(KlibBodyDeclineReason::UnsupportedOperation(
                    "a call that leaves a type argument unset",
                ))?;
                let value = semantic_type(self.arena, self.header.type_parameters(), argument)?;
                bindings.insert(parameter.clone(), value);
            }
            for param in &mut linked.params {
                *param = param.substitute_erased(&bindings);
            }
            linked.ret = linked.ret.substitute_erased(&bindings);
            linked.receiver = linked
                .receiver
                .map(|receiver| receiver.substitute_erased(&bindings));
            return Ok(linked);
        }
        let Callee::External {
            target,
            params,
            ret,
            substitutions,
            ..
        } = &mut linked.callee
        else {
            return Err(mismatch("a generic callee is not a dependency function"));
        };
        let mut bindings = crate::fx_hash::HashMap::default();
        for (ordinal, (parameter, argument)) in linked
            .type_parameters
            .iter()
            .zip(&access.type_arguments)
            .enumerate()
        {
            let argument = argument.ok_or(KlibBodyDeclineReason::UnsupportedOperation(
                "a call that leaves a type argument unset",
            ))?;
            let value = semantic_type(self.arena, self.header.type_parameters(), argument)?;
            bindings.insert(parameter.clone(), value);
            substitutions.push(IrCheckedSubstitution {
                parameter: FirTypeParameterRef::External {
                    callable: *target,
                    ordinal: ordinal as u32,
                },
                // A reified type parameter belongs to an inline function, which a template does
                // not call.
                reified: false,
                value,
                reified_runtime: value,
                additional_bounds: Vec::new(),
            });
        }
        for param in params.iter_mut() {
            *param = param.substitute_erased(&bindings);
        }
        *ret = ret.substitute_erased(&bindings);
        linked.params = params.clone();
        linked.ret = *ret;
        linked.receiver = linked
            .receiver
            .map(|receiver| receiver.substitute_erased(&bindings));
        Ok(linked)
    }
}

/// The declared signature a call's operands are read at.
struct CalleeShape<'a> {
    receiver: Option<Ty>,
    params: &'a [Ty],
    ret: Ty,
    context_count: usize,
    class_type_parameters: &'a [String],
}

/// A call's lowered dispatch receiver and arguments, with the receiver type, parameters and
/// result they were read at.
struct CallOperands {
    receiver: Option<ExprId>,
    args: Vec<ExprId>,
    receiver_type: Option<Ty>,
    params: Vec<Ty>,
    ret: Ty,
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
