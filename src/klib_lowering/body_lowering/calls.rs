//! Calls of compiler built-in operators and of dependency functions.
//!
//! A built-in operator lowers as checked FIR lowers the source operator the KLIB serializes as a
//! call of it; a dependency function lowers as a call of its function in the same unit.

use super::super::array_content_members;
use super::super::ir_builtins::{
    BuiltinCallDisposition, BuiltinOperator, BuiltinRelation, DataClassArrayMember,
};
use super::super::klib_types::{
    classifier_name, function_invocation_arity, member_owner, suspend_function_invocation_arity,
};
use super::{
    mismatch, unsupported, BodyLowering, LinkedAccessor, LinkedAccessorCall, LinkedCallee, Lowered,
};
use crate::fir::FirTypeParameterRef;
use crate::ir::{
    Callee, ExprId, IrBinOp, IrCheckedOperation, IrCheckedSubstitution, IrConst, IrExpr,
    IrIntrinsic, IrPropertyDispatch,
};
use crate::metadata::id_signature::{KlibAccessorIdSignature, KlibPublicIdSignature};
use crate::metadata::klib_ir::tree::{
    KlibIrArguments, KlibIrExprId, KlibIrExprKind, KlibIrMemberAccess,
};
use crate::metadata::klib_ir::{KlibIrSignature, KlibIrSymbol, KlibIrSymbolKind};
use crate::types::{EqualityMode, Ty, TyKind};

use super::super::decline::KlibBodyDeclineReason;

/// How a call of a member reaches its implementation.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CallDispatch {
    /// Through the receiver's dispatch table when a subclass may override the member.
    Virtual,
    /// To the declared implementation itself: a `super` call through the qualifier.
    Direct(crate::types::TypeName),
}

