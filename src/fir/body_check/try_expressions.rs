//! Checked-FIR publication for `try` expressions: each catch clause binds its parameter in a scope
//! of its own, and keeps its `catch` keyword's line for the handler's entry.

use std::collections::HashMap;

use crate::ast::{CatchClause, ExprId};
use crate::fir::FirCatch;

use super::{BodyCheckFailure, BodyCheckFailureKind, BodyFirChecker, FirExprKind};

impl BodyFirChecker<'_> {
    pub(super) fn try_expression(
        &mut self,
        body: ExprId,
        catches: &[CatchClause],
        finally: Option<ExprId>,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let body = self.expression(body)?;
        let mut checked_catches = Vec::with_capacity(catches.len());
        for catch in catches {
            let parameter_ty = self.info.resolved_type(&catch.ty).ok_or_else(|| {
                self.failure(
                    Some(catch.ty.span),
                    BodyCheckFailureKind::UnresolvedTypeSyntax,
                )
            })?;
            self.scopes.push(HashMap::new());
            self.delegate_scopes.push(HashMap::new());
            let parameter_ty = self.resolved_type(catch.ty.span, parameter_ty)?;
            let parameter = self.bind_local(&catch.name, parameter_ty);
            let checked_body = self.expression(catch.body);
            self.delegate_scopes.pop();
            self.scopes.pop();
            checked_catches.push(FirCatch {
                origin: self.origins.source(self.source, catch.param_span),
                parameter,
                parameter_ty,
                body: checked_body?,
                debug_line: catch.line,
            });
        }
        Ok(FirExprKind::Try {
            body,
            catches: checked_catches.into_boxed_slice(),
            finally: finally
                .map(|finally| self.expression(finally))
                .transpose()?,
        })
    }
}
