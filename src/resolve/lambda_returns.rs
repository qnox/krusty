use std::collections::HashMap;

use crate::ast::ExprId;
use crate::types::Ty;

/// The declaration a checked return leaves. This is the frontend-owned control-flow identity
/// consumed by checked FIR and common lowering; later phases must not recover it from a label.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReturnTarget {
    Function,
    Lambda(ExprId),
}

/// All mutable return state for the body currently checked. Keeping labels, targets, contributed
/// result types, and expected result types together makes entering a nested body an explicit
/// ownership transfer instead of a collection of parallel `Checker` fields.
pub(super) struct LambdaReturnScopes {
    labels: Vec<(String, ExprId)>,
    function_label: Option<String>,
    returned_types: HashMap<ExprId, Ty>,
    expected_types: HashMap<ExprId, Ty>,
    bare_target: ReturnTarget,
    active_chain: Vec<ExprId>,
}

impl Default for LambdaReturnScopes {
    fn default() -> Self {
        Self {
            labels: Vec::new(),
            function_label: None,
            returned_types: HashMap::new(),
            expected_types: HashMap::new(),
            bare_target: ReturnTarget::Function,
            active_chain: Vec::new(),
        }
    }
}

pub(super) struct LambdaReturnFrame {
    label_depth: usize,
    chain_depth: usize,
    previous_expected: Option<Ty>,
}

impl LambdaReturnScopes {
    pub(super) fn target(&self, label: Option<&str>) -> Option<ReturnTarget> {
        match label {
            Some(label) => self
                .labels
                .iter()
                .rev()
                .find_map(|(candidate, lambda)| {
                    (candidate == label).then_some(ReturnTarget::Lambda(*lambda))
                })
                .or_else(|| {
                    (self.function_label.as_deref() == Some(label))
                        .then_some(ReturnTarget::Function)
                }),
            None => Some(self.bare_target),
        }
    }

    pub(super) fn active_labels(&self) -> &[(String, ExprId)] {
        &self.labels
    }

    pub(super) fn replace_function_label(&mut self, label: Option<String>) -> Option<String> {
        std::mem::replace(&mut self.function_label, label)
    }

    pub(super) fn restore_function_label(&mut self, label: Option<String>) {
        self.function_label = label;
    }

    pub(super) fn replace_bare_target(&mut self, target: ReturnTarget) -> ReturnTarget {
        std::mem::replace(&mut self.bare_target, target)
    }

    pub(super) fn enter_lambda(
        &mut self,
        lambda: ExprId,
        label: Option<String>,
        expected: Option<Ty>,
    ) -> LambdaReturnFrame {
        let frame = LambdaReturnFrame {
            label_depth: self.labels.len(),
            chain_depth: self.active_chain.len(),
            previous_expected: match expected {
                Some(expected) => self.expected_types.insert(lambda, expected),
                None => self.expected_types.remove(&lambda),
            },
        };
        if let Some(label) = label {
            self.labels.push((label, lambda));
        }
        self.active_chain.push(lambda);
        self.returned_types.remove(&lambda);
        frame
    }

    pub(super) fn leave_lambda(&mut self, lambda: ExprId, frame: LambdaReturnFrame) {
        match frame.previous_expected {
            Some(expected) => self.expected_types.insert(lambda, expected),
            None => self.expected_types.remove(&lambda),
        };
        self.labels.truncate(frame.label_depth);
        self.active_chain.truncate(frame.chain_depth);
    }

    pub(super) fn expected_type(&self, lambda: ExprId) -> Option<Ty> {
        self.expected_types.get(&lambda).copied()
    }

    pub(super) fn record_returned_type(&mut self, lambda: ExprId, returned: Ty) {
        let current = self.returned_types.get(&lambda).copied();
        let merged = crate::symbol_resolver::merge_inferred_ty(current, returned);
        self.returned_types.insert(lambda, merged);
    }

    pub(super) fn take_returned_type(&mut self, lambda: ExprId) -> Option<Ty> {
        self.returned_types.remove(&lambda)
    }

    pub(super) fn active_chain(&self) -> &[ExprId] {
        &self.active_chain
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_lambda_scope_restores_targets_and_expectations() {
        let outer = ExprId(1);
        let inner = ExprId(2);
        let mut scopes = LambdaReturnScopes::default();
        let outer_frame = scopes.enter_lambda(outer, Some("scope".into()), Some(Ty::String));
        assert_eq!(
            scopes.target(Some("scope")),
            Some(ReturnTarget::Lambda(outer))
        );
        assert_eq!(scopes.expected_type(outer), Some(Ty::String));

        let inner_frame = scopes.enter_lambda(inner, Some("scope".into()), Some(Ty::Int));
        assert_eq!(
            scopes.target(Some("scope")),
            Some(ReturnTarget::Lambda(inner))
        );
        scopes.leave_lambda(inner, inner_frame);

        assert_eq!(
            scopes.target(Some("scope")),
            Some(ReturnTarget::Lambda(outer))
        );
        assert_eq!(scopes.expected_type(inner), None);
        scopes.leave_lambda(outer, outer_frame);
        assert_eq!(scopes.target(Some("scope")), None);
        assert_eq!(scopes.expected_type(outer), None);
    }
}
