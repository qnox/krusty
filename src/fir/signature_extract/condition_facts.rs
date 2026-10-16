//! Compact smart-cast facts a condition establishes at either Boolean outcome.
//!
//! Semantic compatibility is still decided by the ordinary resolver/checker adapter when these
//! expression types evaluate; this module only names which lexical values an outcome narrows, and
//! to what.

use crate::ast::{BinOp, Expr, ExprId, File, UnOp};
use crate::fx_hash::{HashMap, HashSet};

use super::{
    ExpressionForm, OriginId, SigExpr, SigExprId, SignatureConstraintExtractor, SignatureScopeId,
    Ty,
};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct CompactBindingIdentity {
    pub(super) scope: usize,
    pub(super) name: Box<str>,
}

#[derive(Clone, Eq, PartialEq)]
pub(super) struct CompactSmartcastFact {
    pub(super) binding: CompactBindingIdentity,
    pub(super) value: SigExprId,
    /// Conjunctive clauses that must each contain a result resolving to `Nothing` for this fact to
    /// reach the continuation. Deferred call selection decides those results when the graph runs.
    pub(super) completion_clauses: Vec<Vec<SigExprId>>,
}

#[derive(Clone, Eq, PartialEq)]
pub(super) struct CompactBooleanValue {
    pub(super) when_true: Vec<CompactSmartcastFact>,
    pub(super) when_false: Vec<CompactSmartcastFact>,
}

pub(super) enum CompactCompletion {
    Never,
    Always,
    Deferred(SigExprId),
}

impl SignatureConstraintExtractor {
    pub(super) fn compact_completion(
        &self,
        file: &File,
        expression: ExprId,
        result: SigExprId,
    ) -> CompactCompletion {
        if !crate::ast::definitely_evaluated::expression_may_complete_normally(file, expression) {
            return CompactCompletion::Never;
        }
        match self.graph.expr(result) {
            Some(SigExpr::Known(ty)) if ty.get() == Ty::Nothing => CompactCompletion::Never,
            Some(SigExpr::Known(_)) => CompactCompletion::Always,
            Some(_) => CompactCompletion::Deferred(result),
            None => unreachable!("a compact branch result must belong to its signature graph"),
        }
    }

    pub(super) fn join_boolean_vals(
        &mut self,
        exits: &[(
            CompactCompletion,
            HashMap<CompactBindingIdentity, CompactBooleanValue>,
        )],
    ) {
        let has_reachable_exit = exits
            .iter()
            .any(|(completion, _)| !matches!(completion, CompactCompletion::Never));
        if !has_reachable_exit {
            self.boolean_vals.clear();
            return;
        }
        for (completion, state) in exits {
            if matches!(completion, CompactCompletion::Never) {
                continue;
            }
            self.boolean_vals.retain(|binding, value| {
                let candidate = state.get(binding);
                for (facts, candidate_facts) in [
                    (
                        &mut value.when_true,
                        candidate.map(|value| &value.when_true),
                    ),
                    (
                        &mut value.when_false,
                        candidate.map(|value| &value.when_false),
                    ),
                ] {
                    facts.retain_mut(|fact| {
                        if let Some(candidate) = candidate_facts.and_then(|facts| {
                            facts.iter().find(|candidate| {
                                candidate.binding == fact.binding && candidate.value == fact.value
                            })
                        }) {
                            for candidate_clause in &candidate.completion_clauses {
                                let mut clause = candidate_clause.clone();
                                if let CompactCompletion::Deferred(completion) = completion {
                                    if !clause.contains(completion) {
                                        clause.push(*completion);
                                    }
                                }
                                if !fact.completion_clauses.contains(&clause) {
                                    fact.completion_clauses.push(clause);
                                }
                            }
                            true
                        } else if let CompactCompletion::Deferred(completion) = completion {
                            let clause = vec![*completion];
                            if !fact.completion_clauses.contains(&clause) {
                                fact.completion_clauses.push(clause);
                            }
                            true
                        } else {
                            false
                        }
                    });
                }
                !value.when_true.is_empty() || !value.when_false.is_empty()
            });
        }
    }

    /// A catch can start after any write in the try body, so it receives the entry facts with only
    /// facts rooted at those exact written bindings removed.
    pub(super) fn catch_entry_boolean_vals(
        &mut self,
        entry: &HashMap<CompactBindingIdentity, CompactBooleanValue>,
        writes: &[CompactBindingIdentity],
    ) -> HashMap<CompactBindingIdentity, CompactBooleanValue> {
        let mut result = entry.clone();
        let replacements = writes.iter().collect::<HashSet<_>>();
        result.retain(|binding, value| {
            value
                .when_true
                .retain(|fact| !replacements.contains(&fact.binding));
            value
                .when_false
                .retain(|fact| !replacements.contains(&fact.binding));
            !replacements.contains(binding)
                && (!value.when_true.is_empty() || !value.when_false.is_empty())
        });
        result
    }

