//! The value of a `const val` initializer, and kotlinc's diagnostic when it has none: a port of
//! kotlinc's frontend `FirExpressionEvaluator` and the evaluation half of `FirConstPropertyChecker`.
//!
//! The evaluator walks the initializer over the decisions ordinary checking recorded for it: the
//! callable selected for each call and operator, the declaration-owned payload of each constant
//! read, the selected enum entry, and the declaration behind a property read. An operation is a
//! compile-time one only when its selected standard-library declaration says so
//! ([`compile_time_operation`]); its value is computed by
//! [`crate::libraries::intrinsic_const_evaluation`].
//!
//! Without `IntrinsicConstEvaluation`, kotlinc evaluates only the `kotlin` package operators,
//! conversions, `toString`, `String.get`, `String.length` and `Char.code`, and never an operation
//! on an unsigned receiver. With it, every standard-library declaration carrying
//! `@IntrinsicConstEvaluation` evaluates, and so do `Enum.name` on an entry and `KCallable.name`
//! on a callable reference.

use crate::ast::{BinOp, Expr, ExprId, TemplatePart, UnOp};
use crate::kt_string::{KtString, KtStringBuf};
use crate::libraries::intrinsic_const_evaluation::{
    compile_time_operation, evaluate, ConstValue, IntrinsicConstDeclaration,
    IntrinsicConstOperation, IntrinsicConstOwner,
};
use crate::libraries::{CompilerIntrinsic, LibraryConst, MemberRealization};
use crate::types::{SemanticCallRole, SemanticCallableOwner, Ty};

use super::{CallableReferenceTarget, Checker, ExprLowering, ResolvedCall, SyntheticOperatorCall};

const NON_CONSTANT_INITIALIZER: &str = "const 'val' initializer must be a constant value.";
const NON_CONST_VAL_READ: &str = "only 'const val' can be used in constant expressions.";

/// The value of one evaluated expression.
#[derive(Clone, Debug)]
enum Evaluated {
    Value(ConstValue),
    /// An enum entry: not a constant itself, but the receiver of `Enum.name`.
    EnumEntry(String),
    /// A callable reference, with the name `KCallable.name` reads (`<init>` for a constructor).
    CallableReference(String),
}

/// Why an expression has no compile-time value (kotlinc's `FirEvaluatorResult` failures).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NotEvaluated {
    /// kotlinc's `NotConst` and the other failures reported as a non-constant initializer.
    NotConst,
    /// A read of a non-`const` `val` initialized by a literal.
    NonConstVal,
    /// An unresolved read. Its own diagnostic explains it; the initializer adds none.
    ResolutionError,
}

type Evaluation = Result<Evaluated, NotEvaluated>;

/// What a `const val` initializer evaluates to.
pub(super) enum ConstInitializer {
    /// The initializer's value, at the property's type.
    Value(LibraryConst),
    /// The initializer has a value that does not convert to the property's type; the type
    /// mismatch is reported by ordinary checking.
    Mistyped,
    /// The initializer has no value; the diagnostic kotlinc reports for it, if any.
    NotConstant(Option<&'static str>),
}

impl Checker<'_> {
    /// Evaluate the initializer `expression` of a `const val` of type `declared`.
    pub(super) fn evaluate_const_initializer(
        &self,
        expression: ExprId,
        declared: Ty,
    ) -> ConstInitializer {
        let evaluated = self.evaluate_constant(expression, 0);
        crate::trace_compiler!(
            "resolve",
            "const initializer {expression:?} as {declared:?}: {evaluated:?}"
        );
        match evaluated {
            Ok(Evaluated::Value(value)) => match value.converted_to(declared) {
                Some(value) => ConstInitializer::Value(value.into_library()),
                None => ConstInitializer::Mistyped,
            },
            Ok(Evaluated::EnumEntry(_) | Evaluated::CallableReference(_)) => {
                ConstInitializer::NotConstant(Some(NON_CONSTANT_INITIALIZER))
            }
            Err(NotEvaluated::NotConst) => {
                ConstInitializer::NotConstant(Some(NON_CONSTANT_INITIALIZER))
            }
            Err(NotEvaluated::NonConstVal) => {
                ConstInitializer::NotConstant(Some(NON_CONST_VAL_READ))
            }
            Err(NotEvaluated::ResolutionError) => ConstInitializer::NotConstant(None),
        }
    }