impl BodyLowering<'_> {
    /// A call of a compiler built-in operator or of a dependency function. A built-in operator
    /// lowers as checked FIR lowers the source operator it serializes; a dependency function
    /// lowers as a call of its function in the same unit.
    pub(super) fn call(&mut self, access: &KlibIrMemberAccess, ty: Ty) -> Lowered<ExprId> {
        if access.symbol.kind != KlibIrSymbolKind::Function {
            return Err(mismatch("a call target's symbol is not a function"));
        }
        let inherited = match self.linker.inherited_member(&access.symbol.signature) {
            Some(member) => Some(member),
            None => self.linker.inherited_getter(&access.symbol.signature)?,
        };
        let signature = inherited.as_ref().unwrap_or(&access.symbol.signature);
        let callee = match signature {
            KlibIrSignature::Public(callee) if !self.linker.links_privately(signature) => callee,
            KlibIrSignature::Accessor(accessor) if !self.linker.links_privately(signature) => {
                return self.accessor_call(accessor, access, ty)
            }
            KlibIrSignature::Public(_)
            | KlibIrSignature::Accessor(_)
            | KlibIrSignature::FileLocal { .. }
            | KlibIrSignature::TypeParameter { .. } => {
                let linked = match self.object_members.get(signature) {
                    Some(member) => member.linked(),
                    None => self.linker.link_local_function(self.ir, signature)?,
                };
                return self.linked_call(linked, access, ty);
            }
        };
        if let Some(operation) = self.linker.atomic_field(callee)? {
            return self.atomic_field(operation, access, ty);
        }
        if let Some(operation) = self.linker.atomic_array_element(callee)? {
            return self.atomic_array_element(operation, access, ty);
        }
        if let Some(arity) = suspend_function_invocation_arity(callee) {
            if !access.type_arguments.is_empty() {
                return Err(mismatch(
                    "a suspend function invocation is called with type arguments",
                ));
            }
            return self.function_invocation(access, arity, ty, true);
        }
        if let Some(arity) = function_invocation_arity(callee) {
            if !access.type_arguments.is_empty() {
                return Err(mismatch(
                    "a function invocation is called with type arguments",
                ));
            }
            return self.function_invocation(access, arity, ty, false);
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
                if let Some((classifier, member)) =
                    self.linker.implicit_enum_member(self.ir, &signature)?
                {
                    return self.implicit_enum_member(classifier, member, access, ty);
                }
                if let Some(suspension) = self.linker.unintercepted_suspension(&signature)? {
                    return self.unintercepted_suspension_call(suspension, access, ty);
                }
                if let Some(intrinsic) = self.linker.continuation_intrinsic(&signature)? {
                    return self.continuation_intrinsic_call(intrinsic, access, ty);
                }
                if let Some(expansion) = self.linker.intercepted_continuation(&signature)? {
                    return self.intercepted_continuation_call(expansion, access, ty);
                }
                if let Some(primitive) = self.linker.common_primitive(&signature)? {
                    return self.primitive_call(primitive, access, ty);
                }
                return self.dependency_call(callee, access, ty);
            }
        };
        if operator == BuiltinOperator::CheckNotNull {
            return self.not_null_assertion(access, ty);
        }
        if operator == BuiltinOperator::NoWhenBranchMatched {
            return unsupported(
                "`noWhenBranchMatchedException` outside an exhaustive `when`'s `else`",
            );
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
            BuiltinOperator::DataClassArrayMember(member) => {
                self.data_class_array_member(access, member, ty)
            }
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
        if primitive.operation == CommonPrimitive::EmptyArray {
            if access.type_arguments.len() != 1 || !ty.is_reference_array() {
                return Err(mismatch(
                    "an empty array factory is called with another shape than its declaration's",
                ));
            }
            let size = self.ir.add_expr(IrExpr::Const(IrConst::Int(0)));
            let array = self.ir.add_expr(IrExpr::NewArray {
                array_type: ty,
                size,
            });
            return Ok(self.typed(array, ty));
        }
        if !access.type_arguments.is_empty() {
            return Err(mismatch("a common primitive is called with type arguments"));
        }
        let operands = self.call_operands(
            access,
            ty,
            CalleeShape {
                receiver: primitive.receiver,
                params: &primitive.params,
                ret: primitive.ret,
                context_count: 0,
                class_type_parameters: &primitive.class_type_parameters,
                parameter_defaults: &[],
                declaration: None,
            },
        )?;
        // An array factory is the array its one vararg argument packs, as checked FIR lowers a
        // source call of it to an array literal. Every other primitive is a member operation.
        let (Some(receiver), Some(receiver_type)) = (operands.receiver, primitive.receiver) else {
            return match (primitive.operation, operands.args.as_slice()) {
                (CommonPrimitive::ArrayLiteral, &[array]) => Ok(self.typed(array, operands.ret)),
                _ => Err(mismatch(
                    "a member primitive is called without its dispatch receiver",
                )),
            };
        };
        Ok(match primitive.operation {
            CommonPrimitive::Unary(operation) => {
                // Match checked FIR's selected promotion before unary realization. In particular,
                // negate is an IEEE unary operation, never subtraction from positive zero.
                let operand = if receiver_type == primitive.ret {
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
            // The ordinary read of the property, as checked FIR lowers a source read of it.
            CommonPrimitive::PropertyRead(_) => {
                let target = primitive
                    .property
                    .expect("an authenticated property read names its property");
                self.ir.add_expr(IrExpr::Checked(Box::new(
                    IrCheckedOperation::ExternalPropertyRead {
                        target,
                        dispatch: IrPropertyDispatch::Ordinary,
                        receiver: Some(receiver),
                        arguments: Vec::new(),
                        parameters: Vec::new(),
                        result: operands.ret,
                        substitutions: Vec::new(),
                        source_receiver: operands.receiver_type,
                    },
                )))
            }
            // A numeric conversion, as checked FIR lowers the source conversion.
            CommonPrimitive::ArrayLiteral | CommonPrimitive::EmptyArray => {
                return Err(mismatch(
                    "an array factory is called with a dispatch receiver",
                ))
            }
            CommonPrimitive::Conversion => self.ir.add_expr(IrExpr::TypeOp {
                op: crate::ir::IrTypeOp::ImplicitCoercion,
                arg: receiver,
                type_operand: primitive.ret,
            }),
            // The character at the index, as checked FIR lowers a source `String.get` call.
            CommonPrimitive::StringGet => {
                let &[index] = operands.args.as_slice() else {
                    return Err(mismatch(
                        "a string read is called with another shape than its declaration's",
                    ));
                };
                let read = self.ir.add_expr(IrExpr::Call {
                    callee: Box::new(Callee::Intrinsic {
                        operation: IrIntrinsic::StringGet,
                        ret: Ty::Char,
                    }),
                    dispatch_receiver: Some(receiver),
                    args: vec![index],
                });
                self.typed(read, Ty::Char)
            }
            // `compareTo` of a builtin scalar, as checked FIR lowers a call of it: the comparison
            // at the provider's common carrier, the operands promoted to a numeric carrier first.
            CommonPrimitive::Compare { operand } => {
                let (&[other], &[other_type]) =
                    (operands.args.as_slice(), operands.params.as_slice())
                else {
                    return Err(mismatch(
                        "a comparison is called with another shape than its declaration's",
                    ));
                };
                let promote = |lowering: &mut Self, value, ty: Ty| {
                    if ty == operand || matches!(ty, Ty::Char | Ty::Boolean) {
                        value
                    } else {
                        lowering.coercion(value, operand)
                    }
                };
                let receiver = promote(self, receiver, receiver_type);
                let other = promote(self, other, other_type);
                let relational_operator = matches!(
                    access.origin.as_deref(),
                    Some("LT" | "GT" | "LTEQ" | "GTEQ")
                );
                let compare = self.ir.add_expr(IrExpr::Call {
                    callee: Box::new(Callee::Intrinsic {
                        operation: IrIntrinsic::PrimitiveCompare {
                            operand,
                            relational_operator,
                        },
                        ret: Ty::Int,
                    }),
                    dispatch_receiver: Some(receiver),
                    args: vec![other],
                });
                self.typed(compare, Ty::Int)
            }
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
                        start_type: receiver_type,
                        end,
                        end_type,
                        result: primitive.ret,
                        // A support body builds a range this way only on a target laying out
                        // the range class itself: no declaration is called to build it.
                        declaration: None,
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
        // A type parameter whose bound admits null asserts to the intersection `T & Any`, which
        // keeps the parameter's identity; any other operand to its non-null type.
        let asserted = self
            .lowered_type(operand)
            .non_null()
            .marked_definitely_not_null();
        if asserted != ty || ty.is_nullable() {
            return Err(mismatch(
                "`CHECK_NOT_NULL` is typed apart from its operand's non-null type",
            ));
        }
        Ok(self.ir.add_expr(IrExpr::NotNullAssert {
            operand,
            check: crate::ir::NullCheck::Source,
        }))
    }

    /// A data class array member, as a call of the array's content extension on the operand.
    /// The prewalk adds the same extension as the call's edge (`array_content_members`).
    fn data_class_array_member(
        &mut self,
        access: &KlibIrMemberAccess,
        member: DataClassArrayMember,
        ty: Ty,
    ) -> Lowered<ExprId> {
        let Some(array) = array_content_members::operand(access) else {
            return Err(mismatch(
                "a data class array member is called with another shape than its declaration's",
            ));
        };
        let Some(callee) = array_content_members::array_element(self.arena, array)
            .and_then(|element| array_content_members::content_callee(member, element))
        else {
            return unsupported("a data class array member over a non-primitive array");
        };
        let KlibIrSignature::Public(signature) = &callee.signature else {
            unreachable!("an array content extension is a public declaration");
        };
        let access = KlibIrMemberAccess {
            symbol: callee.clone(),
            arguments: KlibIrArguments::Split {
                dispatch_receiver: None,
                extension_receiver: Some(array),
                values: Vec::new(),
            },
            type_arguments: Vec::new(),
            origin: access.origin.clone(),
        };
        self.dependency_call(signature, &access, ty)
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
                | BuiltinOperator::CheckNotNull
                | BuiltinOperator::NoWhenBranchMatched
                | BuiltinOperator::DataClassArrayMember(_),
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
                if let Some(membership) = membership {
                    return Ok(membership);
                }
                // Any other `!in` negates the `contains` call the membership is.
                let negates_membership = matches!(
                    &expression.kind,
                    KlibIrExprKind::Call { access, .. }
                        if access.origin.as_deref() == Some("NOT_IN")
                );
                if !negates_membership {
                    return Err(mismatch("a `!in` negates no membership call"));
                }
                let operand = self.operand(operand, Ty::Boolean)?;
                Ok(self.ir.add_negation(operand))
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
        suspend: bool,
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
        if signature.params.len() != arity || signature.suspend != suspend {
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
        let mut args = Vec::with_capacity(arguments.len());
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
        if suspend {
            self.ir.suspend_calls.insert(invocation, signature.ret);
        }
        Ok(if signature.ret == ty {
            invocation
        } else {
            self.ir.set_logical_type(invocation, signature.ret);
            let erased = self.coercion(invocation, ty);
            self.erasure_coercions.insert(erased);
            erased
        })
    }

    /// `super<Q>.f(…)`: the qualifier's own implementation of `f`, called on the receiver without
    /// dispatch, as checked FIR lowers a source `super` call. The KLIB names the member through
    /// the qualifier, so the callee must be a member `Q` declares, and only an implementation this
    /// unit lowers is reached directly: no override of a subclass may replace it.
    pub(super) fn super_call(
        &mut self,
        qualifier: &KlibIrSymbol,
        access: &KlibIrMemberAccess,
        ty: Ty,
    ) -> Lowered<ExprId> {
        let (KlibIrSymbolKind::Class, KlibIrSignature::Public(qualifier)) =
            (qualifier.kind, &qualifier.signature)
        else {
            return Err(mismatch("a `super` qualifier is not a public class"));
        };
        let (KlibIrSymbolKind::Function, KlibIrSignature::Public(callee)) =
            (access.symbol.kind, &access.symbol.signature)
        else {
            return unsupported("a `super` call of a member without a public signature");
        };
        if member_owner(callee)? != classifier_name(qualifier)? {
            return Err(mismatch(
                "a `super` call names a member its qualifier does not declare",
            ));
        }
        let linked = self.linker.link(self.ir, callee)?;
        if !matches!(linked.callee, Callee::Local(_)) {
            return unsupported("a `super` call of a member this unit does not implement");
        }
        if linked.receiver.is_none() {
            return Err(mismatch("a `super` call names a member without a receiver"));
        }
        if !self.linker.declares_implementation(callee)? {
            return Err(mismatch("a `super` call names an abstract member"));
        }
        let qualifier = classifier_name(qualifier)?;
        self.linked_call_with(linked, access, ty, CallDispatch::Direct(qualifier))
    }

    /// The call without dispatch of `function`, the method this unit emits for the `qualifier`'s
    /// member, on `receiver`: the dependency member it implements is the `super` call's
    /// declaration.
    fn super_callee(
        &self,
        function: crate::ir::FunId,
        qualifier: crate::types::TypeName,
        receiver: ExprId,
    ) -> Lowered<Callee> {
        let mut implemented = self
            .ir
            .dependency_methods
            .iter()
            .filter(|(_, &method)| method == function)
            .map(|(&declaration, _)| declaration);
        let (Some(declaration), None) = (implemented.next(), implemented.next()) else {
            return Err(mismatch(
                "a `super` call's member is not exactly one method of its class",
            ));
        };
        let dispatch_owner = self
            .lowered_type(receiver)
            .kotlin_class_internal()
            .ok_or_else(|| mismatch("a `super` call's receiver is not a class instance"))?;
        let declared = &self.ir.functions[function as usize];
        Ok(Callee::Super {
            qualifier,
            owner: qualifier,
            dispatch_owner,
            enclosing_dispatch: false,
            kind: crate::ir::IrSuperCallKind::Function,
            name: declared.name.clone(),
            params: declared.params.clone(),
            ret: declared.ret,
            interface: false,
            realization: crate::libraries::MemberRealization::Dispatch,
            descriptor: String::new(),
            declaration: Some(crate::fir::ResolvedFunctionOverrideTarget::External(
                declaration,
            )),
            defaults: Vec::new(),
            source_member: None,
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

    pub(super) fn linked_call(
        &mut self,
        linked: LinkedCallee,
        access: &KlibIrMemberAccess,
        ty: Ty,
    ) -> Lowered<ExprId> {
        self.linked_call_with(linked, access, ty, CallDispatch::Virtual)
    }

    /// [`Self::linked_call`], with the dispatch the call site names: a `super` call reaches the
    /// qualifier's implementation itself, never a subclass's override.
    fn linked_call_with(
        &mut self,
        linked: LinkedCallee,
        access: &KlibIrMemberAccess,
        ty: Ty,
        dispatch: CallDispatch,
    ) -> Lowered<ExprId> {
        let declared_parameters = linked.params.clone();
        let declared_formals = linked.type_parameters.clone();
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
                parameter_defaults: &linked.parameter_defaults,
                declaration: Some((&declared_parameters, &declared_formals)),
            },
        )?;
        self.linked_call_with_operands(linked, operands, dispatch, Some(access))
    }

    /// A call of `linked` with the operands its call site supplies, dispatched as `dispatch`.
    /// `access` is the serialized call whose type arguments an expanded inline callee binds; a
    /// call the lowering synthesizes has none.
    fn linked_call_with_operands(
        &mut self,
        linked: LinkedCallee,
        operands: CallOperands,
        dispatch: CallDispatch,
        access: Option<&KlibIrMemberAccess>,
    ) -> Lowered<ExprId> {
        let mut callee = linked.callee;
        if let Callee::External { params, ret, .. } = &mut callee {
            (*params, *ret) = (operands.params, operands.ret);
        }
        let suspend_result = match &callee {
            Callee::External { target, .. } if self.linker.is_suspend_callable(*target) => {
                Some(operands.ret)
            }
            _ => None,
        };
        let declared_ret = match callee {
            Callee::Local(function) => Some(self.ir.functions[function as usize].ret),
            _ => None,
        };
        // Only a function of this unit is linked with defaults, so only it can be left to them:
        // the call carries its supplied arguments and the omitted parameters, as a source call
        // of a function of its own file does.
        if !operands.omitted.is_empty() {
            let Callee::Local(function) = callee else {
                unreachable!("only a unit function declares defaults a call may leave to");
            };
            callee = Callee::LocalWithDefaults {
                function,
                defaults: operands.omitted.into_boxed_slice(),
            };
        }
        let (dispatch_receiver, args) = (operands.receiver, operands.args);
        // A backend calls a method with a dispatch slot through it, so a `super` call of one
        // names the dependency member the method implements, which is called without dispatch.
        if let (CallDispatch::Direct(qualifier), Callee::Local(function), Some(receiver)) =
            (dispatch, &callee, dispatch_receiver)
        {
            let function = *function;
            if self.ir.overridable_method(function).is_some() {
                callee = self.super_callee(function, qualifier, receiver)?;
            }
        }
        let singleton_dispatch = linked.singleton_dispatch;
        let expanded = match &callee {
            Callee::Local(function) if self.ir.inline_only_fns.contains(function) => {
                Some(*function)
            }
            _ => None,
        };
        let external = match &callee {
            Callee::External { target, .. } => Some(*target),
            _ => None,
        };
        // A member dispatch reaches on an instance of a class this unit materializes, which a
        // subclass may override, is called through the class's dispatch table.
        let overridable = match (&callee, dispatch_receiver) {
            _ if matches!(dispatch, CallDispatch::Direct(_)) => None,
            (Callee::Local(function), Some(receiver)) => self
                .ir
                .overridable_method(*function)
                .map(|slot| (slot, receiver)),
            (Callee::LocalWithDefaults { function, .. }, Some(receiver)) => self
                .ir
                .overridable_method(*function)
                .map(|slot| (slot, receiver)),
            _ => None,
        };
        let call = match overridable {
            // A dispatched call leaving arguments to their defaults leaves their places empty: the
            // shared default-argument lowering fills them in the declaration's entry, which then
            // dispatches through the slot, as kotlinc's `$default` entry does.
            Some(((class, index), receiver)) => {
                let args = match &callee {
                    Callee::LocalWithDefaults { defaults, .. } => {
                        let mut supplied = args.into_iter();
                        let count = supplied.len() + defaults.len();
                        (0..count)
                            .map(|ordinal| {
                                if u32::try_from(ordinal)
                                    .is_ok_and(|ordinal| defaults.contains(&ordinal))
                                {
                                    None
                                } else {
                                    supplied.next()
                                }
                            })
                            .collect()
                    }
                    _ => args.into_iter().map(Some).collect(),
                };
                self.ir.add_expr(IrExpr::MethodCall {
                    class,
                    index,
                    receiver,
                    args,
                })
            }
            None => self.ir.add_expr(IrExpr::Call {
                callee: Box::new(callee),
                dispatch_receiver,
                args,
            }),
        };
        if let (Some(function), Some(access)) = (expanded, access) {
            self.record_inline_type_arguments(call, function, access)?;
        }
        if let Some(result) = suspend_result {
            self.ir.suspend_calls.insert(call, result);
        }
        if let Some(role) = linked.edge.semantic_role {
            self.ir.semantic_call_roles.insert(call, role);
        }
        if let Some(target) = external {
            self.ir
                .record_external_overridden_declarations(target, &linked.edge.overridden);
        }
        if let Some(classifier) = singleton_dispatch {
            self.ir
                .external_singleton_dispatches
                .insert(call, classifier);
        }
        Ok(self.local_result(call, declared_ret, operands.ret))
    }

    /// The value of a `call` of a function of this unit, whose declared result is `declared`, at
    /// the call's result `ret`. The result of a generic local function is its declared type
    /// parameter; the call's value is that result at the call's type arguments. The call itself
    /// keeps the declaration's result, as a source call of a local function does, and the
    /// coercion carries the substituted one. A backend reads that published result rather than
    /// reconstructing it from the callee.
    fn local_result(&mut self, call: ExprId, declared: Option<Ty>, ret: Ty) -> ExprId {
        if let Some(declared) =
            declared.filter(|_| matches!(self.ir.expr(call), IrExpr::Call { .. }))
        {
            self.ir.call_declared_ret.insert(call, declared);
        }
        match declared {
            Some(declared) if declared != ret => {
                // As checked FIR lowering does for a source call, the inner call publishes the
                // declaration's result it evaluates to, and the coercion the use's type.
                self.ir.call_declared_ret.insert(call, declared);
                let coerced = self.coercion(call, ret);
                self.ir.set_logical_type(coerced, ret);
                coerced
            }
            _ => call,
        }
    }
}

impl BodyLowering<'_> {
    fn unintercepted_suspension_call(
        &mut self,
        selected: super::UninterceptedSuspensionCall,
        access: &KlibIrMemberAccess,
        ty: Ty,
    ) -> Lowered<ExprId> {
        let [Some(type_argument)] = access.type_arguments.as_slice() else {
            return Err(mismatch(
                "an unintercepted suspension call has another type-argument shape",
            ));
        };
        let type_argument = self.semantic(*type_argument)?;
        let mut bindings = crate::fx_hash::HashMap::default();
        bindings.insert(selected.formal, type_argument);
        let parameter =
            crate::types::ty_subst_formals(selected.parameter, &[selected.formal], &bindings);
        let result = crate::types::ty_subst_formals(selected.result, &[selected.formal], &bindings);
        let operands = self.call_operands(
            access,
            ty,
            CalleeShape {
                receiver: None,
                params: &[parameter],
                ret: result,
                context_count: 0,
                class_type_parameters: &[],
                parameter_defaults: &[],
                declaration: None,
            },
        )?;
        let logical_result = operands.ret;
        let [block] = operands.args.as_slice() else {
            return Err(mismatch(
                "an unintercepted suspension call has another operand shape",
            ));
        };
        let IrExpr::Lambda {
            impl_fn,
            inline_body,
            ..
        } = &mut self.ir.exprs[*block as usize]
        else {
            return Err(KlibBodyDeclineReason::UnsupportedOperation(
                "an unintercepted suspension block that is not a KLIB closure",
            ));
        };
        let body = self
            .ir
            .functions
            .get(*impl_fn as usize)
            .and_then(|function| function.body);
        let Some(body) = body else {
            return Err(KlibBodyDeclineReason::NoBody);
        };
        *inline_body = Some(body);
        let invocation = self.ir.invoke_with_current_continuation(*block).ok_or(
            KlibBodyDeclineReason::UnsupportedOperation(
                "an unintercepted suspension block with another invocation shape",
            ),
        )?;
        self.ir
            .mark_unintercepted_suspension(invocation, logical_result);
        Ok(invocation)
    }

    /// A call of Native's `getContinuation<T>()` or `returnIfSuspended<T>(argument)`, which
    /// kotlinc's inliner leaves where `suspendCoroutineUninterceptedOrReturn` was expanded. The
    /// first is the body's own continuation; the second makes its operand the suspension point
    /// the primitive is, as a source call of the primitive marks its block's invocation.
    fn continuation_intrinsic_call(
        &mut self,
        selected: super::super::continuation_intrinsics::ContinuationIntrinsicCall,
        access: &KlibIrMemberAccess,
        ty: Ty,
    ) -> Lowered<ExprId> {
        use super::super::continuation_intrinsics::ContinuationIntrinsic;
        let [Some(type_argument)] = access.type_arguments.as_slice() else {
            return Err(mismatch(
                "a continuation intrinsic call has another type-argument shape",
            ));
        };
        let type_argument = self.semantic(*type_argument)?;
        let mut bindings = crate::fx_hash::HashMap::default();
        bindings.insert(selected.formal, type_argument);
        let substitute = |ty: Ty| crate::types::ty_subst_formals(ty, &[selected.formal], &bindings);
        let params = selected
            .params
            .iter()
            .map(|&ty| substitute(ty))
            .collect::<Vec<_>>();
        let result = substitute(selected.result);
        let operands = self.call_operands(
            access,
            ty,
            CalleeShape {
                receiver: None,
                params: &params,
                ret: result,
                context_count: 0,
                class_type_parameters: &[],
                parameter_defaults: &[],
                declaration: None,
            },
        )?;
        match (selected.operation, operands.args.as_slice()) {
            (ContinuationIntrinsic::Current, []) => {
                let continuation = self.ir.add_expr(IrExpr::CurrentContinuation);
                Ok(self.typed(continuation, operands.ret))
            }
            (ContinuationIntrinsic::ReturnIfSuspended, &[outcome]) => {
                self.ir.mark_unintercepted_suspension(outcome, operands.ret);
                Ok(outcome)
            }
            _ => Err(mismatch(
                "a continuation intrinsic call has another operand shape",
            )),
        }
    }

    /// `interceptedIntrinsic<T>(cont)`, expanded as kotlinc's `BuiltInsLowering` expands it:
    /// `cont` evaluated once, then `if (cont is CoroutineImpl) (cont as CoroutineImpl).intercepted()
    /// else cont`, the member call linked as any call of it a body makes.
    fn intercepted_continuation_call(
        &mut self,
        expansion: super::super::intercepted_continuation::InterceptedContinuationExpansion,
        access: &KlibIrMemberAccess,
        ty: Ty,
    ) -> Lowered<ExprId> {
        let selected = expansion.call;
        let [Some(type_argument)] = access.type_arguments.as_slice() else {
            return Err(mismatch(
                "a continuation interception has another type-argument shape",
            ));
        };
        let type_argument = self.semantic(*type_argument)?;
        let mut bindings = crate::fx_hash::HashMap::default();
        bindings.insert(selected.formal, type_argument);
        let substitute = |ty: Ty| crate::types::ty_subst_formals(ty, &[selected.formal], &bindings);
        let params = selected
            .params
            .iter()
            .map(|&ty| substitute(ty))
            .collect::<Vec<_>>();
        let operands = self.call_operands(
            access,
            ty,
            CalleeShape {
                receiver: None,
                params: &params,
                ret: substitute(selected.result),
                context_count: 0,
                class_type_parameters: &[],
                parameter_defaults: &[],
                declaration: None,
            },
        )?;
        let (&[continuation], &[continuation_type]) =
            (operands.args.as_slice(), operands.params.as_slice())
        else {
            return Err(mismatch(
                "a continuation interception has another operand shape",
            ));
        };
        let linked = self.linker.link(self.ir, &expansion.member)?;
        if linked.receiver.is_none()
            || linked.context_count != 0
            || !linked.params.is_empty()
            || !linked.type_parameters.is_empty()
            || !linked.class_type_parameters.is_empty()
        {
            return Err(mismatch("the intercepting member has another call shape"));
        }
        let coroutine = Ty::obj_name(super::super::intercepted_continuation::coroutine_class());
        let slot = self.locals.anonymous_slot();
        let declaration = self.ir.add_expr(IrExpr::Variable {
            index: slot,
            ty: continuation_type,
            init: Some(continuation),
            named: false,
        });
        let read = self.ir.add_expr(IrExpr::GetValue(slot));
        let matches = self.ir.instance_check(false, read, coroutine);
        let read = self.ir.add_expr(IrExpr::GetValue(slot));
        let receiver = self.ir.add_expr(IrExpr::TypeOp {
            op: crate::ir::IrTypeOp::CastNonNull,
            arg: read,
            type_operand: coroutine,
        });
        let receiver = self.typed(receiver, coroutine);
        let intercepted_type = linked.ret;
        let intercepted = self.linked_call_with_operands(
            linked,
            CallOperands {
                receiver: Some(receiver),
                args: Vec::new(),
                omitted: Vec::new(),
                receiver_type: Some(coroutine),
                params: Vec::new(),
                ret: intercepted_type,
            },
            CallDispatch::Virtual,
            None,
        )?;
        let intercepted = self.coercion(intercepted, operands.ret);
        let unchanged = self.ir.add_expr(IrExpr::GetValue(slot));
        let unchanged = self.typed(unchanged, operands.ret);
        let result = self.ir.add_expr(IrExpr::When {
            branches: vec![(Some(matches), intercepted), (None, unchanged)],
        });
        let result = self.typed(result, operands.ret);
        let block = self.ir.add_expr(IrExpr::Block {
            stmts: vec![declaration],
            value: Some(result),
        });
        Ok(self.typed(block, operands.ret))
    }

    /// A call of a dependency property's accessor, lowered as checked FIR lowers a source access
    /// of the property: a read through its getter, a write through its setter.
    pub(super) fn accessor_call(
        &mut self,
        accessor: &KlibAccessorIdSignature,
        access: &KlibIrMemberAccess,
        ty: Ty,
    ) -> Lowered<ExprId> {
        let signature = crate::libraries::KlibDeclarationSignature::Accessor(accessor.clone());
        if let Some((classifier, member)) = self.linker.implicit_enum_member(self.ir, &signature)? {
            return self.implicit_enum_member(classifier, member, access, ty);
        }
        if access.type_arguments.is_empty() {
            if let Some(primitive) = self.linker.common_primitive(&signature)? {
                return self.primitive_call(primitive, access, ty);
            }
        }
        let mut linked = match self.linker.link_accessor(self.ir, accessor)? {
            LinkedAccessorCall::Property(linked) => linked,
            // An accessor the unit emits is called as any function of the unit is, binding its
            // own type parameters at the access's type arguments.
            LinkedAccessorCall::Function(linked) => return self.linked_call(linked, access, ty),
        };
        if linked.type_parameters.len() != access.type_arguments.len() {
            return Err(mismatch(
                "a property access has another number of type arguments than its selected accessor",
            ));
        }
        if !linked.reified_type_parameters.is_empty() {
            return unsupported("a reified generic property accessor");
        }
        if linked.type_parameter_bounds.len() != linked.type_parameters.len() {
            return Err(mismatch(
                "the selected accessor's bounds do not match its formal identities",
            ));
        }
        let target = if linked.type_parameters.is_empty() {
            None
        } else {
            Some(linked.callable.ok_or_else(|| {
                KlibBodyDeclineReason::SignatureMismatch(
                    "a generic accessor has no exact external callable identity".to_owned(),
                )
            })?)
        };
        // An accessor captures no projection: its arguments are bound as serialized.
        let TypeArgumentBindings {
            bindings,
            substitutions,
        } = self.bind_type_arguments(
            &linked.type_parameters,
            access,
            &crate::fx_hash::HashMap::default(),
            target,
        )?;
        if !linked.type_parameters.is_empty() {
            // The accessor's own formals are bound here; a declaring-class formal stays as written
            // until the dispatch receiver applies it.
            let formals = &linked.type_parameters;
            let own = |ty| crate::types::ty_subst_formals(ty, formals, &bindings);
            for parameter in &mut linked.params {
                *parameter = own(*parameter);
            }
            linked.ret = own(linked.ret);
            linked.receiver = linked.receiver.map(own);
        }
        let CallOperands {
            receiver,
            args: arguments,
            receiver_type: source_receiver,
            params: parameters,
            ret: result,
            omitted: _,
        } = self.call_operands(
            access,
            ty,
            CalleeShape {
                receiver: linked.receiver,
                params: &linked.params,
                ret: linked.ret,
                context_count: linked.context_count,
                class_type_parameters: &linked.class_type_parameters,
                // An accessor declares no default a call may leave to.
                parameter_defaults: &[],
                declaration: None,
            },
        )?;
        // Accessor bounds may mention both accessor formals and declaring-class formals. The
        // former are known from the serialized accessor type arguments; the latter are only
        // known after the dispatch receiver has been applied to its declaring class.
        let mut bound_bindings = bindings;
        if !linked.class_type_parameters.is_empty() {
            let receiver_type = source_receiver.ok_or_else(|| {
                mismatch("a generic member accessor has no applied dispatch receiver")
            })?;
            if receiver_type.type_args().len() != linked.class_type_parameters.len() {
                return Err(mismatch(
                    "an accessor receiver applies another number of declaring-class type arguments",
                ));
            }
            bound_bindings.extend(
                linked
                    .class_type_parameters
                    .iter()
                    .map(crate::types::TyParamName::from)
                    .zip(receiver_type.type_args().iter().copied()),
            );
        }
        for (formal, bounds) in linked
            .type_parameters
            .iter()
            .zip(&linked.type_parameter_bounds)
        {
            let value = bound_bindings[formal];
            for bound in bounds {
                let bound = crate::types::ty_subst_formals(
                    *bound,
                    &linked.type_parameters,
                    &bound_bindings,
                );
                if !self
                    .linker
                    .is_subtype(self.header.type_parameters(), value, bound)
                {
                    return Err(KlibBodyDeclineReason::ImplicitConversion);
                }
            }
        }
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
                substitutions,
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
                substitutions,
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
        // The shape is already at the call's type arguments, which may name the caller's own type
        // parameters; only the class's type parameters are the receiver's to bind.
        let substitute = |declared: Ty| {
            crate::types::ty_subst_formals(declared, callee.class_type_parameters, &bindings)
        };
        // A result read through a projected receiver argument (`Array<out T>.get`) is that
        // projection's read type, as a source call's result is; a nullable result reads its
        // projection the same way (`MutableMap<in K, in V>.put` answers `Any?`).
        let ret = match substitute(callee.ret).kind() {
            crate::types::TyKind::Nullable(inner) => Ty::nullable(inner.projection_read_ty()),
            _ => substitute(callee.ret).projection_read_ty(),
        };
        let ret = if self.linker.same_type(ret, ty) {
            ty
        } else {
            return Err(mismatch("a call is typed apart from its callee's result"));
        };
        let params = callee
            .params
            .iter()
            .map(|&param| substitute(param))
            .collect::<Vec<_>>();
        let mut args = Vec::with_capacity(params.len());
        let mut omitted = Vec::new();
        for (index, (argument, (&declared, &parameter))) in
            arguments.zip(callee.params.iter().zip(&params)).enumerate()
        {
            match argument {
                Some(argument) => {
                    let accepted = argument_expectation(declared, parameter, &bindings);
                    let site = callee.declaration.and_then(|(parameters, formals)| {
                        Some(super::CaptureSite {
                            declared: *parameters.get(index)?,
                            parameters,
                            formals,
                        })
                    });
                    args.push(self.operand_capturing(argument, accepted, site)?);
                }
                // A serialized call leaves out exactly the arguments the callee's defaults supply.
                None if callee.parameter_defaults.get(index).copied() == Some(true) => {
                    omitted.push(u32::try_from(index).expect("a parameter list fits u32"));
                }
                None => {
                    return unsupported("a call that leaves a parameter to its default");
                }
            }
        }
        Ok(CallOperands {
            receiver,
            args,
            omitted,
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
            .applied_supertype(actual, owner)
            .ok_or(KlibBodyDeclineReason::ImplicitConversion)?;
        if applied.type_args().len() != declared.type_args().len() {
            return Err(mismatch(
                "a generic member's receiver applies another number of type arguments",
            ));
        }
        Ok(applied)
    }

    /// The type arguments `call` of the inline function `function` binds, under the selected
    /// semantic identities of the function's type parameters, which its body's types name, for the
    /// expansion to specialize that body with.
    fn record_inline_type_arguments(
        &mut self,
        call: ExprId,
        function: crate::ir::FunId,
        access: &KlibIrMemberAccess,
    ) -> Lowered<()> {
        let Some(signature) = self.ir.signatures.get(&function) else {
            return Ok(());
        };
        if signature.type_params.len() != access.type_arguments.len() {
            return Err(mismatch(
                "a call passes another number of type arguments than its inline callee declares",
            ));
        }
        let formals = signature
            .type_params
            .iter()
            .map(|formal| (formal.semantic_name.clone(), formal.reified))
            .collect::<Vec<_>>();
        let mut bound = Vec::with_capacity(formals.len());
        let mut reified = Vec::new();
        for ((name, is_reified), argument) in formals.into_iter().zip(&access.type_arguments) {
            let argument = argument.ok_or(KlibBodyDeclineReason::UnsupportedOperation(
                "a call that leaves a type argument unset",
            ))?;
            let value = self.semantic(argument)?;
            if is_reified {
                reified.push((name.clone(), value));
            }
            bound.push((name, value));
        }
        if !reified.is_empty() {
            self.ir.reified_call_subst.insert(call, reified);
        }
        self.ir.inline_call_type_arguments.insert(call, bound);
        Ok(())
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
        let captured = self.captured_type_arguments(&linked, access)?;
        // A captured projection is serialized approximated to its bound; the call binds the
        // projection itself, as checked FIR lowering records a source call that captures it. A
        // serialized argument that is not that bound is another call than the one captured.
        for (parameter, argument) in linked.type_parameters.iter().zip(&access.type_arguments) {
            let (Some(projection), Some(argument)) = (captured.get(parameter), argument) else {
                continue;
            };
            let value = self.semantic(*argument)?;
            if projection.projection_read_ty() != value {
                return Err(mismatch(
                    "a captured projection disagrees with the call's serialized type argument",
                ));
            }
        }
        if let Callee::Local(_) = linked.callee {
            // A call of a function the unit declares binds its type parameters as checked FIR
            // lowers a source call of a module generic function: the arguments are read at the
            // substituted parameters and the declared result is coerced to the substituted one,
            // with no substitutions recorded on the call.
            let bindings = self
                .bind_type_arguments(&linked.type_parameters, access, &captured, None)?
                .bindings;
            // A member of a generic class keeps naming its class's type parameters, which
            // [`Self::call_operands`] binds from the dispatch receiver's type.
            let formals = linked.type_parameters.clone();
            let substitute = |ty: Ty| crate::types::ty_subst_formals(ty, &formals, &bindings);
            for param in &mut linked.params {
                *param = substitute(*param);
            }
            linked.ret = substitute(linked.ret);
            linked.receiver = linked.receiver.map(substitute);
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
        let bound =
            self.bind_type_arguments(&linked.type_parameters, access, &captured, Some(*target))?;
        let bindings = bound.bindings;
        substitutions.extend(bound.substitutions);
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

/// What a call or property access binds its declaration's own type parameters to.
struct TypeArgumentBindings {
    /// Each type parameter's value.
    bindings: crate::fx_hash::HashMap<crate::types::TyParamName, Ty>,
    /// The substitutions recorded on the external declaration the access targets, in ordinal
    /// order; none when it targets a function of the unit.
    substitutions: Vec<IrCheckedSubstitution>,
}

impl BodyLowering<'_> {
    /// The binding of `formals`, a generic declaration's own type parameters, at the type
    /// arguments `access` serializes for them, as checked FIR lowering binds a source call or
    /// property access of that declaration. A projection the access captures stands for the
    /// argument serialized approximated to its bound. When the access targets the external
    /// declaration `target`, each binding is also recorded as that declaration's substitution.
    fn bind_type_arguments(
        &self,
        formals: &[crate::types::TyParamName],
        access: &KlibIrMemberAccess,
        captured: &crate::fx_hash::HashMap<crate::types::TyParamName, Ty>,
        target: Option<crate::fir::ExternalCallableId>,
    ) -> Lowered<TypeArgumentBindings> {
        let mut bindings = crate::fx_hash::HashMap::default();
        let mut substitutions = Vec::new();
        for (ordinal, (parameter, argument)) in
            formals.iter().zip(&access.type_arguments).enumerate()
        {
            let argument = argument.ok_or(KlibBodyDeclineReason::UnsupportedOperation(
                "an access that leaves a type argument unset",
            ))?;
            let value = self.semantic(argument)?;
            let value = captured.get(parameter).copied().unwrap_or(value);
            bindings.insert(*parameter, value);
            if let Some(callable) = target {
                substitutions.push(IrCheckedSubstitution {
                    parameter: FirTypeParameterRef::External {
                        callable,
                        ordinal: u32::try_from(ordinal)
                            .map_err(|_| mismatch("a declaration has too many type parameters"))?,
                    },
                    // A reified type parameter belongs to an inline function, which a body does
                    // not call through a substitution.
                    reified: false,
                    value,
                    reified_runtime: value,
                    additional_bounds: Vec::new(),
                });
            }
        }
        Ok(TypeArgumentBindings {
            bindings,
            substitutions,
        })
    }
}

impl BodyLowering<'_> {
    /// The type parameters of `linked` that a call captures from a projected argument: a value
    /// parameter declared as a class applied to the bare type parameter `P`, passed a value whose
    /// serialized type applies an `out` projection in that slot that the class's own variance does
    /// not restate, binds `P` to that projection (`copyOf()` on an `Array<out T>`).
    ///
    /// Kotlin captures each projected argument apart, so a capture is published only when it is
    /// the call's sole use of `P`: no other slot, operand, receiver or type-parameter bound of the
    /// declaration names `P`, including the complete declared bounds of every other type
    /// parameter, used or not. Any other shape declines; nothing is bound.
    fn captured_type_arguments(
        &self,
        linked: &LinkedCallee,
        access: &KlibIrMemberAccess,
    ) -> Lowered<crate::fx_hash::HashMap<crate::types::TyParamName, Ty>> {
        let values = arguments(access)
            .into_iter()
            .skip(usize::from(linked.receiver.is_some()));
        let mut operands = Vec::with_capacity(linked.params.len());
        for (&declared, argument) in linked.params.iter().zip(values) {
            let actual = match argument.and_then(|argument| self.arena.expr(argument).ty) {
                Some(serialized) => Some(self.semantic(serialized)?),
                None => None,
            };
            operands.push((declared, actual));
        }
        let mut captured = crate::fx_hash::HashMap::default();
        for (operand, &(declared, actual)) in operands.iter().enumerate() {
            let (Some((owner, formals)), Some((actual_owner, actuals))) =
                (declared.obj_parts(), actual.and_then(Ty::obj_parts))
            else {
                continue;
            };
            if owner != actual_owner || formals.len() != actuals.len() {
                continue;
            }
            for (slot, (formal, &projected)) in formals.iter().zip(actuals).enumerate() {
                let (TyKind::TyParam(name, bound), TyKind::OutProjection(read)) =
                    (formal.kind(), projected.kind())
                else {
                    continue;
                };
                let mut restated = actuals.to_vec();
                restated[slot] = read;
                if self.linker.same_type(
                    actual.expect("matched above"),
                    Ty::obj_args_name(owner, &restated),
                ) {
                    continue;
                }
                // The captured type is a subtype of the projection's read type and of every
                // bound the classifier declares for the slot: `Key<*>` of `Key<E : Element>`
                // captures an `Element`.
                let captured_upper_bounds = std::iter::once(read)
                    .chain(
                        self.linker
                            .classifier_parameter_bounds(owner, slot)
                            .into_iter()
                            .filter(|bound| !bound.mentions_ty_param()),
                    )
                    .collect::<Vec<_>>();
                // The capture reads as the most specific of them, which is how the producer
                // serializes the call's type argument.
                let parameters = self.header.type_parameters();
                let Some(&captured_read) = captured_upper_bounds.iter().find(|&&upper| {
                    captured_upper_bounds
                        .iter()
                        .all(|&other| self.linker.is_subtype(parameters, upper, other))
                }) else {
                    return Err(mismatch(
                        "a call captures a projection whose bounds have no most specific one",
                    ));
                };
                let projected = if captured_read == read {
                    projected
                } else {
                    Ty::out_projection(captured_read)
                };
                let sole = captured.insert(name, projected).is_none()
                    && formals
                        .iter()
                        .enumerate()
                        .all(|(other, formal)| other == slot || !names(*formal, name))
                    && operands
                        .iter()
                        .enumerate()
                        .all(|(other, (declared, _))| other == operand || !names(*declared, name))
                    && !linked
                        .receiver
                        .is_some_and(|receiver| names(receiver, name))
                    && !names(bound, name)
                    && self.bounds_admit_capture(linked, name, &captured_upper_bounds);
                if !sole {
                    return Err(mismatch(
                        "a call captures a projection its other operands or bounds also constrain",
                    ));
                }
            }
        }
        Ok(captured)
    }
}

impl BodyLowering<'_> {
    /// Whether the selected declaration's complete authenticated bound ledger, not only the
    /// first bound a type-parameter type carries, admits the captured type for its type parameter
    /// `name`, a subtype of each of `captured`: every bound of `name` names no type parameter and
    /// is a supertype of one of `captured`, and no bound of any other type parameter names
    /// `name`, since that peer would constrain the capture too. A selection with no complete
    /// ledger is no proof.
    fn bounds_admit_capture(
        &self,
        linked: &LinkedCallee,
        name: crate::types::TyParamName,
        captured: &[Ty],
    ) -> bool {
        let Some(ledger) = linked
            .type_parameter_bounds
            .as_ref()
            .filter(|ledger| ledger.len() == linked.type_parameters.len())
        else {
            return false;
        };
        linked
            .type_parameters
            .iter()
            .zip(ledger)
            .all(|(parameter, bounds)| {
                if *parameter != name {
                    return bounds.iter().all(|&bound| !names(bound, name));
                }
                bounds.iter().all(|&bound| {
                    !bound.mentions_ty_param()
                        && captured.iter().any(|&upper| {
                            self.linker
                                .is_subtype(self.header.type_parameters(), upper, bound)
                        })
                })
            })
    }
}

/// Whether `ty` names the type parameter `name` anywhere, including through the declared bound
/// of another type parameter it names.
pub(super) fn names(ty: Ty, name: crate::types::TyParamName) -> bool {
    fn walk(
        ty: Ty,
        name: crate::types::TyParamName,
        seen: &mut Vec<crate::types::TyParamName>,
    ) -> bool {
        match ty.kind() {
            TyKind::TyParam(parameter, bound) => {
                if parameter == name {
                    return true;
                }
                if seen.contains(&parameter) {
                    return false;
                }
                seen.push(parameter);
                walk(bound, name, seen)
            }
            TyKind::Nullable(inner)
            | TyKind::PlatformNullable(inner)
            | TyKind::MutabilityFlexible(inner)
            | TyKind::DefinitelyNotNull(inner)
            | TyKind::InProjection(inner)
            | TyKind::OutProjection(inner)
            | TyKind::StarProjection(inner)
            | TyKind::RawProjection(inner) => walk(inner, name, seen),
            TyKind::Intersection(parts) => parts.iter().any(|part| walk(*part, name, seen)),
            TyKind::Obj(_, args) => args.iter().any(|arg| walk(*arg, name, seen)),
            TyKind::Fun(signature) => {
                walk(signature.ret, name, seen)
                    || signature
                        .params
                        .iter()
                        .any(|param| walk(*param, name, seen))
            }
            _ => false,
        }
    }
    walk(ty, name, &mut Vec::new())
}

/// The declared signature a call's operands are read at.
struct CalleeShape<'a> {
    receiver: Option<Ty>,
    params: &'a [Ty],
    ret: Ty,
    context_count: usize,
    class_type_parameters: &'a [String],
    /// Whether each parameter declares a default a call may leave it to.
    parameter_defaults: &'a [bool],
    /// The callee's parameters and type parameters as declared, before the call binds them.
    declaration: Option<(&'a [Ty], &'a [crate::types::TyParamName])>,
}

/// A call's lowered dispatch receiver and arguments, with the receiver type, parameters and
/// result they were read at.
struct CallOperands {
    receiver: Option<ExprId>,
    /// The supplied arguments, in parameter order.
    args: Vec<ExprId>,
    /// The parameters the call leaves to their defaults, in order.
    omitted: Vec<u32>,
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

/// The type an argument for the parameter `declared` is checked against, where `parameter` is
/// `declared` at the call's receiver, which binds the class's type parameters to `bindings`. A
/// class type parameter the receiver reads through an `in` projection (`E` on a
/// `MutableCollection<in T>`) has a captured type. kotlinc approximates every occurrence of that
/// captured type in the parameter to the type parameter's bound when it places the argument's
/// implicit casts: at the top (`add(element: E)` takes an `Any?`) and inside the parameter type
/// (`addAll(elements: Collection<E>)` takes a `Collection<Any?>`).
fn argument_expectation(
    declared: Ty,
    parameter: Ty,
    bindings: &crate::fx_hash::HashMap<String, Ty>,
) -> Ty {
    let approximations = bindings
        .iter()
        .filter(|(_, binding)| matches!(binding.kind(), crate::types::TyKind::InProjection(_)))
        .filter_map(|(name, _)| {
            let bound = declared.type_parameter_occurrence_bound(name)?;
            Some((name.clone(), bound.substitute_erased(bindings)))
        })
        .collect::<Vec<_>>();
    if approximations.is_empty() {
        return parameter;
    }
    let mut approximated = bindings.clone();
    approximated.extend(approximations);
    declared.substitute_erased(&approximated)
}
