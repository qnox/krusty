//! Declaration-owned dependency signatures: emitted operands and checked generic instantiation
//! have different lifetimes and must never be substituted for one another.

use super::{BackendCallableFact, DependencyMemberReceiver};
use crate::fir::{ExternalCallableId, FirTypeParameterRef};
use crate::fx_hash::HashMap;
use crate::ir::{Callee, FunId, IrCheckedSubstitution, IrFile};
use crate::libraries::GenericSig;
use crate::types::{Ty, TyKind};

mod class_application;

/// The source checker already applied class parameters, including inherited applications. Borrow
/// that complete application; target emission must not reconstruct it from an operand or hierarchy.
struct CheckedDependencyApplication<'a> {
    target: ExternalCallableId,
    parameters: &'a [Ty],
    result: Ty,
    substitutions: &'a [IrCheckedSubstitution],
    extension_receiver_parameter: Option<u32>,
}

impl<'a> CheckedDependencyApplication<'a> {
    fn of(callee: &'a Callee) -> Result<Self, &'static str> {
        let Callee::External {
            target,
            params,
            ret,
            substitutions,
            extension_receiver_parameter,
            ..
        } = callee
        else {
            return Err("a dependency signature has no checked external application");
        };
        Ok(Self {
            target: *target,
            parameters: params,
            result: *ret,
            substitutions,
            extension_receiver_parameter: *extension_receiver_parameter,
        })
    }
}

/// The exact semantic carriers of one emitted function, including retained class dispatch.
/// Targets retain this beside the export identity, not beside an individual call site.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DependencyFunctionSignature {
    parameters: Box<[Ty]>,
    result: Ty,
    coroutine_abi: bool,
}

impl DependencyFunctionSignature {
    pub fn from_ir(
        ir: &IrFile,
        function: FunId,
        receiver: Option<DependencyMemberReceiver>,
    ) -> Result<Self, &'static str> {
        let declared = ir
            .functions
            .get(function as usize)
            .ok_or("a dependency export has no declared function")?;
        let mut parameters = Vec::new();
        match (
            receiver.map(DependencyMemberReceiver::semantic_type),
            declared.dispatch_receiver,
        ) {
            (Some(ty), Some(owner))
                if matches!(ty.kind(), TyKind::Obj(classifier, _)
                    if classifier == owner) =>
            {
                parameters.push(ty)
            }
            (None, None) => {}
            _ => return Err("a dependency export whose member receiver disagrees with its body"),
        }
        parameters.extend_from_slice(&declared.params);
        let coroutine_abi = if let Some((source_parameters, source_result)) =
            ir.suspend_declared_sigs.get(&function)
        {
            let (continuation, completed) = crate::types::continuation_passing(*source_result);
            let mut expected = source_parameters.clone();
            expected.push(continuation);
            if declared.params != expected || declared.ret != completed {
                return Err(
                    "a coroutine export whose CPS signature disagrees with its source declaration",
                );
            }
            true
        } else {
            false
        };
        Ok(Self {
            parameters: parameters.into_boxed_slice(),
            result: declared.ret,
            coroutine_abi,
        })
    }

    pub fn parameters(&self) -> &[Ty] {
        &self.parameters
    }
    pub fn result(&self) -> Ty {
        self.result
    }

    /// Whether this exported function was realized with the common CPS calling convention.
    pub fn has_coroutine_abi(&self) -> bool {
        self.coroutine_abi
    }
}

/// Authenticate a checked dependency suspend call after common lowering adds its physical CPS
/// operand and result carrier. The source application still owns declaration selection and generic
/// substitution; this separate physical application must be exactly that source signature plus the
/// continuation required by its logical result, and the exact export must be a CPS realization.
pub fn authenticate_dependency_coroutine_call(
    source: &Callee,
    physical: &Callee,
    logical_result: Ty,
    export: &DependencyFunctionSignature,
) -> Result<(), &'static str> {
    let Callee::External {
        target: source_target,
        default_provider: source_default_provider,
        params: source_params,
        ret: source_result,
        substitutions: source_substitutions,
        defaults: source_defaults,
        extension_receiver_parameter: source_extension_receiver,
    } = source
    else {
        return Err("a dependency coroutine call without its checked source declaration");
    };
    let Callee::External {
        target,
        default_provider,
        params,
        ret,
        substitutions,
        defaults,
        extension_receiver_parameter,
    } = physical
    else {
        return Err("a dependency coroutine call without its physical external identity");
    };
    let (continuation, completed) = crate::types::continuation_passing(*source_result);
    let mut expected_params = source_params.clone();
    expected_params.push(continuation);
    if source_target != target
        || source_default_provider != default_provider
        || source_substitutions != substitutions
        || source_defaults != defaults
        || source_extension_receiver != extension_receiver_parameter
        || *source_result != logical_result
        || *params != expected_params
        || *ret != completed
        || !defaults.is_empty()
        || !export.has_coroutine_abi()
    {
        return Err(
            "a dependency coroutine call whose selected source and exported CPS signatures disagree",
        );
    }
    Ok(())
}