    /// Check the initializer of a top-level or singleton-member `const` property: publish its
    /// value on the initializer, or report kotlinc's diagnostic at it. kotlinc's checker stops
    /// before evaluating at an `excluded` property (a `var`, or one with a getter or delegate) and
    /// at a type no constant may have; an error type is still evaluated.
    pub(super) fn check_const_initializer(
        &mut self,
        initializer: ExprId,
        declared: Ty,
        excluded: bool,
    ) {
        if excluded || !(declared.can_be_used_for_const_val() || declared == Ty::Error) {
            return;
        }
        match self.evaluate_const_initializer(initializer, declared) {
            ConstInitializer::Value(value) => {
                self.resolved_constants.insert(initializer, value);
            }
            ConstInitializer::Mistyped | ConstInitializer::NotConstant(None) => {}
            ConstInitializer::NotConstant(Some(message)) => {
                self.diags
                    .error(self.span(initializer), message.to_string());
            }
        }
    }

    fn intrinsic_const_evaluation(&self) -> bool {
        self.file.language_gates.intrinsic_const_evaluation
    }

    fn evaluate_constant(&self, expression: ExprId, depth: u32) -> Evaluation {
        let evaluated = self.evaluate_constant_expression(expression, depth);
        crate::trace_compiler!(
            "resolve",
            "const operand {expression:?} {:?} type={:?} call={:?} operator={:?} lowering={:?}: {evaluated:?}",
            self.file.expr(expression),
            self.expr_types[expression.0 as usize],
            self.resolved_calls.get(&expression).map(trace_call),
            self.resolved_operator_calls
                .iter()
                .filter(|((operator, _), _)| *operator == expression)
                .map(|(_, call)| trace_call(call))
                .collect::<Vec<_>>(),
            self.expr_lowers.get(&expression).map(trace_lowering),
        );
        evaluated
    }

