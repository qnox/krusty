//! Compact control-flow state across try/catch/finally expressions.

use crate::ast::{CatchClause, ExprId, File};
use crate::fx_hash::HashMap;

use super::{
    ExpressionForm, OriginId, SigExpr, SigExprId, SignatureConstraintExtractor, SignatureScopeId,
};

impl SignatureConstraintExtractor {
    pub(super) fn try_expression(
        &mut self,
        file: &File,
        body: ExprId,
        catches: &[CatchClause],
        finally: Option<ExprId>,
        scope: SignatureScopeId,
        origin: &mut impl FnMut(crate::diag::Span) -> OriginId,
        node_origin: OriginId,
    ) -> Result<SigExprId, ExpressionForm> {
        let mut results = Vec::with_capacity(catches.len() + 1);
        let branch_entry = self.boolean_vals.clone();
        self.try_body_writes.push(Default::default());
        let body_result = self.expression(file, body, scope, origin);
        let body_writes = self
            .try_body_writes
            .pop()
            .expect("the compact try-body write set is still open");
        let body_state = std::mem::replace(&mut self.boolean_vals, branch_entry.clone());
        let body_value = body_result?;
        results.push(body_value);
        let mut exits = Vec::with_capacity(catches.len() + 1);
        exits.push((self.compact_completion(file, body, body_value), body_state));
        let catch_entry = self.catch_entry_boolean_vals(&branch_entry, &body_writes);
        for catch in catches {
            self.boolean_vals = catch_entry.clone();
            let ty = self.compact_type(&catch.ty, scope, origin);
            self.lexical_values.push(HashMap::from_iter([(
                catch.name.clone().into_boxed_str(),
                ty,
            )]));
            let result = self.expression(file, catch.body, scope, origin);
            self.lexical_values.pop();
            let catch_state = std::mem::replace(&mut self.boolean_vals, branch_entry.clone());
            let catch_value = result?;
            results.push(catch_value);
            exits.push((
                self.compact_completion(file, catch.body, catch_value),
                catch_state,
            ));
        }
        self.join_boolean_vals(&exits);
        let normal_continuation = self.boolean_vals.clone();
        // A finalizer runs on abrupt exits too, so check it only with facts common to every path.
        // The continuation after the try still joins normal exits alone; only the finalizer's
        // writes are projected onto that state because abrupt paths never reach the continuation.
        if let Some(finally) = finally {
            self.boolean_vals = branch_entry;
            let all_exits = exits
                .iter()
                .map(|(_, state)| (super::CompactCompletion::Always, state.clone()))
                .collect::<Vec<_>>();
            self.join_boolean_vals(&all_exits);
            self.try_body_writes.push(Default::default());
            let _ = self.expression(file, finally, scope, origin)?;
            let finally_writes = self
                .try_body_writes
                .pop()
                .expect("the compact finalizer write set is still open");
            self.boolean_vals =
                self.catch_entry_boolean_vals(&normal_continuation, &finally_writes);
        }
        let operands = self.graph.add_operands(results);
        Ok(self.graph.add_expr(SigExpr::Join {
            operands,
            scope,
            origin: node_origin,
        }))
    }
}
