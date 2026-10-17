//! Shared results of the one eager statement-local type-alias binding pass.
//!
//! Explicit-header publication and solver semantics are separate semantic consumers, but they
//! must observe the same declaration-site binding result.  In particular, a failed binding is a
//! stable result rather than permission to retry the target spelling in a later environment.

use super::*;
use std::rc::Rc;

#[derive(Clone, Copy)]
enum BindingState {
    Unbound,
    Binding,
    Bound(Result<Ty, ()>),
}

pub(super) enum BindRequest {
    Bind,
    Complete(Result<Ty, ()>),
}

#[derive(Clone)]
pub(super) struct BoundLocalAliasTemplates {
    states: Rc<RefCell<HashMap<crate::fir::LocalTypeAliasTemplateId, BindingState>>>,
}

impl BoundLocalAliasTemplates {
    pub(super) fn for_graph(graph: &crate::fir::SignatureGraph) -> Self {
        Self {
            states: Rc::new(RefCell::new(
                graph
                    .local_type_alias_template_ids()
                    .map(|alias| (alias, BindingState::Unbound))
                    .collect(),
            )),
        }
    }

    pub(super) fn begin(&self, alias: crate::fir::LocalTypeAliasTemplateId) -> BindRequest {
        let mut states = self.states.borrow_mut();
        match states.get(&alias).copied() {
            Some(BindingState::Unbound) => {
                states.insert(alias, BindingState::Binding);
                BindRequest::Bind
            }
            Some(BindingState::Bound(result)) => BindRequest::Complete(result),
            Some(BindingState::Binding) | None => BindRequest::Complete(Err(())),
        }
    }

    pub(super) fn finish(
        &self,
        alias: crate::fir::LocalTypeAliasTemplateId,
        result: Result<Ty, ()>,
    ) {
        let previous = self
            .states
            .borrow_mut()
            .insert(alias, BindingState::Bound(result));
        debug_assert!(matches!(previous, Some(BindingState::Binding)));
    }

    pub(super) fn bound(
        &self,
        alias: crate::fir::LocalTypeAliasTemplateId,
    ) -> Option<Result<Ty, ()>> {
        match self.states.borrow().get(&alias).copied() {
            Some(BindingState::Bound(result)) => Some(result),
            Some(BindingState::Unbound | BindingState::Binding) | None => None,
        }
    }
}