    fn evaluate_constant_expression(&self, expression: ExprId, depth: u32) -> Evaluation {
        if depth > 256 {
            return Err(NotEvaluated::NotConst);
        }
        let depth = depth + 1;
        let ty = self.expr_types[expression.0 as usize];
        match self.file.expr(expression) {
            Expr::IntLit(_)
            | Expr::LongLit(_)
            | Expr::UIntLit(_)
            | Expr::ULongLit(_)
            | Expr::FloatLit(_)
            | Expr::DoubleLit(_)
            | Expr::BoolLit(_)
            | Expr::CharLit(_)
            | Expr::StringLit(_) => literal(self.file.expr(expression), ty)
                .map(Evaluated::Value)
                .ok_or(NotEvaluated::NotConst),
            Expr::Name(_) | Expr::Member { .. } => self.evaluate_read(expression, depth),
            Expr::Template(parts) => {
                let mut output = KtStringBuf::new();
                for part in parts {
                    match part {
                        TemplatePart::Str(text) => output.push_kt(text),
                        TemplatePart::Expr(part)
                            if matches!(self.file.expr(*part), Expr::NullLit) =>
                        {
                            output.push_str("null")
                        }
                        TemplatePart::Expr(part) => {
                            let value = self.evaluate_operand(*part, depth)?;
                            value.push_text(&mut output).ok_or(NotEvaluated::NotConst)?;
                        }
                    }
                }
                Ok(Evaluated::Value(ConstValue::String(output.finish())))
            }
            Expr::Unary { op, operand } => {
                let operator = match op {
                    UnOp::Neg => SyntheticOperatorCall::UnaryMinus,
                    UnOp::Plus => SyntheticOperatorCall::UnaryPlus,
                    UnOp::Not => SyntheticOperatorCall::Not,
                };
                let operation = self.operator_operation(expression, operator, &[*operand])?;
                self.apply(operation, &[*operand], ty, depth)
            }
            Expr::Binary { op, lhs, rhs, .. } => {
                self.evaluate_binary(expression, *op, *lhs, *rhs, depth)
            }
            Expr::As {
                operand,
                nullable: false,
                ..
            } => {
                let Evaluated::Value(value) = self.evaluate_constant(*operand, depth)? else {
                    return Err(NotEvaluated::NotConst);
                };
                let target = ty.canonical_semantic();
                crate::assignable::is_subtype(
                    &crate::assignable::TyCtx::new(),
                    self,
                    value.ty(),
                    target,
                )
                .then_some(Evaluated::Value(value))
                .ok_or(NotEvaluated::NotConst)
            }
            Expr::Call { callee, args } => {
                let Some(call) = self.resolved_calls.get(&expression) else {
                    return Err(NotEvaluated::NotConst);
                };
                let operation = self.call_operation(call).ok_or(NotEvaluated::NotConst)?;
                let mut operands = Vec::with_capacity(args.len() + 1);
                match (call, self.file.expr(*callee)) {
                    (
                        ResolvedCall::Member(_) | ResolvedCall::Extension(_),
                        Expr::Member { receiver, .. },
                    ) => operands.push(*receiver),
                    (ResolvedCall::TopLevel(_), Expr::Name(_)) => {}
                    _ => return Err(NotEvaluated::NotConst),
                }
                operands.extend(args.iter().copied());
                self.apply(operation, &operands, ty, depth)
            }
            Expr::Index { array, indices } => {
                let call = self
                    .resolved_calls
                    .get(&expression)
                    .ok_or(NotEvaluated::NotConst)?;
                let operation = self.call_operation(call).ok_or(NotEvaluated::NotConst)?;
                let operands = std::iter::once(*array)
                    .chain(indices.iter().copied())
                    .collect::<Vec<_>>();
                self.apply(operation, &operands, ty, depth)
            }
            Expr::CallableRef { .. } => self
                .callable_reference_name(expression)
                .map(Evaluated::CallableReference)
                .ok_or(NotEvaluated::NotConst),
            _ => Err(NotEvaluated::NotConst),
        }
    }

    fn evaluate_binary(
        &self,
        expression: ExprId,
        operation: BinOp,
        lhs: ExprId,
        rhs: ExprId,
        depth: u32,
    ) -> Evaluation {
        let ty = self.expr_types[expression.0 as usize];
        let operator = match operation {
            BinOp::Add => SyntheticOperatorCall::Plus,
            BinOp::Sub => SyntheticOperatorCall::Minus,
            BinOp::Mul => SyntheticOperatorCall::Times,
            BinOp::Div => SyntheticOperatorCall::Div,
            BinOp::Rem => SyntheticOperatorCall::Rem,
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => SyntheticOperatorCall::CompareTo,
            BinOp::RefEq | BinOp::RefNe => return Err(NotEvaluated::NotConst),
            BinOp::Eq | BinOp::Ne => {
                let mut values = Vec::with_capacity(2);
                for operand in [lhs, rhs] {
                    let operand_ty = self.expr_types[operand.0 as usize];
                    let unsigned = operand_ty.canonical_semantic().is_unsigned();
                    if unsigned && !self.intrinsic_const_evaluation() {
                        return Err(NotEvaluated::NotConst);
                    }
                    values.push(self.evaluate_operand(operand, depth)?);
                }
                let equal = values[0].kotlin_equals(&values[1]);
                return Ok(Evaluated::Value(ConstValue::Boolean(
                    equal == (operation == BinOp::Eq),
                )));
            }
            BinOp::And | BinOp::Or => {
                let boolean = |operand: ExprId| {
                    self.expr_types[operand.0 as usize].canonical_semantic() == Ty::Boolean
                };
                if !boolean(lhs) || !boolean(rhs) {
                    return Err(NotEvaluated::NotConst);
                }
                let left = self.evaluate_constant(lhs, depth)?;
                let right = self.evaluate_constant(rhs, depth)?;
                let (
                    Evaluated::Value(ConstValue::Boolean(left)),
                    Evaluated::Value(ConstValue::Boolean(right)),
                ) = (left, right)
                else {
                    return Err(NotEvaluated::NotConst);
                };
                return Ok(Evaluated::Value(ConstValue::Boolean(
                    if operation == BinOp::And {
                        left && right
                    } else {
                        left || right
                    },
                )));
            }
        };
        let selected = self.operator_operation(expression, operator, &[lhs, rhs])?;
        if operator != SyntheticOperatorCall::CompareTo {
            return self.apply(selected, &[lhs, rhs], ty, depth);
        }
        let Evaluated::Value(ConstValue::Int(order)) =
            self.apply(selected, &[lhs, rhs], Ty::Int, depth)?
        else {
            return Err(NotEvaluated::NotConst);
        };
        Ok(Evaluated::Value(ConstValue::Boolean(match operation {
            BinOp::Lt => order < 0,
            BinOp::Le => order <= 0,
            BinOp::Gt => order > 0,
            _ => order >= 0,
        })))
    }

