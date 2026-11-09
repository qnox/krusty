//! What candidate selection publishes from a call solved over its own inference variables.
//!
//! Selection collects and solves a call's constraints against the call's variables
//! ([`crate::symbol_resolver::CallSiteVariables`]); everything it hands on (the specialized
//! callable, its bindings and the parameter shapes overload ranking compares) is expressed over the
//! declaration's formals again, exactly as a declaration-owned signature would have produced. The
//! selected parameters also stay available over the call's own variables, so selected-argument
//! checking can tell a variable the selection left open from a declared formal in scope.

use super::*;

/// Bridge from the argument expectations call resolution still builds over a candidate's declared
/// formals to the variables its selection solves.
///
/// Before selection, an argument (a callable reference, a lambda shaped by the candidate) is
/// checked against the candidate's declared parameter types, so its type can name the candidate's
/// own formal (`(T) -> Int` for `String::length` against `project: (T) -> Int`). A formal that is
/// not lexically in scope at the call cannot reach the argument any other way: it is this call's
/// open variable and becomes the call's variable for it. A formal lexically in scope at the call (a
/// recursive `f(x)` inside `fun <T> f`) names the enclosing declaration's fixed type and stays.
/// This shrinks away once argument expectations are built over the call's variables directly.
pub(super) struct ShapedArguments {
    variables: crate::symbol_resolver::CallSiteVariableMap,
}

impl ShapedArguments {
    pub(super) fn new(
        declared: &crate::libraries::GenericSig,
        site: crate::types::CallSiteIdentity,
        lexical: &[String],
    ) -> Self {
        let open = declared
            .formals
            .iter()
            .filter(|formal| !lexical.iter().any(|identity| identity == formal.as_str()))
            .copied()
            .collect::<Vec<_>>();
        Self {
            variables: crate::symbol_resolver::CallSiteVariableMap::for_call(&open, site),
        }
    }

    pub(super) fn types(&self, types: &[Ty]) -> Vec<Ty> {
        types
            .iter()
            .map(|&ty| self.variables.instantiate_type(ty))
            .collect()
    }

    pub(super) fn kinds(
        &self,
        kinds: Vec<crate::symbol_resolver::CallArgKind>,
    ) -> Vec<crate::symbol_resolver::CallArgKind> {
        kinds
            .into_iter()
            .map(|kind| kind.map_types(|ty| self.variables.instantiate_type(ty)))
            .collect()
    }

    pub(super) fn partial(&self, partial: &[Option<Ty>]) -> Vec<Option<Ty>> {
        partial
            .iter()
            .map(|ty| ty.map(|ty| self.variables.instantiate_type(ty)))
            .collect()
    }
}

/// The parameter shapes overload ranking compares for one applicable candidate.
pub(super) struct CandidateParameterShapes {
    pub(super) selection: Vec<Ty>,
    pub(super) declaration_specificity: Vec<Ty>,
}

/// `candidate` specialized by `bindings` over `variables`, expressed over its declared formals.
pub(super) fn declared_selection(
    variables: &crate::symbol_resolver::CallSiteVariableMap,
    mut candidate: crate::libraries::FunctionInfo,
    bindings: crate::symbol_resolver::GSigBinds,
    intersection_bindings: HashMap<String, Vec<Ty>>,
    candidate_index: usize,
) -> SelectedCallable {
    let declared = |ty: Ty| variables.declared_type(ty);
    let call_site_params = candidate.callable.params.clone();
    candidate.receiver = candidate.receiver.map(declared);
    candidate.callable.params = candidate
        .callable
        .params
        .iter()
        .copied()
        .map(declared)
        .collect();
    candidate.callable.ret = declared(candidate.callable.ret);
    candidate.callable.vararg_elem = candidate.callable.vararg_elem.map(declared);
    SelectedCallable {
        info: candidate,
        call_site_params,
        bindings: variables.declared_bindings(bindings),
        intersection_bindings: intersection_bindings
            .into_iter()
            .map(|(formal, bounds)| {
                let bounds = bounds.into_iter().map(declared).collect();
                (variables.declared_formal(&formal).to_string(), bounds)
            })
            .collect(),
        candidate_index: Some(candidate_index),
    }
}

impl CandidateParameterShapes {
    pub(super) fn declared(self, variables: &crate::symbol_resolver::CallSiteVariableMap) -> Self {
        let declared = |shapes: Vec<Ty>| {
            shapes
                .into_iter()
                .map(|ty| variables.declared_type(ty))
                .collect()
        };
        Self {
            selection: declared(self.selection),
            declaration_specificity: declared(self.declaration_specificity),
        }
    }
}

impl SelectedCallable {
    /// [`SelectedCallable::call_site_params`] in the layout of its applied parameters.
    pub(super) fn applied_call_site_params(&self) -> std::borrow::Cow<'_, [Ty]> {
        self.info.applied_layout(&self.call_site_params)
    }
}

impl ContextualCallShape {
    /// `call_site`, laid out like the parameters this shape was built from, in this shape's layout;
    /// `None` when it does not cover them.
    pub(super) fn call_site_params(&self, call_site: &[Ty]) -> Option<Vec<Ty>> {
        self.parameter_indices
            .iter()
            .map(|&parameter| call_site.get(parameter).copied())
            .collect()
    }
}