    /// Compact smart-cast facts established when `condition` has `truth`. Semantic compatibility
    /// is still decided by the ordinary resolver/checker adapter when these expression types
    /// evaluate.
    pub(super) fn smartcast_facts(
        &mut self,
        file: &File,
        condition: ExprId,
        truth: bool,
        scope: SignatureScopeId,
        origin: &mut impl FnMut(crate::diag::Span) -> OriginId,
    ) -> Result<Vec<CompactSmartcastFact>, ExpressionForm> {
        match file.expr(condition) {
            // A local `val` read as a condition proves what its initializer proves.
            Expr::Name(name) => match self.boolean_val_facts(condition, name, truth) {
                Some(facts) => Ok(facts),
                None => Ok(Vec::new()),
            },
            Expr::Unary {
                op: UnOp::Not,
                operand,
            } => self.smartcast_facts(file, *operand, !truth, scope, origin),
            Expr::Is {
                operand,
                ty,
                negated,
            } if truth != *negated => match file.expr(*operand) {
                Expr::Name(name) => {
                    let Some(binding) = self.binding_for_reference(*operand, name) else {
                        return Ok(Vec::new());
                    };
                    Ok(vec![CompactSmartcastFact {
                        binding,
                        value: self.smartcast_type(file, *operand, ty, scope, origin)?,
                        completion_clauses: Vec::new(),
                    }])
                }
                _ => Ok(Vec::new()),
            },
            Expr::Binary {
                op: BinOp::And,
                lhs,
                rhs,
                ..
            } if truth => {
                let mut bindings = self.positive_smartcasts(file, *lhs, scope, origin)?;
                bindings.extend(self.positive_smartcasts(file, *rhs, scope, origin)?);
                Ok(bindings)
            }
            Expr::Binary {
                op: BinOp::Or,
                lhs,
                rhs,
                ..
            } if !truth => {
                let mut bindings = self.negative_smartcasts(file, *lhs, scope, origin)?;
                bindings.extend(self.negative_smartcasts(file, *rhs, scope, origin)?);
                Ok(bindings)
            }
            // Equality with a Boolean literal projects an exact outcome onto the other operand.
            // A safe call remains owned by the next arm because a successful comparison first
            // proves its receiver non-null; the safe-call result is not itself a saved Boolean val.
            Expr::Binary { op, lhs, rhs, .. }
                if matches!(op, BinOp::Eq | BinOp::Ne)
                    && matches!(
                        (file.expr(*lhs), file.expr(*rhs)),
                        (_, Expr::BoolLit(_)) | (Expr::BoolLit(_), _)
                    )
                    && !matches!(
                        (file.expr(*lhs), file.expr(*rhs)),
                        (Expr::SafeCall { .. }, _) | (_, Expr::SafeCall { .. })
                    ) =>
            {
                let projected = match (file.expr(*lhs), file.expr(*rhs)) {
                    (_, Expr::BoolLit(literal)) => Some((*lhs, *literal)),
                    (Expr::BoolLit(literal), _) => Some((*rhs, *literal)),
                    _ => None,
                }
                .expect("guard requires one Boolean literal");
                let projected_truth = if (matches!(op, BinOp::Eq) == truth) {
                    projected.1
                } else {
                    !projected.1
                };
                self.smartcast_facts(file, projected.0, projected_truth, scope, origin)
            }
            // `x?.p == literal` and `x?.p != null` hold only when `x` is non-null.
            Expr::Binary { op, lhs, rhs, .. }
                if matches!(op, BinOp::Eq | BinOp::RefEq | BinOp::Ne | BinOp::RefNe)
                    && matches!(
                        (file.expr(*lhs), file.expr(*rhs)),
                        (Expr::SafeCall { .. }, _) | (_, Expr::SafeCall { .. })
                    ) =>
            {
                let (receiver, other) = match (file.expr(*lhs), file.expr(*rhs)) {
                    (Expr::SafeCall { receiver, .. }, other) => (*receiver, other),
                    (other, Expr::SafeCall { receiver, .. }) => (*receiver, other),
                    _ => return Ok(Vec::new()),
                };
                let effective_op = if truth {
                    *op
                } else {
                    match op {
                        BinOp::Eq => BinOp::Ne,
                        BinOp::RefEq => BinOp::RefNe,
                        BinOp::Ne => BinOp::Eq,
                        BinOp::RefNe => BinOp::RefEq,
                        _ => unreachable!("guard restricts equality operators"),
                    }
                };
                let proves = match effective_op {
                    BinOp::Eq | BinOp::RefEq => matches!(
                        other,
                        Expr::BoolLit(_)
                            | Expr::IntLit(_)
                            | Expr::LongLit(_)
                            | Expr::StringLit(_)
                            | Expr::CharLit(_)
                    ),
                    _ => matches!(other, Expr::NullLit),
                };
                let Expr::Name(name) = file.expr(receiver) else {
                    return Ok(Vec::new());
                };
                Ok(proves
                    .then(|| {
                        let binding = self.binding_for_reference(receiver, name)?;
                        let value = self.compact_value_for_binding(&binding)?;
                        let non_null = self.graph.add_expr(SigExpr::NonNullable(value));
                        Some(CompactSmartcastFact {
                            binding,
                            value: non_null,
                            completion_clauses: Vec::new(),
                        })
                    })
                    .flatten()
                    .into_iter()
                    .collect())
            }
            Expr::Binary { op, lhs, rhs, .. }
                if matches!(op, BinOp::Eq | BinOp::RefEq | BinOp::Ne | BinOp::RefNe)
                    && (matches!(op, BinOp::Ne | BinOp::RefNe) == truth) =>
            {
                self.non_null_equality_fact(file, *lhs, *rhs)
            }
            _ => Ok(Vec::new()),
        }
    }

