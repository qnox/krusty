//! Whether one generic declaration accepts everything another's parameters stand for.
//!
//! Kotlin compares two applicable overloads by passing the parameters of one (the specific side,
//! its type parameters rigid) to the other (the general side) as if they were call arguments: the
//! general declaration's type parameters become fresh variables, the argument constraints are
//! solved with the same variance rules as any call, and the solution must satisfy the general
//! declaration's bounds. This is that call-site operation, not a second solver.

use super::call_constraints::infer_generic_call_constraints_with_receiver_from_symbols;
use super::{generic_bindings_satisfy_bounds, CallSiteVariables, GSigBinds, GenericSig};
use crate::symbol_source::SymbolSource;
use crate::types::ty_subst_keep_unbound_closed;
use crate::types::CallSiteIdentity;

/// The call site the comparison's variables belong to. No source call owns them, and a comparison
/// never publishes a solution.
const COMPARISON_SITE: CallSiteIdentity = CallSiteIdentity {
    compilation: u64::MAX,
    file: u32::MAX,
    call: u32::MAX,
};

/// Whether a call whose arguments are `specific`'s receiver and parameters is accepted by
/// `general`: the constraint system over `general`'s own variables has no contradiction and its
/// solution satisfies `general`'s declared bounds.
pub(crate) fn generic_declaration_accepts_parameters_of(
    source: &dyn SymbolSource,
    general: &GenericSig,
    specific: &GenericSig,
) -> bool {
    if general.params.len() != specific.params.len()
        || general.receiver.is_some() != specific.receiver.is_some()
    {
        return false;
    }
    let call = CallSiteVariables::instantiate(general, COMPARISON_SITE);
    let inferred = infer_generic_call_constraints_with_receiver_from_symbols(
        source,
        call.signature(),
        specific.receiver,
        specific
            .params
            .iter()
            .enumerate()
            .map(|(parameter, actual)| (parameter, *actual, false)),
        None,
    );
    let admits =
        |actual, expected| crate::symbol_resolver::resolution_subtype(source, actual, expected);
    // Only the general declaration's own variables are solved. An enclosing classifier's parameter
    // or one of the specific side's rigid parameters stays itself, so a binding the collector made
    // for it never stands in for the rigid type when the inputs are checked.
    let substituted = call.signature();
    let solved: GSigBinds = inferred
        .bindings
        .iter()
        .filter(|(name, _)| substituted.formals.iter().any(|formal| formal == *name))
        .map(|(name, ty)| (*name, *ty))
        .collect();
    inferred.bound_violations.is_empty()
        && generic_bindings_satisfy_bounds(substituted, &solved, admits)
        && substituted
            .receiver
            .zip(specific.receiver)
            .is_none_or(|(formal, actual)| {
                admits(actual, ty_subst_keep_unbound_closed(formal, &solved))
            })
        && substituted
            .params
            .iter()
            .zip(&specific.params)
            .all(|(formal, actual)| admits(*actual, ty_subst_keep_unbound_closed(*formal, &solved)))
}
