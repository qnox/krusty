//! Shared results of the one eager statement-local type-alias binding pass.
//!
//! Explicit-header publication and solver semantics are separate semantic consumers, but they
//! must observe the same declaration-site binding result.  In particular, a failed binding is a
//! stable result rather than permission to retry the target spelling in a later environment.

use super::*;
use std::rc::Rc;

#[derive(Clone, Debug)]
pub(super) struct LocalTypeSpellingOccurrence {
    pub(super) source: TypeRef,
    pub(super) source_file: crate::fir::SourceFileId,
    pub(super) binding: super::super::type_spellings::ResolvedTypeSpelling,
}

impl LocalTypeSpellingOccurrence {
    fn substituted(&self, bindings: &crate::symbol_resolver::GSigBinds) -> Self {
        let mut occurrence = self.clone();
        occurrence.binding.resolved = occurrence
            .binding
            .resolved
            .map(|ty| crate::symbol_resolver::ty_subst_keep_unbound(ty, bindings));
        occurrence
    }
}

/// Graph-owned spelling substitution. A formal is identified solely by the ordinal captured in
/// `LocalTypeTemplateTarget::Formal`; no semantic type name is inspected to rediscover it.
#[derive(Clone, Debug)]
pub(super) enum LocalTypeSpellingProjection {
    Formal {
        ordinal: u32,
        occurrence: LocalTypeSpellingOccurrence,
    },
    Structural {
        occurrence: LocalTypeSpellingOccurrence,
        children: Box<[LocalTypeSpellingProjection]>,
    },
    Overlay {
        occurrence: LocalTypeSpellingOccurrence,
        inner: Box<LocalTypeSpellingProjection>,
    },
}

#[derive(Clone, Debug)]
pub(super) struct AppliedLocalTypeSpellingProjection {
    pub(super) root: LocalTypeSpellingProjection,
    /// The names are graph-interned declaration formal identities. They are used only to apply the
    /// already-bound semantic template, never to identify a formal occurrence in the projection.
    pub(super) formals: Box<[String]>,
    pub(super) arguments: Box<[Ty]>,
}

impl LocalTypeSpellingProjection {
    pub(super) fn substitute(
        &self,
        arguments: &[LocalTypeSpellingProjection],
        bindings: &crate::symbol_resolver::GSigBinds,
    ) -> Option<Self> {
        match self {
            Self::Formal {
                ordinal,
                occurrence,
            } => Some(Self::Overlay {
                occurrence: occurrence.substituted(bindings),
                inner: Box::new(arguments.get(*ordinal as usize)?.clone()),
            }),
            Self::Structural {
                occurrence,
                children,
            } => Some(Self::Structural {
                occurrence: occurrence.substituted(bindings),
                children: children
                    .iter()
                    .map(|child| child.substitute(arguments, bindings))
                    .collect::<Option<Vec<_>>>()?
                    .into_boxed_slice(),
            }),
            Self::Overlay { occurrence, inner } => Some(Self::Overlay {
                occurrence: occurrence.substituted(bindings),
                inner: Box::new(inner.substitute(arguments, bindings)?),
            }),
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct BoundLocalAliasTemplate {
    pub(super) semantic: Ty,
    pub(super) spelling: LocalTypeSpellingProjection,
}

#[derive(Clone)]
enum BindingState {
    Unbound,
    Binding,
    Bound(Result<BoundLocalAliasTemplate, ()>),
}

pub(super) enum BindRequest {
    Bind,
    Complete(Result<BoundLocalAliasTemplate, ()>),
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
        match states.get(&alias).cloned() {
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
        result: Result<BoundLocalAliasTemplate, ()>,
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
    ) -> Option<Result<BoundLocalAliasTemplate, ()>> {
        match self.states.borrow().get(&alias).cloned() {
            Some(BindingState::Bound(result)) => Some(result),
            Some(BindingState::Unbound | BindingState::Binding) | None => None,
        }
    }
}