    /// The compile-time operation an operator expression applies: the one its selected callable
    /// declares, or the builtin primitive operator when checking applied the primitive operation
    /// table without selecting a callable.
    fn operator_operation(
        &self,
        expression: ExprId,
        operator: SyntheticOperatorCall,
        operands: &[ExprId],
    ) -> Result<IntrinsicConstOperation, NotEvaluated> {
        if let Some(call) = self.resolved_operator_calls.get(&(expression, operator)) {
            return self.call_operation(call).ok_or(NotEvaluated::NotConst);
        }
        let primitive = operands.iter().all(|operand| {
            let ty = self.expr_types[operand.0 as usize].canonical_semantic();
            ty.is_numeric_or_char() || ty == Ty::Boolean
        });
        if !primitive {
            return Err(NotEvaluated::NotConst);
        }
        Ok(match operator {
            SyntheticOperatorCall::Plus => IntrinsicConstOperation::Plus,
            SyntheticOperatorCall::Minus => IntrinsicConstOperation::Minus,
            SyntheticOperatorCall::Times => IntrinsicConstOperation::Times,
            SyntheticOperatorCall::Div => IntrinsicConstOperation::Div,
            SyntheticOperatorCall::Rem => IntrinsicConstOperation::Rem,
            SyntheticOperatorCall::UnaryMinus => IntrinsicConstOperation::UnaryMinus,
            SyntheticOperatorCall::UnaryPlus => IntrinsicConstOperation::UnaryPlus,
            SyntheticOperatorCall::Not => IntrinsicConstOperation::Not,
            SyntheticOperatorCall::CompareTo => IntrinsicConstOperation::CompareTo,
            _ => return Err(NotEvaluated::NotConst),
        })
    }

    /// The compile-time operation `call` selects (kotlinc's `isCompileTimeBuiltinCall`).
    fn call_operation(&self, call: &ResolvedCall) -> Option<IntrinsicConstOperation> {
        let declaration = match call {
            // A member's owner is its platform class. A constant-typed receiver's members are
            // declared by its builtin classifier or a supertype of it, all in package `kotlin`;
            // a member of any other receiver has no constant receiver operand.
            ResolvedCall::Member(member) if is_constant_type(member.receiver) => {
                let receiver = member.receiver.canonical_semantic();
                IntrinsicConstDeclaration {
                    owner: IntrinsicConstOwner::Classifier(receiver),
                    name: &member.member.name,
                    receiver: Some(receiver),
                    first_parameter: member.member.params.first().copied(),
                }
            }
            ResolvedCall::TopLevel(call) => {
                callable_declaration(&call.callable, None, call.callable.params.first().copied())?
            }
            ResolvedCall::Extension(call) => callable_declaration(
                &call.callable,
                Some(call.callable.source_receiver.unwrap_or(call.receiver)),
                call.params.first().copied(),
            )?,
            ResolvedCall::Member(_)
            | ResolvedCall::Companion(_)
            | ResolvedCall::MemberExtension { .. }
            | ResolvedCall::LocalFunction(_) => return None,
        };
        compile_time_operation(declaration, self.intrinsic_const_evaluation())
    }

