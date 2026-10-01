//! The stacks of contextual receivers and builder-inference constraint frames that signature
//! inference keeps while it walks a contextual function literal's body.

use super::*;

impl ProductionSignatureSemantics<'_> {
    pub(in crate::resolve::streaming_signature_bridge) fn active_scoped_constraint_frame(
        &self,
        declaration: crate::fir::DeclarationId,
    ) -> Option<(crate::fir::DeclarationId, usize)> {
        let constraints = self.scoped_constraints.borrow();
        let inputs = self.scoped_constraint_inputs.borrow();
        let mut current = Some(declaration);
        while let Some(declaration) = current {
            if let (Some(constraint_stack), Some(input_stack)) =
                (constraints.get(&declaration), inputs.get(&declaration))
            {
                if let Some(index) =
                    input_stack
                        .iter()
                        .enumerate()
                        .rev()
                        .find_map(|(index, inputs)| {
                            (constraint_stack.get(index).is_some()
                                && inputs.iter().any(|input| input.mentions_ty_param()))
                            .then_some(index)
                        })
                {
                    return Some((declaration, index));
                }
            }
            current = self.declaration_semantic_parent(declaration);
        }
        None
    }

    pub(super) fn push_scoped_receiver(
        &self,
        declaration: crate::fir::DeclarationId,
        receiver: Ty,
    ) {
        self.scoped_receivers
            .borrow_mut()
            .entry(declaration)
            .or_default()
            .push(receiver);
    }

    pub(super) fn pop_scoped_receiver(&self, declaration: crate::fir::DeclarationId) {
        let mut receivers = self.scoped_receivers.borrow_mut();
        let remove = receivers.get_mut(&declaration).is_some_and(|stack| {
            stack.pop();
            stack.is_empty()
        });
        if remove {
            receivers.remove(&declaration);
        }
    }

    pub(super) fn push_scoped_constraint_frame(
        &self,
        declaration: crate::fir::DeclarationId,
        inputs: Vec<Ty>,
    ) {
        self.scoped_constraint_inputs
            .borrow_mut()
            .entry(declaration)
            .or_default()
            .push(inputs);
        self.scoped_constraints
            .borrow_mut()
            .entry(declaration)
            .or_default()
            .push(crate::symbol_resolver::GSigBinds::new());
    }

    pub(super) fn pop_scoped_constraint_frame(&self, declaration: crate::fir::DeclarationId) {
        let mut semantic_ancestors = Vec::new();
        let mut ancestor = self.declaration_semantic_parent(declaration);
        while let Some(current) = ancestor {
            semantic_ancestors.push(current);
            ancestor = self.declaration_semantic_parent(current);
        }
        let mut inputs = self.scoped_constraint_inputs.borrow_mut();
        let remove_inputs = inputs.get_mut(&declaration).is_some_and(|stack| {
            stack.pop();
            stack.is_empty()
        });
        if remove_inputs {
            inputs.remove(&declaration);
        }
        drop(inputs);

        let completed = {
            let mut all = self.scoped_constraints.borrow_mut();
            let (completed, remove) = all
                .get_mut(&declaration)
                .map(|stack| (stack.pop(), stack.is_empty()))
                .unwrap_or((None, false));
            if remove {
                all.remove(&declaration);
            }
            completed
        };
        let Some(completed) = completed else {
            return;
        };
        let mut all = self.scoped_constraints.borrow_mut();
        let active_parent = all
            .get(&declaration)
            .and_then(|stack| stack.last())
            .is_some()
            .then_some(declaration)
            .or_else(|| {
                semantic_ancestors
                    .into_iter()
                    .find(|ancestor| all.get(ancestor).and_then(|stack| stack.last()).is_some())
            });
        if let Some(parent) = active_parent
            .and_then(|parent| all.get_mut(&parent))
            .and_then(|stack| stack.last_mut())
        {
            Self::merge_scoped_constraints(parent, completed.clone());
            drop(all);
            let mut finished = self.completed_scoped_constraints.borrow_mut();
            let target = finished.entry(declaration).or_default();
            Self::merge_scoped_constraints(target, completed);
            return;
        }
        drop(all);
        let mut finished = self.completed_scoped_constraints.borrow_mut();
        let target = finished.entry(declaration).or_default();
        Self::merge_scoped_constraints(target, completed);
    }
}
