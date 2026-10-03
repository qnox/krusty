//! Applicability of a lexical local extension as a callable reference.
//!
//! The declaration stays generic. A reference is a candidate only after its receiver unifies
//! through the symbol hierarchy and those bindings satisfy the declaration's formal bounds.
//! An inapplicable local extension leaves a same-named property available.

use crate::libraries::GenericSig;
use crate::symbol_resolver::{
    generic_bindings_satisfy_bounds, instantiate_slot, unify_ty_from_symbols, GSigBinds,
    TypePosition, UnboundSpecialization,
};
use crate::symbol_source::SymbolSource;
use crate::types::Ty;

use super::Signature;

/// The specialized call shape of `signature` when `receiver` is a legal extension receiver.
///
/// A non-generic extension is unchanged when its declared receiver accepts `receiver`. A generic
/// extension unifies its receiver through applied supertypes (`Derived<U> : Base<U>` binds `T` in
/// `Base<T>.pick`), rejects a binding that violates a formal bound, and returns only the
/// specialized shape. `None` means this local extension is not a candidate.
pub(super) fn applicable_local_extension_signature(
    source: &dyn SymbolSource,
    signature: &Signature,
    receiver: Ty,
    mut admits: impl FnMut(Ty, Ty) -> bool,
) -> Option<Signature> {
    let Some(generic) = signature
        .generic_sig
        .as_ref()
        .filter(|generic| generic.receiver.is_some())
    else {
        let declared = signature.source_receiver?;
        return admits(receiver, declared).then(|| signature.clone());
    };
    let mut bindings = GSigBinds::new();
    unify_ty_from_symbols(
        source,
        generic.receiver.expect("filtered generic receiver"),
        receiver,
        &mut bindings,
    );
    if !generic_bindings_satisfy_bounds(generic, &bindings, &mut admits) {
        return None;
    }
    let selected = specialize_local_extension_signature(source, signature, generic, &bindings);
    let declared = selected.source_receiver?;
    admits(receiver, declared).then_some(selected)
}

/// Apply a local extension's own generic receiver bindings to its call-site signature. The
/// declaration remains generic; only the selected call shape is specialized (`Base<T>` on
/// `Derived<String>` yields `T := String` when `Derived<U> : Base<U>`).
fn specialize_local_extension_signature(
    source: &dyn SymbolSource,
    signature: &Signature,
    generic: &GenericSig,
    bindings: &GSigBinds,
) -> Signature {
    let mut selected = signature.clone();
    selected.params = generic
        .params
        .iter()
        .map(|parameter| {
            instantiate_slot(
                source,
                Some(generic),
                *parameter,
                bindings,
                TypePosition::In,
                UnboundSpecialization::Preserve,
            )
        })
        .collect();
    selected.ret = instantiate_slot(
        source,
        Some(generic),
        generic.ret,
        bindings,
        TypePosition::Out,
        UnboundSpecialization::Preserve,
    );
    selected.source_receiver = generic.receiver.map(|declared| {
        instantiate_slot(
            source,
            Some(generic),
            declared,
            bindings,
            TypePosition::Invariant,
            UnboundSpecialization::Preserve,
        )
    });
    selected.lambda_param_types = selected
        .params
        .iter()
        .map(|parameter| match parameter {
            Ty::Fun(function) => function.params.clone(),
            _ => Vec::new(),
        })
        .collect();
    selected
}