    /// Evaluate `operation` over `operands`, each of which must have a constant type and value.
    fn apply(
        &self,
        operation: IntrinsicConstOperation,
        operands: &[ExprId],
        result: Ty,
        depth: u32,
    ) -> Evaluation {
        let mut values = Vec::with_capacity(operands.len());
        for operand in operands {
            values.push(self.evaluate_operand(*operand, depth)?);
        }
        evaluate(operation, &values, result)
            .map(Evaluated::Value)
            .map_err(|_| NotEvaluated::NotConst)
    }

    /// An operand of a call or template: an expression of a constant type (kotlinc's
    /// `hasAllowedCompileTimeType`) with a value.
    fn evaluate_operand(&self, operand: ExprId, depth: u32) -> Result<ConstValue, NotEvaluated> {
        if !is_constant_type(self.expr_types[operand.0 as usize]) {
            return Err(NotEvaluated::NotConst);
        }
        match self.evaluate_constant(operand, depth)? {
            Evaluated::Value(value) => Ok(value),
            Evaluated::EnumEntry(_) | Evaluated::CallableReference(_) => {
                Err(NotEvaluated::NotConst)
            }
        }
    }

    /// A property or enum-entry read.
    fn evaluate_read(&self, expression: ExprId, depth: u32) -> Evaluation {
        if let Some(constant) = self.resolved_constants.get(&expression) {
            return ConstValue::from_library(constant)
                .map(Evaluated::Value)
                .ok_or(NotEvaluated::NotConst);
        }
        if let Some(entry) = self.resolved_enum_entries.get(&expression) {
            return Ok(Evaluated::EnumEntry(entry.name.clone()));
        }
        let lowering = self.expr_lowers.get(&expression);
        let Some(lowering) = lowering else {
            return Err(if self.expr_types[expression.0 as usize] == Ty::Error {
                NotEvaluated::ResolutionError
            } else {
                NotEvaluated::NotConst
            });
        };
        let receiver = match self.file.expr(expression) {
            Expr::Member { receiver, .. } => Some(*receiver),
            _ => None,
        };
        let (intrinsic, role) = selected_property_facts(lowering);
        match (intrinsic, role, receiver) {
            (Some(CompilerIntrinsic::StringLength), _, Some(receiver)) => {
                let value = self.evaluate_operand(receiver, depth)?;
                return evaluate(IntrinsicConstOperation::Length, &[value], Ty::Int)
                    .map(Evaluated::Value)
                    .map_err(|_| NotEvaluated::NotConst);
            }
            (Some(CompilerIntrinsic::CharCode), _, Some(receiver)) => {
                let value = self.evaluate_operand(receiver, depth)?;
                return evaluate(IntrinsicConstOperation::Code, &[value], Ty::Int)
                    .map(Evaluated::Value)
                    .map_err(|_| NotEvaluated::NotConst);
            }
            (Some(CompilerIntrinsic::EnumName), _, Some(receiver))
            | (_, Some(SemanticCallRole::KotlinCallableReferenceName), Some(receiver))
                if self.intrinsic_const_evaluation() =>
            {
                return match self.evaluate_constant(receiver, depth)? {
                    Evaluated::EnumEntry(name) | Evaluated::CallableReference(name) => {
                        Ok(Evaluated::Value(ConstValue::String(KtString::from(name))))
                    }
                    Evaluated::Value(_) => Err(NotEvaluated::NotConst),
                };
            }
            _ => {}
        }
        if self.reads_literal_initialized_val(lowering) {
            Err(NotEvaluated::NonConstVal)
        } else {
            Err(NotEvaluated::NotConst)
        }
    }

