//! Smart casts that a jumping guard proves for the rest of its block.
//!
//! `if (v == null) throw …` runs the rest of its block only when the condition was false, so the
//! condition's negative facts hold there. Each fact narrows the binding it names, wherever that
//! binding lives, and the narrowing ends with the block that holds the guard.

use crate::ast::{Expr, File, Stmt, StmtId};
use crate::diag::Span;

use super::{
    CompactBindingIdentity, CompactSmartcastFact, ExpressionForm, OriginId, SigExpr, SigExprId,
    SignatureConstraintExtractor, SignatureScopeId,
};

/// A binding's value before a guard narrowed it, to restore when the block ends.
pub(super) type GuardNarrowing = (CompactBindingIdentity, Option<SigExprId>);

impl SignatureConstraintExtractor {
    /// Fold each fact's completion clauses into its narrowed value, so the value a later
    /// reference reads is the narrowing the clauses prove.
    pub(super) fn complete_smartcast_facts(&mut self, bindings: &mut [CompactSmartcastFact]) {
        for fact in bindings {
            if fact.completion_clauses.is_empty() {
                continue;
            }
            let Some(value) = self.compact_value_for_binding(&fact.binding) else {
                continue;
            };
            for clause in fact.completion_clauses.drain(..) {
                let completions = self.graph.add_operands(clause);
                fact.value = self.graph.add_expr(SigExpr::CompletionNarrowed {
                    value,
                    narrowed: fact.value,
                    completions,
                });
            }
        }
    }

    /// Narrow the bindings of `if (condition) <jump>` (no `else`) for the rest of the block, and
    /// return what to restore when the block ends.
    pub(super) fn narrow_after_jumping_guard(
        &mut self,
        file: &File,
        statement: StmtId,
        scope: SignatureScopeId,
        origin: &mut impl FnMut(Span) -> OriginId,
    ) -> Result<Vec<GuardNarrowing>, ExpressionForm> {
        let Stmt::Expr(expression) = file.stmt(statement) else {
            return Ok(Vec::new());
        };
        let Expr::If {
            cond,
            then_branch,
            else_branch: None,
        } = file.expr(*expression)
        else {
            return Ok(Vec::new());
        };
        if crate::ast::definitely_evaluated::expression_may_complete_normally(file, *then_branch) {
            return Ok(Vec::new());
        }
        let mut facts = self.negative_smartcasts(file, *cond, scope, origin)?;
        self.complete_smartcast_facts(&mut facts);
        let mut previous = Vec::with_capacity(facts.len());
        for fact in facts {
            let values = self
                .lexical_values
                .get_mut(fact.binding.scope)
                .expect("a compact smart-cast binding must remain in lexical scope");
            let old = values.insert(fact.binding.name.clone(), fact.value);
            previous.push((fact.binding, old));
        }
        Ok(previous)
    }

    /// Undo [`Self::narrow_after_jumping_guard`] as the block that held the guard ends.
    pub(super) fn restore_guard_narrowings(&mut self, narrowings: Vec<GuardNarrowing>) {
        for (binding, old) in narrowings.into_iter().rev() {
            let values = self
                .lexical_values
                .get_mut(binding.scope)
                .expect("a compact smart-cast binding must remain in lexical scope");
            match old {
                Some(value) => {
                    values.insert(binding.name, value);
                }
                None => {
                    values.remove(binding.name.as_ref());
                }
            }
        }
    }
}
