//! Signature effects of an unparenthesized safe index.
//!
//! The member `get` is selected on the receiver and the whole selector is nullable. Assignment
//! evaluates the receiver, each index, and the stored value.

use super::{
    ExpressionForm, OriginId, SigExpr, SigExprId, SignatureConstraintExtractor, SignatureScopeId,
};
use crate::ast::{Expr, ExprId, File};

impl SignatureConstraintExtractor {
    pub(super) fn safe_index_read(
        &mut self,
        file: &File,
        expression: ExprId,
        scope: SignatureScopeId,
        origin: &mut impl FnMut(crate::diag::Span) -> OriginId,
        node_origin: OriginId,
    ) -> Result<SigExprId, ExpressionForm> {
        let (receiver, access, indices) = match file.expr(expression) {
            Expr::SafeIndex {
                receiver,
                access,
                indices,
                ..
            }
            | Expr::SafeIndexIncDec {
                receiver,
                access,
                indices,
                ..
            } => (*receiver, *access, indices.clone()),
            _ => unreachable!("a safe index selector is a safe index"),
        };
        let receiver = self.expression(file, receiver, scope, origin)?;
        let Expr::Member { name, .. } = file.expr(access) else {
            unreachable!("a safe index selector is a member access");
        };
        let name = name.clone();
        let member = self.member(receiver, &name, scope, node_origin);
        let mut arguments = Vec::with_capacity(indices.len());
        for index in &indices {
            arguments.push(self.expression(file, *index, scope, origin)?);
        }
        let indexed = self.member_call(member, "get", arguments, scope, node_origin);
        Ok(self.graph.add_expr(SigExpr::Nullable(indexed)))
    }

    pub(super) fn safe_index_assignment_effects(
        &mut self,
        file: &File,
        statement: crate::ast::StmtId,
        scope: SignatureScopeId,
        origin: &mut impl FnMut(crate::diag::Span) -> OriginId,
    ) -> Result<Vec<SigExprId>, ExpressionForm> {
        let crate::ast::Stmt::AssignSafeIndex {
            receiver,
            indices,
            value,
            ..
        } = file.stmt(statement).clone()
        else {
            unreachable!("a safe index store is an assignment to a safe index");
        };
        let mut effects = Vec::with_capacity(indices.len() + 2);
        effects.push(self.expression(file, receiver, scope, origin)?);
        for index in indices {
            effects.push(self.expression(file, index, scope, origin)?);
        }
        effects.push(self.expression(file, value, scope, origin)?);
        Ok(effects)
    }
}
