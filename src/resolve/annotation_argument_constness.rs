//! Why an annotation argument did not fold to a compile-time constant, reported as kotlinc's
//! `FirAnnotationArgumentChecker` does: on the argument (or the array element) itself, never on the
//! annotation.

use super::*;

/// The diagnostic one non-constant annotation argument earns (kotlinc's `ConstantArgumentKind`).
enum NonConstantArgument {
    /// `ANNOTATION_ARGUMENT_MUST_BE_CONST`.
    NotConst,
    /// `ANNOTATION_ARGUMENT_MUST_BE_KCLASS_LITERAL`: a class literal of a value (`value::class`).
    NotKClassLiteral,
    /// `NON_CONST_VAL_USED_IN_CONSTANT_EXPRESSION`: a `val` that could have been `const`.
    NonConstVal,
    /// The argument already failed resolution, which is its only diagnostic.
    ResolutionError,
}

impl Checker<'_> {
    /// Report the arguments of an application whose values did not fold. Returns false when no
    /// argument is one kotlinc classifies, so the application's fold failure is krusty's own.
    pub(super) fn report_non_constant_annotation_arguments(
        &mut self,
        arguments: &[ExprId],
    ) -> bool {
        let mut leaves = Vec::new();
        for &argument in arguments {
            self.annotation_argument_leaves(argument, &mut leaves);
        }
        let mut classified = false;
        for leaf in leaves {
            let Some(kind) = self.non_constant_argument(leaf) else {
                continue;
            };
            classified = true;
            let message = match kind {
                NonConstantArgument::NotConst => {
                    "annotation argument must be a compile-time constant."
                }
                NonConstantArgument::NotKClassLiteral => {
                    "annotation argument must be class literal (T::class)."
                }
                NonConstantArgument::NonConstVal => {
                    "only 'const val' can be used in constant expressions."
                }
                NonConstantArgument::ResolutionError => continue,
            };
            self.diags.error(self.span(leaf), message.to_string());
        }
        classified
    }

    /// The elements an argument's value is made of: an array literal or a vararg's elements are each
    /// checked on their own.
    fn annotation_argument_leaves(&self, argument: ExprId, leaves: &mut Vec<ExprId>) {
        use crate::synthetics::SyntheticKind;
        match self.file.expr(argument) {
            Expr::AnnotationArrayLiteral(elements) => {
                for &element in elements {
                    self.annotation_argument_leaves(element, leaves);
                }
            }
            Expr::Call { args, .. }
                if matches!(
                    self.expr_lowers.get(&argument),
                    Some(ExprLowering::CompilerSynthetic(
                        SyntheticKind::PrimitiveVararg(_)
                            | SyntheticKind::ReferenceVararg
                            | SyntheticKind::EmptyReference,
                    ))
                ) =>
            {
                for &element in args {
                    self.annotation_argument_leaves(element, leaves);
                }
            }
            _ => leaves.push(argument),
        }
    }

    /// A class literal folds only when its receiver selected a classifier. One whose receiver
    /// failed to resolve is not a constant; one whose receiver is a value is not a class literal.
    fn non_constant_argument(&self, leaf: ExprId) -> Option<NonConstantArgument> {
        if let Expr::CallableRef {
            receiver: Some(receiver),
            name,
        } = self.file.expr(leaf)
        {
            if name == "class" && !self.class_literal_targets.contains_key(&leaf) {
                return Some(if self.expr_types[receiver.0 as usize] == Ty::Error {
                    NonConstantArgument::NotConst
                } else {
                    NonConstantArgument::NotKClassLiteral
                });
            }
        }
        if self.expr_types[leaf.0 as usize] == Ty::Error {
            return Some(NonConstantArgument::ResolutionError);
        }
        self.non_constant_expression(leaf)
    }

    /// kotlinc's `computeConstantExpressionKind` for the expression forms krusty selects with a
    /// known answer: an operator is as constant as its first non-constant operand, a lexical value
    /// or a branch is never constant, a property read is constant only for a `const val`, and a
    /// call of a module function is never constant. Other forms (library calls kotlinc may
    /// evaluate) are not classified here.
    fn non_constant_expression(&self, expression: ExprId) -> Option<NonConstantArgument> {
        if self.resolved_constants.contains_key(&expression)
            || self.resolved_enum_entries.contains_key(&expression)
        {
            return None;
        }
        match self.file.expr(expression) {
            Expr::Binary { lhs, rhs, .. } => self
                .non_constant_expression(*lhs)
                .or_else(|| self.non_constant_expression(*rhs)),
            Expr::Unary { operand, .. } => self.non_constant_expression(*operand),
            Expr::If { .. } | Expr::When { .. } => Some(NonConstantArgument::NotConst),
            Expr::Call { .. } => self
                .resolved_calls
                .get(&expression)
                .is_some_and(|call| match call {
                    ResolvedCall::TopLevel(call) => {
                        call.stable_declaration.is_some() || call.source_decl.is_some()
                    }
                    ResolvedCall::Extension(call) => {
                        call.stable_declaration.is_some() || call.source.is_some()
                    }
                    _ => false,
                })
                .then_some(NonConstantArgument::NotConst),
            _ if matches!(
                self.read_flow_roots.get(&expression),
                Some(scope::PathRoot::Value(_))
            ) =>
            {
                Some(NonConstantArgument::NotConst)
            }
            _ => self.non_constant_property_read(expression),
        }
    }

    /// A selected non-`const` property read. Only a module `val` with a default getter and a
    /// literal initializer could have been `const`; a dependency's declaration carries no
    /// initializer, so its reads are simply not constant.
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
}
