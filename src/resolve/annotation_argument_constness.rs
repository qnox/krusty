//! Why an annotation argument did not fold to a compile-time constant, reported as kotlinc's
//! `FirAnnotationArgumentChecker` does: on the argument (or the array element) itself, never on the
//! annotation.

use super::*;

/// The diagnostic one non-constant annotation argument earns (kotlinc's `ConstantArgumentKind`).
#[derive(Clone, Copy)]
enum NonConstantArgument {
    /// `ANNOTATION_ARGUMENT_MUST_BE_CONST`.
    NotConst,
    /// `ANNOTATION_ARGUMENT_MUST_BE_ENUM_CONST`: a property of enum type, not an entry.
    NotEnumConstant,
    /// `ANNOTATION_ARGUMENT_MUST_BE_KCLASS_LITERAL`: a class literal of a value (`value::class`).
    NotKClassLiteral,
    /// `NON_CONST_VAL_USED_IN_CONSTANT_EXPRESSION`: a `val` that could have been `const`.
    NonConstVal,
    /// The argument already failed resolution, which is its only diagnostic.
    ResolutionError,
}

/// Whether a value stands directly in an annotation slot or inside a constant expression. Enum
/// entries and class literals are legal in the former position, but are not scalar constants an
/// operator or template can consume.
#[derive(Clone, Copy, Eq, PartialEq)]
enum ConstantUse {
    DirectAnnotationValue,
    EmbeddedExpression,
}