    /// Whether the selected property is a non-`const` source `val` whose initializer is a literal
    /// (kotlinc's `NotConstValInConstExpression`). That is known only for a declaration whose
    /// syntax is part of the fragment being checked: a sibling member of the same singleton. Other
    /// declarations' initializers are not retained, and their reads report a non-constant
    /// initializer instead.
    fn reads_literal_initialized_val(&self, lowering: &ExprLowering) -> bool {
        let declaration = match lowering {
            ExprLowering::TopLevelPropertyGet(access) => access.property.stable_declaration,
            ExprLowering::MemberPropertyRead {
                stable_declaration,
                source_member,
                ..
            } => stable_declaration
                .or_else(|| self.active_source_member_declaration((*source_member)?)),
            ExprLowering::AssociatedPropertyRead {
                stable_declaration, ..
            } => *stable_declaration,
            _ => None,
        };
        let property = declaration
            .and_then(|declaration| self.active_declarations?.property(self.file, declaration));
        let Some(property) = property else {
            return false;
        };
        !property.is_const
            && !property.is_var
            && property
                .init
                .is_some_and(|initializer| is_literal(self.file, initializer))
    }

    /// The Kotlin name of the declaration a callable reference selects.
    fn callable_reference_name(&self, expression: ExprId) -> Option<String> {
        Some(match self.expr_lowers.get(&expression)? {
            ExprLowering::ConstructorRef { .. } => "<init>".to_string(),
            ExprLowering::TopLevelFunctionRef(reference) => reference.target.name.clone(),
            ExprLowering::AdaptedRef { target, .. } => target.name.clone(),
            ExprLowering::CallableReference { target, .. }
            | ExprLowering::AdaptedCallableReference { target, .. } => match target {
                CallableReferenceTarget::Classifier(member) => member.name.clone(),
                CallableReferenceTarget::ClassifierProperty { name, .. } => name.clone(),
                CallableReferenceTarget::Extension { callable, .. } => callable.name.clone(),
                CallableReferenceTarget::Member { member, .. } => member.name.clone(),
                CallableReferenceTarget::Property(property) => property.name.clone(),
                CallableReferenceTarget::TopLevelProperty(property) => property.name.clone(),
            },
            _ => return None,
        })
    }
}

/// The declaration facts of a selected package-level or extension callable.
fn callable_declaration(
    callable: &crate::libraries::LibraryCallable,
    receiver: Option<Ty>,
    first_parameter: Option<Ty>,
) -> Option<IntrinsicConstDeclaration<'_>> {
    let package = match callable.declaration_owner? {
        SemanticCallableOwner::Package(package) => package,
        SemanticCallableOwner::Classifier(_) => return None,
    };
    Some(IntrinsicConstDeclaration {
        owner: IntrinsicConstOwner::Package(package),
        name: &callable.name,
        receiver,
        first_parameter,
    })
}

/// The compiler intrinsic and language role of the property a read selected.
fn selected_property_facts(
    lowering: &ExprLowering,
) -> (Option<CompilerIntrinsic>, Option<SemanticCallRole>) {
    match lowering {
        ExprLowering::MemberPropertyRead {
            compiler_intrinsic,
            accessor,
            ..
        } => (
            *compiler_intrinsic,
            accessor
                .as_ref()
                .and_then(|accessor| accessor.semantic_role),
        ),
        ExprLowering::ExtensionPropertyGet { access } => (
            access.property.getter.compiler_intrinsic,
            access.property.getter.semantic_role,
        ),
        ExprLowering::IntrinsicProperty(member) => (
            match member.realization {
                MemberRealization::Intrinsic(intrinsic) => Some(intrinsic),
                _ => None,
            },
            member.semantic_role,
        ),
        _ => (None, None),
    }
}

/// kotlinc's `constantAllowedTypes`, non-null.
fn is_constant_type(ty: Ty) -> bool {
    matches!(
        ty.canonical_semantic(),
        Ty::Boolean
            | Ty::Char
            | Ty::Byte
            | Ty::Short
            | Ty::Int
            | Ty::Long
            | Ty::Float
            | Ty::Double
            | Ty::UByte
            | Ty::UShort
            | Ty::UInt
            | Ty::ULong
            | Ty::String
    )
}

