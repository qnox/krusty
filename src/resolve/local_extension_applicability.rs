//! Applicability of a lexical local extension.
//!
//! The declaration stays generic. A call or a callable reference is a candidate only after its
//! receiver unifies through the federated symbol hierarchy and those bindings satisfy the
//! declaration's formal bounds. An inapplicable local extension leaves a same-named property
//! available. Formal bounds use the shared bound-admission contract; the receiver check stays
//! ordinary assignability.

use crate::ast::{ExprId, Stmt, StmtId};
use crate::libraries::GenericSig;
use crate::symbol_resolver::{
    generic_bindings_satisfy_bounds, instantiate_slot, unify_ty_from_symbols, GSigBinds,
    TypePosition, UnboundSpecialization,
};
use crate::symbol_source::SymbolSource;
use crate::types::{FnSig, Ty};

use super::callable_reference_selection::AdaptedRefArgument;
use super::{Checker, CheckerScope, Signature};

pub(super) fn function_expectation(expected: Option<Ty>) -> Option<&'static FnSig> {
    match expected {
        Some(Ty::Fun(function)) => Some(function),
        _ => None,
    }
}

/// One applicable local extension selected for a callable reference, with the shape the reference
/// exposes. An expectation-free reference carries the receiver specialization; a contextual
/// reference still applies the expected callable shape on top of the declaration.
pub(super) struct LocalExtensionReferenceCandidate {
    pub(super) statement: StmtId,
    pub(super) signature: Signature,
    receiver_rank: u32,
    declared_receiver: Ty,
    generic_receiver: bool,
    pub(super) parameters: Vec<Ty>,
    pub(super) ret: Ty,
    pub(super) adaptation: Option<(Vec<AdaptedRefArgument>, bool)>,
    /// Specialized declaration parameters in the target-slot order used by `adaptation`.
    adaptation_parameters: Vec<Ty>,
}

enum LocalExtensionReferenceSelection {
    None,
    Selected(Box<LocalExtensionReferenceCandidate>),
    Ambiguous(Vec<LocalExtensionReferenceCandidate>),
}

impl LocalExtensionReferenceSelection {
    fn into_candidate(
        self,
    ) -> Result<Option<Box<LocalExtensionReferenceCandidate>>, Vec<LocalExtensionReferenceCandidate>>
    {
        match self {
            Self::Selected(candidate) => Ok(Some(candidate)),
            Self::None => Ok(None),
            Self::Ambiguous(candidates) => Err(candidates),
        }
    }
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
    pub(super) fn selected_local_extension_reference(
        &mut self,
        expression: ExprId,
        scope: &CheckerScope<'_>,
        name: &str,
        receiver: Ty,
        expected: Option<&FnSig>,
        leading_receiver_in_expected: bool,
    ) -> Result<Option<Box<LocalExtensionReferenceCandidate>>, Ty> {
        match self
            .select_local_extension_reference(
                scope,
                name,
                receiver,
                expected,
                leading_receiver_in_expected,
            )
            .into_candidate()
        {
            Ok(candidate) => Ok(candidate),
            Err(candidates) => {
                Err(self.report_local_extension_reference_ambiguity(expression, name, &candidates))
            }
        }
    }

    fn report_local_extension_reference_ambiguity(
        &mut self,
        expression: ExprId,
        name: &str,
        candidates: &[LocalExtensionReferenceCandidate],
    ) -> Ty {
        let mut message = "overload resolution ambiguity between candidates:".to_string();
        for candidate in candidates {
            let Stmt::LocalFun(function) = self.file.stmt(candidate.statement) else {
                unreachable!("a local-extension reference candidate must be a local function")
            };
            message.push('\n');
            message.push_str(&super::source_function_display(
                self.file,
                function,
                candidate.signature.ret,
            ));
        }
        self.diags
            .error(self.member_name_span(expression, name), message);
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
        matches!(
            self.select_local_extension_reference(
                scope,
                name,
                receiver,
                Some(expected),
                leading_receiver_in_expected,
            ),
            LocalExtensionReferenceSelection::Selected(_)
        )
    }

