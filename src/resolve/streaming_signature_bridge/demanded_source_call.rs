//! Specialize a demanded source callable with the same constraint system as a checked call.
//!
//! Equality unification treats `In<Nothing?>` and `In<Left>` as `E = Left`. Those arguments are
//! upper bounds: their solution is `Nothing? & Left`, which is `Nothing`.

use crate::libraries::GenericSig;
use crate::symbol_resolver::{CallArgKind, GSigBinds};
use crate::symbol_source::SymbolSource;
use crate::types::Ty;

use super::super::Signature;

pub(super) fn demanded_source_result(
    source: &dyn SymbolSource,
    callable: &Signature,
    signature: &crate::fir::ResolvedSignature,
    receiver: Option<Ty>,
    arguments: &[Ty],
    argument_kinds: Option<&[CallArgKind]>,
    explicit_type_arguments: &[Ty],
    expected: Option<Ty>,
) -> Ty {
    let mut bindings = GSigBinds::new();
    if let Some(generic) = callable.generic_sig.as_ref() {
        bindings.extend(
            generic
                .formals
                .iter()
                .cloned()
                .zip(explicit_type_arguments.iter().copied()),
        );
    }
    let expected_bindings =
        callable
            .generic_sig
            .as_ref()
            .zip(expected)
            .and_then(|(generic, expected)| {
                crate::symbol_resolver::infer_generic_return_bindings_from_symbols(
                    source,
                    generic,
                    expected,
                    |actual, bound| admits(source, actual, bound),
                )
            });
    if let (Some(generic), Some(expected_bindings)) =
        (callable.generic_sig.as_ref(), expected_bindings.clone())
    {
        crate::symbol_resolver::merge_generic_upper_bindings(
            generic,
            explicit_type_arguments,
            &mut bindings,
            expected_bindings,
            |actual, bound| admits(source, actual, bound),
        );
    }
    if let (Some(declared), Some(actual)) = (callable.source_receiver, receiver) {
        crate::symbol_resolver::unify_inferred_ty_with_source(
            source,
            declared,
            actual,
            &mut bindings,
        );
    }
    if let Some(generic) = callable.generic_sig.as_ref() {
        let inferred =
            crate::symbol_resolver::infer_generic_call_constraints_with_receiver_from_symbols(
                source,
                generic,
                None,
                argument_constraints(source, callable, generic, arguments, argument_kinds),
                callable.vararg_index,
            );
        let denotable = inferred.denotable_upper_bindings(source);
        crate::symbol_resolver::merge_call_argument_bindings(
            source,
            generic,
            explicit_type_arguments,
            &GSigBinds::new(),
            &mut bindings,
            inferred.bindings,
        );
        crate::symbol_resolver::publish_denotable_upper_bindings(
            &mut bindings,
            denotable,
            expected_bindings.as_ref(),
            |sub, sup| admits(source, sub, sup),
        );
        crate::symbol_resolver::complete_bottom_constraint_bindings(
            generic,
            &mut bindings,
            explicit_type_arguments,
        );
    }
    crate::symbol_resolver::ty_subst_keep_unbound(signature.result.get(), &bindings)
}

fn admits(source: &dyn SymbolSource, actual: Ty, bound: Ty) -> bool {
    let oracle = crate::symbol_resolver::SourceOracle(source);
    crate::assignable::is_assignable(&crate::assignable::TyCtx::new(), &oracle, actual, bound)
}

fn argument_constraints(
    source: &dyn SymbolSource,
    callable: &Signature,
    generic: &GenericSig,
    arguments: &[Ty],
    argument_kinds: Option<&[CallArgKind]>,
) -> Vec<(usize, Ty, bool)> {
    let context_count = callable.context_count.min(generic.params.len());
    let visible_vararg = callable
        .vararg_index
        .and_then(|index| index.checked_sub(context_count));
    let mut actuals = Vec::new();
    for (argument_index, argument) in arguments.iter().enumerate() {
        if *argument == Ty::Error {
            continue;
        }
        let visible_parameter = visible_vararg
            .filter(|vararg| argument_index >= *vararg)
            .unwrap_or(argument_index);
        let parameter_index = context_count + visible_parameter;
        let Some(declared) = generic.params.get(parameter_index).copied() else {
            continue;
        };
        let kind = argument_kinds.and_then(|kinds| kinds.get(argument_index));
        if kind.is_some_and(CallArgKind::is_omitted_default) {
            continue;
        }
        let spread = kind.is_some_and(CallArgKind::is_spread);
        let is_vararg = callable.vararg_index == Some(parameter_index);
        let actual = if spread {
            // Spread arguments are already the element type. The spread kind still carries the
            // source array, which would bind the element variable to that array.
            *argument
        } else if let Some(kind) = kind {
            let shape = if is_vararg {
                declared.array_elem().unwrap_or(declared)
            } else {
                declared
            };
            kind.inference_type(source, shape)
        } else {
            *argument
        };
        if actual == Ty::Error {
            continue;
        }
        // The compact signature graph already maps every vararg input to the element slot.
        // In particular, a positional array value is one element (`T = Array<String>`), while a
        // spread's recorded `argument` is already its element type. Reclassifying either from the
        // runtime array shape would lose that source mapping.
        actuals.push((parameter_index, actual, false));
    }
    actuals
}
