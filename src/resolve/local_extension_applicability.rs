//! Applicability of a lexical local extension.
//!
//! The declaration stays generic. A call or a callable reference is a candidate only after its
//! receiver unifies through the federated symbol hierarchy and those bindings satisfy the
//! declaration's formal bounds. An inapplicable local extension leaves a same-named property
//! available. Formal bounds use the shared bound-admission contract; the receiver check stays
//! ordinary assignability.

use crate::ast::{ExprId, StmtId};
use crate::libraries::GenericSig;
use crate::symbol_resolver::{
    generic_bindings_satisfy_bounds, instantiate_slot, unify_ty_from_symbols, GSigBinds,
    TypePosition, UnboundSpecialization,
};
use crate::symbol_source::SymbolSource;
use crate::types::{FnSig, Ty};

use super::callable_reference_selection::AdaptedRefArgument;
use super::{Checker, CheckerScope, Signature};

/// One applicable local extension selected for a callable reference, with the shape the reference
/// exposes. An expectation-free reference carries the receiver specialization; a contextual
/// reference still applies the expected callable shape on top of the declaration.
pub(super) struct LocalExtensionReferenceCandidate {
    pub(super) statement: StmtId,
    pub(super) signature: Signature,
    pub(super) parameters: Vec<Ty>,
    pub(super) ret: Ty,
    pub(super) adaptation: Option<(Vec<AdaptedRefArgument>, bool)>,
}

/// The specialized call shape of `signature` when `receiver` is a legal extension receiver.
///
/// A non-generic extension is unchanged when its declared receiver accepts `receiver`. A generic
/// extension unifies its receiver through applied supertypes (`Derived<U> : Base<U>` binds `T` in
/// `Base<T>.pick`), rejects a binding that violates a formal bound, and returns only the
/// specialized shape. `None` means this local extension is not a candidate. `bounds_admit` is the
/// shared formal-bound contract; `receiver_admits` is receiver assignability.
pub(super) fn applicable_local_extension_signature(
    source: &dyn SymbolSource,
    signature: &Signature,
    receiver: Ty,
    mut bounds_admit: impl FnMut(Ty, Ty) -> bool,
    mut receiver_admits: impl FnMut(Ty, Ty) -> bool,
) -> Option<Signature> {
    let Some(generic) = signature
        .generic_sig
        .as_ref()
        .filter(|generic| generic.receiver.is_some())
    else {
        let declared = signature.source_receiver?;
        return receiver_admits(receiver, declared).then(|| signature.clone());
    };
    let mut bindings = GSigBinds::new();
    unify_ty_from_symbols(
        source,
        generic.receiver.expect("filtered generic receiver"),
        receiver,
        &mut bindings,
    );
    if !generic_bindings_satisfy_bounds(generic, &bindings, &mut bounds_admit) {
        return None;
    }
    let selected = specialize_local_extension_signature(source, signature, generic, &bindings);
    let declared = selected.source_receiver?;
    receiver_admits(receiver, declared).then_some(selected)
}

impl Checker<'_> {
    pub(super) fn report_local_extension_reference_ambiguity(
        &mut self,
        expression: ExprId,
        name: &str,
    ) -> Ty {
        self.diags.error(
            self.member_name_span(expression, name),
            "overload resolution ambiguity between candidates:".to_string(),
        );
        Ty::Error
    }

    pub(super) fn local_extension_reference_adapts_to(
        &self,
        scope: &CheckerScope<'_>,
        name: &str,
        receiver: Ty,
        expected: &FnSig,
        leading_receiver_in_expected: bool,
    ) -> bool {
        self.local_extension_reference_candidates(
            scope,
            name,
            receiver,
            Some(expected),
            leading_receiver_in_expected,
        )
        .len()
            == 1
    }

    /// Local extensions applicable to `receiver::name` or `Receiver::name`.
    ///
    /// `leading_receiver_in_expected` is the unbound `Receiver::name` shape: the expected callable's
    /// first parameter is that receiver, and the exposed function type puts it back in front. A
    /// bound `value::name` reference compares the expected value parameters directly.
    pub(super) fn local_extension_reference_candidates(
        &self,
        scope: &CheckerScope<'_>,
        name: &str,
        receiver: Ty,
        expected: Option<&FnSig>,
        leading_receiver_in_expected: bool,
    ) -> Vec<LocalExtensionReferenceCandidate> {
        for overloads in self.lookup_local_fun_overload_rungs(scope, name) {
            let candidates = overloads
                .into_iter()
                .filter_map(|(statement, declared)| {
                    let signature = applicable_local_extension_signature(
                        &self.fed_source(),
                        &declared,
                        receiver,
                        |actual, bound| self.generic_bound_admits(actual, bound),
                        |actual, expected| self.receiver_is_assignable(actual, expected),
                    )?;
                    let (value_parameters, ret) = match expected {
                        Some(expected) => {
                            let expected_values = if leading_receiver_in_expected {
                                let (_, values) = expected.params.split_first()?;
                                values
                            } else {
                                expected.params.as_slice()
                            };
                            self.contextual_local_function_reference_shape(
                                &declared,
                                Some(receiver),
                                expected_values,
                                expected.ret,
                            )?
                        }
                        None => (signature.params.clone(), signature.ret),
                    };
                    let parameters = if leading_receiver_in_expected {
                        let mut parameters = vec![receiver];
                        parameters.extend(value_parameters.iter().copied());
                        parameters
                    } else {
                        value_parameters.clone()
                    };
                    let adaptation = expected.map(|expected| {
                        let expected_values = if leading_receiver_in_expected {
                            let (expected_receiver, values) = expected.params.split_first()?;
                            if !self.receiver_is_assignable(*expected_receiver, receiver) {
                                return None;
                            }
                            values
                        } else {
                            expected.params.as_slice()
                        };
                        if (declared.is_suspend() && !expected.suspend)
                            || (expected.ret != Ty::Unit
                                && !self.receiver_is_assignable(ret, expected.ret))
                        {
                            return None;
                        }
                        let call_sig = crate::libraries::CallSig {
                            param_defaults: declared.param_defaults.clone(),
                            required: declared.required,
                            vararg: declared.vararg(),
                            vararg_index: declared.vararg_index,
                            ..crate::libraries::CallSig::default()
                        };
                        let plan = self.callable_ref_parameter_plan(
                            &value_parameters,
                            &call_sig,
                            expected_values,
                        )?;
                        Some((plan, expected.suspend && !declared.is_suspend()))
                    });
                    if matches!(adaptation, Some(None)) {
                        return None;
                    }
                    Some(LocalExtensionReferenceCandidate {
                        statement,
                        signature,
                        parameters,
                        ret,
                        adaptation: adaptation.flatten(),
                    })
                })
                .collect::<Vec<_>>();
            if !candidates.is_empty() {
                return candidates;
            }
        }
        Vec::new()
    }
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