    fn non_null_equality_fact(
        &mut self,
        file: &File,
        lhs: ExprId,
        rhs: ExprId,
    ) -> Result<Vec<CompactSmartcastFact>, ExpressionForm> {
        let reference = match (file.expr(lhs), file.expr(rhs)) {
            (Expr::Name(name), Expr::NullLit) => Some((lhs, name)),
            (Expr::NullLit, Expr::Name(name)) => Some((rhs, name)),
            _ => None,
        };
        Ok(reference
            .and_then(|(reference, name)| {
                let binding = self.binding_for_reference(reference, name)?;
                let value = self.compact_value_for_binding(&binding)?;
                let non_null = self.graph.add_expr(SigExpr::NonNullable(value));
                Some(CompactSmartcastFact {
                    binding,
                    value: non_null,
                    completion_clauses: Vec::new(),
                })
            })
            .into_iter()
            .collect())
    }

    /// The facts held by the local `val` that `name` currently reads.
    fn boolean_val_facts(
        &self,
        reference: ExprId,
        name: &str,
        truth: bool,
    ) -> Option<Vec<CompactSmartcastFact>> {
        let binding = self.binding_for_reference(reference, name)?;
        self.boolean_vals.get(&binding).map(|value| {
            if truth {
                value.when_true.clone()
            } else {
                value.when_false.clone()
            }
        })
    }

    fn lexical_binding_identity(&self, name: &str) -> Option<CompactBindingIdentity> {
        self.lexical_values
            .iter()
            .enumerate()
            .rev()
            .find_map(|(scope, values)| {
                values.contains_key(name).then(|| CompactBindingIdentity {
                    scope,
                    name: name.into(),
                })
            })
    }

    pub(super) fn binding_for_reference(
        &self,
        reference: ExprId,
        name: &str,
    ) -> Option<CompactBindingIdentity> {
        self.resolved_name_bindings
            .get(&reference)
            .cloned()
            .or_else(|| self.lexical_binding_identity(name))
    }

    pub(super) fn compact_value_for_binding(
        &self,
        binding: &CompactBindingIdentity,
    ) -> Option<SigExprId> {
        self.lexical_values
            .get(binding.scope)?
            .get(binding.name.as_ref())
            .copied()
    }

    /// Record one declaration's implication under its lexical binding, never under the compact
    /// value node that aliases may share. Dependencies are exactly the bindings narrowed by either
    /// outcome, not every value read while evaluating the condition: rewriting an unrelated guard
    /// operand cannot invalidate a fact about another binding.
    pub(super) fn record_boolean_val(
        &mut self,
        file: &File,
        name: &str,
        condition: ExprId,
        signature_scope: SignatureScopeId,
        origin: &mut impl FnMut(crate::diag::Span) -> OriginId,
    ) -> Result<(), ExpressionForm> {
        let binding_scope = self
            .lexical_values
            .len()
            .checked_sub(1)
            .expect("a local Boolean val must have a lexical scope");
        let when_true = self.positive_smartcasts(file, condition, signature_scope, origin)?;
        let when_false = self.negative_smartcasts(file, condition, signature_scope, origin)?;
        self.boolean_vals.insert(
            CompactBindingIdentity {
                scope: binding_scope,
                name: name.into(),
            },
            CompactBooleanValue {
                when_true,
                when_false,
            },
        );
        Ok(())
    }

    pub(super) fn forget_boolean_vals_depending_on(&mut self, name: &str) {
        if self.unreachable_statement_depth != 0 {
            return;
        }
        let Some(written) = self.lexical_binding_identity(name) else {
            return;
        };
        for writes in &mut self.try_body_writes {
            writes.push(written.clone());
        }
        self.boolean_vals.retain(|binding, value| {
            value.when_true.retain(|fact| fact.binding != written);
            value.when_false.retain(|fact| fact.binding != written);
            binding != &written && (!value.when_true.is_empty() || !value.when_false.is_empty())
        });
    }

    pub(super) fn leave_boolean_val_scope(&mut self) {
        let Some(scope) = self.lexical_values.len().checked_sub(1) else {
            return;
        };
        self.boolean_vals
            .retain(|binding, _| binding.scope != scope);
    }
}