/// A literal's value at the type checking gave it: an integer literal takes the integral type it
/// was adapted to.
fn literal(expression: &Expr, ty: Ty) -> Option<ConstValue> {
    let ty = ty.canonical_semantic();
    Some(match expression {
        Expr::IntLit(value)
        | Expr::LongLit(value)
        | Expr::UIntLit(value)
        | Expr::ULongLit(value) => {
            let natural = match expression {
                Expr::IntLit(_) => Ty::Int,
                Expr::LongLit(_) => Ty::Long,
                Expr::UIntLit(_) => Ty::UInt,
                _ => Ty::ULong,
            };
            let integral = matches!(
                ty,
                Ty::Byte
                    | Ty::Short
                    | Ty::Int
                    | Ty::Long
                    | Ty::UByte
                    | Ty::UShort
                    | Ty::UInt
                    | Ty::ULong
            );
            let target = if integral { ty } else { natural };
            match target {
                Ty::Byte => ConstValue::Byte(*value as i8),
                Ty::Short => ConstValue::Short(*value as i16),
                Ty::Int => ConstValue::Int(*value as i32),
                Ty::Long => ConstValue::Long(*value),
                Ty::UByte => ConstValue::UByte(*value as u8),
                Ty::UShort => ConstValue::UShort(*value as u16),
                Ty::UInt => ConstValue::UInt(*value as u32),
                _ => ConstValue::ULong(*value as u64),
            }
        }
        Expr::FloatLit(value) => ConstValue::Float(*value),
        Expr::DoubleLit(value) => ConstValue::Double(*value),
        Expr::BoolLit(value) => ConstValue::Boolean(*value),
        Expr::CharLit(value) => ConstValue::Char(*value),
        Expr::StringLit(value) => ConstValue::String(value.clone()),
        _ => return None,
    })
}

fn trace_call(call: &ResolvedCall) -> String {
    match call {
        ResolvedCall::Member(member) => format!(
            "member {:?}.{} params={:?} receiver={:?}",
            member.member.owner, member.member.name, member.member.params, member.receiver
        ),
        ResolvedCall::TopLevel(call) => format!(
            "top-level {:?} {} ann={:?}",
            call.callable.declaration_owner, call.callable.name, call.callable.annotations
        ),
        ResolvedCall::Extension(call) => format!(
            "extension {:?} {} ann={:?}",
            call.callable.declaration_owner, call.callable.name, call.callable.annotations
        ),
        _ => "other".to_string(),
    }
}

fn trace_lowering(lowering: &ExprLowering) -> &'static str {
    match lowering {
        ExprLowering::TopLevelPropertyGet(_) => "top-level property",
        ExprLowering::MemberPropertyRead { .. } => "member property",
        ExprLowering::AssociatedPropertyRead { .. } => "associated property",
        ExprLowering::ExtensionPropertyGet { .. } => "extension property",
        ExprLowering::IntrinsicProperty(_) => "intrinsic property",
        ExprLowering::ClassifierPropertyRead { .. } => "classifier property",
        ExprLowering::SingletonValue(_) => "singleton",
        ExprLowering::CallableReference { .. } => "callable reference",
        ExprLowering::ConstructorRef { .. } => "constructor reference",
        ExprLowering::TopLevelFunctionRef(_) => "top-level function reference",
        _ => "other",
    }
}

/// Whether an initializer is what kotlinc builds a literal expression from: a literal, or a
/// negated numeric literal.
fn is_literal(file: &crate::ast::File, initializer: ExprId) -> bool {
    // The syntax retained for constant publication drops other declarations' initializers.
    if initializer.0 as usize >= file.expr_arena.len() {
        return false;
    }
    match file.expr(initializer) {
        Expr::IntLit(_)
        | Expr::LongLit(_)
        | Expr::UIntLit(_)
        | Expr::ULongLit(_)
        | Expr::FloatLit(_)
        | Expr::DoubleLit(_)
        | Expr::BoolLit(_)
        | Expr::CharLit(_)
        | Expr::StringLit(_)
        | Expr::NullLit => true,
        Expr::Template(parts) => parts
            .iter()
            .all(|part| matches!(part, TemplatePart::Str(_))),
        Expr::Unary {
            op: UnOp::Neg,
            operand,
        } => matches!(
            file.expr(*operand),
            Expr::IntLit(_) | Expr::LongLit(_) | Expr::FloatLit(_) | Expr::DoubleLit(_)
        ),
        _ => false,
    }
}