    fn select_local_extension_reference(
        &self,
        scope: &CheckerScope<'_>,
        name: &str,
        receiver: Ty,
        expected: Option<&FnSig>,
        leading_receiver_in_expected: bool,
    ) -> LocalExtensionReferenceSelection {
        let mut candidates = self.local_extension_reference_candidates(
            scope,
            name,
            receiver,
            expected,
            leading_receiver_in_expected,
        );
        let Some(nearest_receiver) = candidates
            .iter()
            .map(|candidate| candidate.receiver_rank)
            .min()
        else {
            return LocalExtensionReferenceSelection::None;
        };
        candidates.retain(|candidate| candidate.receiver_rank == nearest_receiver);

        // A repeated normalized fact for the same declaration is one candidate. Distinct local
        // declarations remain overloads even when adaptation erases their visible difference;
        // their specialized declared receiver and source parameter shape own that identity.
        let mut unique = Vec::<LocalExtensionReferenceCandidate>::new();
        for candidate in candidates {
            if unique.iter().any(|existing| {
                existing.statement == candidate.statement
                    && existing.declared_receiver == candidate.declared_receiver
                    && existing.generic_receiver == candidate.generic_receiver
                    && existing.signature.params == candidate.signature.params
                    && existing.ret == candidate.ret
                    && existing.signature.is_suspend() == candidate.signature.is_suspend()
            }) {
                continue;
            }
            unique.push(candidate);
        }
        let receiver_maximal = unique
            .iter()
            .enumerate()
            .filter_map(|(index, current)| {
                let dominated = unique.iter().enumerate().any(|(other_index, other)| {
                    if index == other_index {
                        return false;
                    }
                    if current.generic_receiver != other.generic_receiver {
                        return super::member_extension_selection::concrete_receiver_is_more_specific(
                            other.generic_receiver,
                            current.generic_receiver,
                        );
                    }
                    let other_receiver_is_subtype =
                        self.receiver_is_assignable(other.declared_receiver, current.declared_receiver);
                    let current_receiver_is_subtype =
                        self.receiver_is_assignable(current.declared_receiver, other.declared_receiver);
                    if other_receiver_is_subtype != current_receiver_is_subtype {
                        return other_receiver_is_subtype;
                    }
                    false
                });
                (!dominated).then_some(index)
            })
            .collect::<Vec<_>>();
        let maximal = if expected.is_some() {
            let specificity = receiver_maximal
                .iter()
                .map(|&index| {
                    let candidate = &unique[index];
                    super::callable_reference_selection::AdaptedReferenceSpecificity {
                        parameters: &candidate.adaptation_parameters,
                        ret: candidate.ret,
                        plan: &candidate
                            .adaptation
                            .as_ref()
                            .expect("expected local reference has an adaptation plan")
                            .0,
                        is_vararg: candidate.signature.vararg_index.is_some(),
                    }
                })
                .collect::<Vec<_>>();
            super::callable_reference_selection::maximal_adapted_references(
                &specificity,
                |left_params, left_ret, right_params, right_ret| {
                    self.callable_ref_shape_at_least_as_specific(
                        left_params,
                        left_ret,
                        right_params,
                        right_ret,
                    )
                },
            )
            .into_iter()
            .map(|index| receiver_maximal[index])
            .collect()
        } else {
            receiver_maximal
                .iter()
                .copied()
                .filter(|&index| {
                    let current = &unique[index];
                    !receiver_maximal.iter().copied().any(|other_index| {
                        if index == other_index {
                            return false;
                        }
                        let other = &unique[other_index];
                        self.callable_ref_shape_at_least_as_specific(
                            &other.parameters,
                            other.ret,
                            &current.parameters,
                            current.ret,
                        ) && !self.callable_ref_shape_at_least_as_specific(
                            &current.parameters,
                            current.ret,
                            &other.parameters,
                            other.ret,
                        )
                    })
                })
                .collect()
        };
        match maximal.as_slice() {
            [selected] => {
                LocalExtensionReferenceSelection::Selected(Box::new(unique.swap_remove(*selected)))
            }
            _ => LocalExtensionReferenceSelection::Ambiguous(
                unique
                    .into_iter()
                    .enumerate()
                    .filter_map(|(index, candidate)| maximal.contains(&index).then_some(candidate))
                    .collect(),
            ),
        }
    }

    /// Local extensions applicable to `receiver::name` or `Receiver::name`.
    ///
    /// `leading_receiver_in_expected` is the unbound `Receiver::name` shape: the expected callable's
    /// first parameter is that receiver, and the exposed function type puts it back in front. A
    /// bound `value::name` reference compares the expected value parameters directly.
    fn local_extension_reference_candidates(
        &self,
        scope: &CheckerScope<'_>,
        name: &str,
        receiver: Ty,
        expected: Option<&FnSig>,
        leading_receiver_in_expected: bool,
    ) -> Vec<LocalExtensionReferenceCandidate> {
        let source = self.fed_source();
        let receiver_mro = crate::symbol_resolver::ReceiverMro::new(&source, receiver);
        for overloads in self.lookup_local_fun_overload_rungs(scope, name) {
            let candidates = overloads
                .into_iter()
                .filter_map(|(statement, declared)| {
                    let receiver_domain = declared
                        .generic_sig
                        .as_ref()
                        .and_then(|generic| generic.receiver)
                        .or(declared.source_receiver)?;
                    let generic_receiver = declared.generic_sig.as_ref().is_some_and(|generic| {
                        crate::types::ty_mentions_param(receiver_domain, &generic.formals)
                    });
                    let signature = applicable_local_extension_signature(
                        &source,
                        &declared,
                        receiver,
                        |actual, bound| self.generic_bound_admits(actual, bound),
                        |actual, expected| self.receiver_is_assignable(actual, expected),
                    )?;
                    let specialized_receiver = signature.source_receiver?;
                    let receiver_rank = receiver_mro.rank(&source, specialized_receiver)?;
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
                        receiver_rank,
                        declared_receiver: receiver_domain,
                        generic_receiver,
                        parameters,
                        ret,
                        adaptation: adaptation.flatten(),
                        adaptation_parameters: value_parameters,
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
