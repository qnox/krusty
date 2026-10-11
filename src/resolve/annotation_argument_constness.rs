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
        (self.expr_types[leaf.0 as usize] == Ty::Error)
            .then_some(NonConstantArgument::ResolutionError)
    }
}
