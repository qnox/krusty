//! Checked bottom-value completion at the common-IR/backend boundary.
//!
//! Kotlin's non-null `Nothing` is semantically divergent, but some target calls and assertions
//! still have a physical fallthrough path. Common lowering records that mismatch once as
//! [`IrExpr::BottomValue`]. A backend emits the producer according to its own representation and
//! then realizes the recorded completion mode without re-reading AST shape or logical-type maps.

use super::{Callee, ExprId, IrExpr, IrFile, IrTypeOp};
use crate::types::Ty;

/// What a discarded use of a checked bottom value does. Value uses always terminate; a substituted
/// generic result is the Kotlin/JVM exception, where kotlinc discards the erased result and permits
/// statement fallthrough.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrBottomValueCompletion {
    Diverge,
    FallThroughWhenDiscarded,
}

impl IrBottomValueCompletion {
    pub fn diverges_when_discarded(self) -> bool {
        self == Self::Diverge
    }
}

/// Attach the semantic completion contract to a freshly lowered expression. The checked type is
/// passed directly by the frontend; this module never reconstructs it from spelling or target ABI.
pub(crate) fn complete_bottom_value(
    ir: &mut IrFile,
    expression: ExprId,
    checked_type: Ty,
) -> ExprId {
    if checked_type.is_nullable() || checked_type.non_null() != Ty::Nothing {
        return expression;
    }
    let Some(completion) = completion_for(ir, expression) else {
        return expression;
    };
    ir.add_expr(IrExpr::BottomValue {
        producer: expression,
        completion,
    })
}

fn completion_for(ir: &IrFile, mut expression: ExprId) -> Option<IrBottomValueCompletion> {
    let mut substituted_generic = false;
    loop {
        match ir.expr(expression) {
            IrExpr::BottomValue { .. } => return None,
            IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg,
                type_operand,
            } if !type_operand.is_nullable() && type_operand.non_null() == Ty::Nothing => {
                substituted_generic = true;
                expression = *arg;
            }
            IrExpr::Block {
                value: Some(value), ..
            } => expression = *value,
            IrExpr::NotNullAssert { .. } => return Some(IrBottomValueCompletion::Diverge),
            IrExpr::MethodCall { .. } => {
                return Some(if substituted_generic {
                    IrBottomValueCompletion::FallThroughWhenDiscarded
                } else {
                    IrBottomValueCompletion::Diverge
                });
            }
            IrExpr::Call { callee, .. } if physically_invoked(callee) => {
                return Some(if substituted_generic {
                    IrBottomValueCompletion::FallThroughWhenDiscarded
                } else {
                    IrBottomValueCompletion::Diverge
                });
            }
            _ => return None,
        }
    }
}

fn physically_invoked(callee: &Callee) -> bool {
    match callee {
        Callee::Intrinsic { .. } => false,
        Callee::Static { inline, .. } => !inline.can_inline(),
        _ => true,
    }
}

impl IrFile {
    pub(crate) fn expr_diverges_by(
        &self,
        expression: ExprId,
        leaf: &impl Fn(ExprId, &IrExpr) -> bool,
    ) -> bool {
        self.expr_diverges_by_use(expression, false, leaf)
    }

    /// Whether evaluating `expression` for effect always transfers control. A bottom completion
    /// selected as [`IrBottomValueCompletion::FallThroughWhenDiscarded`] is non-divergent only at
    /// the discarded root/branch position; operands consumed by another operation remain value
    /// uses and therefore still diverge.
    pub(crate) fn expr_discarding_diverges_by(
        &self,
        expression: ExprId,
        leaf: &impl Fn(ExprId, &IrExpr) -> bool,
    ) -> bool {
        self.expr_diverges_by_use(expression, true, leaf)
    }

    fn expr_diverges_by_use(
        &self,
        expression: ExprId,
        discarded: bool,
        leaf: &impl Fn(ExprId, &IrExpr) -> bool,
    ) -> bool {
        match self.expr(expression) {
            IrExpr::Return(_)
            | IrExpr::Throw { .. }
            | IrExpr::Break { .. }
            | IrExpr::Continue { .. } => true,
            IrExpr::BottomValue { completion, .. } => {
                !discarded || completion.diverges_when_discarded()
            }
            IrExpr::Block { stmts, value } => {
                stmts
                    .last()
                    .is_some_and(|stmt| self.expr_diverges_by_use(*stmt, true, leaf))
                    || value.is_some_and(|value| self.expr_diverges_by_use(value, discarded, leaf))
            }
            IrExpr::TypeOp { arg, .. } => self.expr_diverges_by_use(*arg, false, leaf),
            IrExpr::SetValue { value, .. } | IrExpr::SetStatic { value, .. } => {
                self.expr_diverges_by_use(*value, false, leaf)
            }
            IrExpr::SetField {
                receiver, value, ..
            } => {
                self.expr_diverges_by_use(*receiver, false, leaf)
                    || self.expr_diverges_by_use(*value, false, leaf)
            }
            IrExpr::When { branches } => {
                branches.iter().any(|(condition, _)| condition.is_none())
                    && branches
                        .iter()
                        .all(|(_, body)| self.expr_diverges_by_use(*body, discarded, leaf))
            }
            IrExpr::Try {
                body,
                catches,
                finally,
                ..
            } => {
                finally.is_some_and(|finally| self.expr_diverges_by_use(finally, true, leaf))
                    || (self.expr_diverges_by_use(*body, discarded, leaf)
                        && catches
                            .iter()
                            .all(|catch| self.expr_diverges_by_use(catch.body, discarded, leaf)))
            }
            _ => leaf(expression, self.expr(expression)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bottom_completion_obeys_the_expression_use_context() {
        let mut ir = IrFile::with_package(None);
        let producer = ir.add_expr(IrExpr::UnitInstance);
        let fallthrough = ir.add_expr(IrExpr::BottomValue {
            producer,
            completion: IrBottomValueCompletion::FallThroughWhenDiscarded,
        });
        let diverging = ir.add_expr(IrExpr::BottomValue {
            producer,
            completion: IrBottomValueCompletion::Diverge,
        });
        let consumed = ir.add_expr(IrExpr::SetValue {
            var: 0,
            value: fallthrough,
        });

        let no_leaf_diverges = |_: ExprId, _: &IrExpr| false;
        assert!(!ir.expr_discarding_diverges_by(fallthrough, &no_leaf_diverges));
        assert!(ir.expr_diverges_by(fallthrough, &no_leaf_diverges));
        assert!(ir.expr_discarding_diverges_by(diverging, &no_leaf_diverges));
        assert!(ir.expr_discarding_diverges_by(consumed, &no_leaf_diverges));
    }
}