/// The provider declaration instantiated by this exact checked call. This validates source
/// operand roles; it is NOT the physical signature of the already-emitted dependency export.
pub struct InstantiatedDependencySignature {
    pub parameters: Box<[Ty]>,
    pub receiver: Option<Ty>,
    pub result: Ty,
}

pub fn instantiate_dependency_signature(
    fact: &BackendCallableFact,
    callee: &Callee,
) -> Result<InstantiatedDependencySignature, &'static str> {
    let application = CheckedDependencyApplication::of(callee)?;
    let signature =
        instantiate_callable_signature(fact, application.target, application.substitutions)?;
    class_application::apply(fact, &application, signature)
}

fn instantiate_callable_signature(
    fact: &BackendCallableFact,
    target: ExternalCallableId,
    substitutions: &[IrCheckedSubstitution],
) -> Result<InstantiatedDependencySignature, &'static str> {
    if let Some(signature) = fact.generic_sig.as_deref() {
        if fact.context_count > signature.params.len()
            || signature
                .parameters_with_receiver(fact.context_count)
                .as_ref()
                != fact.params
            || signature.receiver != fact.source_receiver
            || signature.ret != fact.ret
        {
            return Err("a dependency generic signature disagrees with its frozen declaration");
        }
    }
    match fact.generic_sig.as_deref() {
        Some(signature) if !signature.formals.is_empty() => {
            if !fact.reified_type_parameter_ordinals.is_empty() {
                return Err("a reified dependency cannot use an ordinary generic export");
            }
            instantiate(signature, fact.context_count, target, substitutions)
        }
        _ if substitutions.is_empty() => Ok(InstantiatedDependencySignature {
            parameters: fact.params.clone().into_boxed_slice(),
            receiver: fact.source_receiver,
            result: fact.ret,
        }),
        _ => Err("a nongeneric dependency call carries type substitutions"),
    }
}

fn instantiate(
    signature: &GenericSig,
    context_count: usize,
    target: ExternalCallableId,
    substitutions: &[IrCheckedSubstitution],
) -> Result<InstantiatedDependencySignature, &'static str> {
    let mut values = vec![None; signature.formals.len()];
    for substitution in substitutions {
        let FirTypeParameterRef::External { callable, ordinal } = substitution.parameter else {
            return Err("a dependency call carries a foreign type substitution");
        };
        if callable != target {
            return Err("a dependency call carries a foreign type substitution");
        }
        let Some(slot) = values.get_mut(ordinal as usize) else {
            return Err("a dependency call has an out-of-range type substitution");
        };
        if substitution.reified {
            return Err("a reified dependency cannot use an ordinary generic export");
        }
        if slot.replace(substitution.value).is_some() {
            return Err("a dependency call repeats a type substitution");
        }
    }
    let mut bindings = HashMap::default();
    for (formal, value) in signature.formals.iter().zip(values) {
        let value = value.ok_or("a dependency call is missing a type substitution")?;
        if bindings.insert(*formal, value).is_some() {
            return Err("a dependency declaration repeats a formal identity");
        }
    }
    // FIR checked the substitution and all its bounds. Applying its exact formal identities is
    // not inference, erasure, or another bound-narrowing pass. Runtime reification is not used.
    let substitute = |ty| crate::types::ty_subst_applied_arguments(ty, &bindings);
    Ok(InstantiatedDependencySignature {
        parameters: signature
            .parameters_with_receiver(context_count)
            .iter()
            .copied()
            .map(substitute)
            .collect(),
        receiver: signature.receiver.map(substitute),
        result: substitute(signature.ret),
    })
}

#[cfg(test)]
mod tests;