impl Checker<'_> {
    /// Report the arguments of an application whose values did not fold. Returns false when no
    /// argument is one kotlinc classifies, so the application's fold failure is krusty's own.
    pub(super) fn report_non_constant_annotation_arguments(
        &mut self,
        arguments: &[ExprId],
    ) -> bool {
        let mut diagnostics = Vec::new();
        for &argument in arguments {
            self.annotation_argument_diagnostics(argument, &mut diagnostics);
        }
        for (expression, kind) in &diagnostics {
            let message = match kind {
                NonConstantArgument::NotConst => {
                    "annotation argument must be a compile-time constant."
                }
                NonConstantArgument::NotEnumConstant => {
                    "enum annotation argument must be an enum constant."
                }
                NonConstantArgument::NotKClassLiteral => {
                    "annotation argument must be class literal (T::class)."
                }
                NonConstantArgument::NonConstVal => {
                    "only 'const val' can be used in constant expressions."
                }
                NonConstantArgument::ResolutionError => continue,
            };
            self.diags
                .error(self.span(*expression), message.to_string());
        }
        !diagnostics.is_empty()
    }

    /// Collect kotlinc's diagnostics for one argument in emission order. An array (a literal, an
    /// `arrayOf`-family call, or the operand of a vararg spread) reports an aggregate
    /// `NON_CONST_VAL_USED_IN_CONSTANT_EXPRESSION` before its element diagnostics when any element
    /// is non-constant, except for dedicated enum/KClass and resolution-error cases.
    fn annotation_argument_diagnostics(
        &self,
        argument: ExprId,
        diagnostics: &mut Vec<(ExprId, NonConstantArgument)>,
    ) {
        let elements = match self.file.expr(argument) {
            Expr::AnnotationArrayLiteral(elements) => elements.as_slice(),
            _ => match self.synthetic_vararg_elements(argument) {
                Some(elements) => elements,
                None => {
                    if let Some(kind) = self.non_constant_argument(argument) {
                        diagnostics.push((argument, kind));
                    }
                    return;
                }
            },
        };
        let first_element_diagnostic = diagnostics.len();
        for &element in elements {
            self.annotation_argument_diagnostics(element, diagnostics);
        }
        if diagnostics[first_element_diagnostic..]
            .iter()
            .any(|(_, kind)| {
                matches!(
                    kind,
                    NonConstantArgument::NotConst | NonConstantArgument::NonConstVal
                )
            })
        {
            diagnostics.insert(
                first_element_diagnostic,
                (argument, NonConstantArgument::NonConstVal),
            );
        }
    }

    /// Source elements packed by an array-construction synthetic (`arrayOf`), in their own order.
    fn synthetic_vararg_elements(&self, argument: ExprId) -> Option<&[ExprId]> {
        use crate::synthetics::SyntheticKind;
        let Expr::Call { args, .. } = self.file.expr(argument) else {
            return None;
        };
        matches!(
            self.expr_lowers.get(&argument),
            Some(ExprLowering::CompilerSynthetic(
                SyntheticKind::PrimitiveVararg(_)
                    | SyntheticKind::ReferenceVararg
                    | SyntheticKind::EmptyReference,
            ))
        )
        .then_some(args.as_slice())
    }

    /// The first non-constant source value inside one annotation slot. Nested annotations inherit
    /// this kind at their own call span; unlike a direct array diagnostic, they do not inherit the
    /// array's additional aggregate diagnostic.
    fn first_non_constant_annotation_argument(
        &self,
        argument: ExprId,
    ) -> Option<NonConstantArgument> {
        if let Some(elements) = self.synthetic_vararg_elements(argument) {
            return elements
                .iter()
                .find_map(|&element| self.first_non_constant_annotation_argument(element));
        }
        match self.file.expr(argument) {
            Expr::AnnotationArrayLiteral(elements) => elements
                .iter()
                .find_map(|&element| self.first_non_constant_annotation_argument(element)),
            _ => self.non_constant_argument(argument),
        }
    }

    /// Whether `call` was checked as a nested annotation: a call the checker typed as an
    /// annotation class without selecting a callable for it.
    fn constructs_nested_annotation(&self, call: ExprId) -> bool {
        !self.resolved_calls.contains_key(&call)
            && self.expr_types[call.0 as usize]
                .kotlin_class_internal()
                .is_some_and(|annotation| self.annotation_shape(annotation).is_some())
    }

    /// A class literal folds only when its receiver selected a classifier. One whose receiver
    /// failed to resolve is not a constant; one whose receiver is a value is not a class literal.
    fn non_constant_argument(&self, leaf: ExprId) -> Option<NonConstantArgument> {
        self.non_constant_argument_for(leaf, ConstantUse::DirectAnnotationValue)
    }

    fn non_constant_argument_for(
        &self,
        leaf: ExprId,
        constant_use: ConstantUse,
    ) -> Option<NonConstantArgument> {
        if let Expr::CallableRef {
            receiver: Some(receiver),
            name,
        } = self.file.expr(leaf)
        {
            if name == "class" {
                if !self.class_literal_targets.contains_key(&leaf) {
                    let kind = if self.expr_types[receiver.0 as usize] == Ty::Error {
                        NonConstantArgument::NotConst
                    } else {
                        NonConstantArgument::NotKClassLiteral
                    };
                    return Some(Self::kind_for_constant_use(kind, constant_use));
                }
                return (constant_use == ConstantUse::EmbeddedExpression)
                    .then_some(NonConstantArgument::NotConst);
            }
        }
        if self.expr_types[leaf.0 as usize] == Ty::Error {
            return Some(NonConstantArgument::ResolutionError);
        }
        self.non_constant_expression(leaf, constant_use)
            .map(|kind| Self::kind_for_constant_use(kind, constant_use))
    }

    /// Enum/KClass diagnostics describe values that stand directly in the annotation slot. Once
    /// embedded in a scalar expression, either value makes that complete expression non-constant.
    fn kind_for_constant_use(
        kind: NonConstantArgument,
        constant_use: ConstantUse,
    ) -> NonConstantArgument {
        match (constant_use, kind) {
            (
                ConstantUse::EmbeddedExpression,
                NonConstantArgument::NotEnumConstant | NonConstantArgument::NotKClassLiteral,
            ) => NonConstantArgument::NotConst,
            _ => kind,
        }
    }

    /// kotlinc's `computeConstantExpressionKind` for the expression forms krusty selects with a
    /// known answer: a compiler-intrinsic operator is as constant as its first non-constant operand,
    /// a lexical value or a branch is never constant, a property read is constant only for a `const
    /// val`, a cast keeps its operand's kind, and an indexed read, type test, range test, null
    /// handling, `try`, lambda, block, branch, selected constructor, or selected non-intrinsic call
    /// is never constant; a call producing a `KClass` is not a class literal. Other forms
    /// (compiler intrinsics kotlinc may evaluate) are not classified here.
    fn non_constant_expression(
        &self,
        expression: ExprId,
        constant_use: ConstantUse,
    ) -> Option<NonConstantArgument> {
        if self.resolved_constants.contains_key(&expression) {
            return None;
        }
        if self.resolved_enum_entries.contains_key(&expression) {
            return (constant_use == ConstantUse::EmbeddedExpression)
                .then_some(NonConstantArgument::NotConst);
        }
        match self.file.expr(expression) {
            Expr::Binary { op, lhs, rhs, .. }
                if self.operator_propagates_constant_kind(expression, *op) =>
            {
                self.non_constant_argument_for(*lhs, ConstantUse::EmbeddedExpression)
                    .or_else(|| {
                        self.non_constant_argument_for(*rhs, ConstantUse::EmbeddedExpression)
                    })
            }
            Expr::Binary { .. } => Some(NonConstantArgument::NotConst),
            Expr::Unary { op, operand }
                if self.unary_operator_propagates_constant_kind(expression, *op) =>
            {
                self.non_constant_argument_for(*operand, ConstantUse::EmbeddedExpression)
            }
            Expr::Unary { .. } => Some(NonConstantArgument::NotConst),
            Expr::Template(parts) => parts.iter().find_map(|part| match part {
                TemplatePart::Str(_) => None,
                TemplatePart::Expr(expression) => {
                    self.non_constant_argument_for(*expression, ConstantUse::EmbeddedExpression)
                }
            }),
            Expr::As { operand, .. } => {
                self.non_constant_argument_for(*operand, ConstantUse::EmbeddedExpression)
            }
            Expr::Index { .. }
            | Expr::SafeIndex { .. }
            | Expr::Is { .. }
            | Expr::InRange { .. }
            | Expr::NotNull { .. }
            | Expr::Elvis { .. }
            | Expr::SafeCall { .. }
            | Expr::Try { .. }
            | Expr::Lambda { .. }
            | Expr::Block { .. }
            | Expr::If { .. }
            | Expr::When { .. } => Some(NonConstantArgument::NotConst),
            // kotlinc reports a nested annotation at its call, with the kind of its first
            // non-constant argument.
            Expr::Call { args, .. } if self.constructs_nested_annotation(expression) => args
                .iter()
                .find_map(|&argument| self.first_non_constant_annotation_argument(argument)),
            Expr::Call { .. } => (self.resolved_constructors.contains_key(&expression)
                || self
                    .resolved_calls
                    .get(&expression)
                    .is_some_and(|call| call.compiler_intrinsic().is_none()))
            .then(|| {
                if self.expr_types[expression.0 as usize].kotlin_class_internal()
                    == Some(crate::types::wk::kclass())
                {
                    NonConstantArgument::NotKClassLiteral
                } else {
                    NonConstantArgument::NotConst
                }
            }),
            _ if let Some(scope::PathRoot::Value(identity)) =
                self.read_flow_roots.get(&expression) =>
            {
                if self.local_properties.contains(identity)
                    && self.expression_has_enum_type(expression)
                {
                    Some(NonConstantArgument::NotEnumConstant)
                } else {
                    Some(NonConstantArgument::NotConst)
                }
            }
            _ => self.non_constant_property_read(expression),
        }
    }

    /// Builtin non-call operators have no selected-call entry. An overload-selected operator only
    /// inherits an operand's diagnostic when the exact declaration carries a compiler intrinsic;
    /// a source overload is an ordinary call and is non-constant as a whole.
    fn operator_call_propagates_constant_kind(
        &self,
        expression: ExprId,
        operator: SyntheticOperatorCall,
    ) -> bool {
        self.resolved_operator_calls
            .get(&(expression, operator))
            .or_else(|| self.resolved_calls.get(&expression))
            .is_none_or(|call| call.compiler_intrinsic().is_some())
    }

    fn operator_propagates_constant_kind(&self, expression: ExprId, op: BinOp) -> bool {
        let operator = match op {
            BinOp::Add => SyntheticOperatorCall::Plus,
            BinOp::Sub => SyntheticOperatorCall::Minus,
            BinOp::Mul => SyntheticOperatorCall::Times,
            BinOp::Div => SyntheticOperatorCall::Div,
            BinOp::Rem => SyntheticOperatorCall::Rem,
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => SyntheticOperatorCall::CompareTo,
            BinOp::Eq | BinOp::Ne | BinOp::And | BinOp::Or | BinOp::RefEq | BinOp::RefNe => {
                return true;
            }
        };
        self.operator_call_propagates_constant_kind(expression, operator)
    }

    fn unary_operator_propagates_constant_kind(&self, expression: ExprId, op: UnOp) -> bool {
        let operator = match op {
            UnOp::Neg => SyntheticOperatorCall::UnaryMinus,
            UnOp::Plus => SyntheticOperatorCall::UnaryPlus,
            UnOp::Not => SyntheticOperatorCall::Not,
        };
        self.operator_call_propagates_constant_kind(expression, operator)
    }

    /// A selected non-`const` property read. Enum and KClass results keep their type-specific
    /// diagnostics. Otherwise only a module `val` with a default getter and a literal initializer
    /// could have been `const`; a dependency's declaration carries no initializer, so its reads are
    /// simply not constant.
    fn non_constant_property_read(&self, expression: ExprId) -> Option<NonConstantArgument> {
        let declaration = match self.expr_lowers.get(&expression)? {
            ExprLowering::TopLevelPropertyGet(access)
            | ExprLowering::ExtensionPropertyGet { access } => access.property.stable_declaration,
            // kotlinc evaluates an intrinsic member (`"a".length`); folding it is krusty's gap.
            ExprLowering::MemberPropertyRead {
                compiler_intrinsic: Some(_),
                ..
            } => return None,
            ExprLowering::MemberPropertyRead {
                stable_declaration, ..
            }
            | ExprLowering::AssociatedPropertyRead {
                stable_declaration, ..
            }
            | ExprLowering::MemberExtensionPropertyRead {
                stable_declaration, ..
            } => *stable_declaration,
            ExprLowering::ClassifierPropertyRead { .. } => None,
            _ => return None,
        };
        let ty = self.expr_types[expression.0 as usize];
        if ty.kotlin_class_internal() == Some(crate::types::wk::kclass()) {
            return Some(NonConstantArgument::NotKClassLiteral);
        }
        if self.expression_has_enum_type(expression) {
            return Some(NonConstantArgument::NotEnumConstant);
        }
        let flags = declaration
            .and_then(|declaration| self.resolved_index?.declaration_header(declaration))
            .map(|header| header.flags);
        use crate::fir::DeclarationFlags as F;
        Some(match flags {
            Some(flags) if flags.has(F::CONST) => return None,
            Some(flags)
                if flags.has(F::LITERAL_INITIALIZER)
                    && !flags.has(F::MUTABLE)
                    && !flags.has(F::CUSTOM_GETTER)
                    && !flags.has(F::DELEGATED) =>
            {
                NonConstantArgument::NonConstVal
            }
            _ => NonConstantArgument::NotConst,
        })
    }

    /// Whether checking gave this expression an enum classifier. Source and dependency enums are
    /// normalized by the symbol provider before this query; no declaration spelling is inspected.
    fn expression_has_enum_type(&self, expression: ExprId) -> bool {
        self.expr_types[expression.0 as usize]
            .kotlin_class_internal()
            .and_then(|classifier| self.resolver().classifier(classifier))
            .is_some_and(|classifier| classifier.is_enum())
    }
}
