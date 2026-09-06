//! Generic functional-interface construction.
//!
//! A SAM constructor is not an interface constructor with a synthetic vararg. Its single function
//! operand constrains the classifier's type parameters, and the selected result is the applied
//! interface type. This module owns that inference boundary so ordinary constructor argument mapping
//! never has to approximate it.

use crate::libraries::{GenericReturnPolicy, GenericSig};
use crate::symbol_resolver::{
    generic_bindings_satisfy_bounds, ty_subst_keep_unbound, unify_ty_from_symbols, GSigBinds,
    SamSignature,
};
use crate::symbol_source::SymbolSource;
use crate::types::{Ty, TypeVariance};

pub(super) struct SelectedSamConstructor {
    pub result: Ty,
    pub signature: SamSignature,
}

/// Validate a SAM operand against an application already fixed by explicit type arguments or an
/// enclosing expected type. The caller-owned arguments are not inference variables of the SAM
/// constructor: reopening the classifier formals here would replace dependent lexical parameters
/// with unrelated top completions before bounds are checked.
pub(super) fn select_fixed_sam_constructor(
    source: &dyn SymbolSource,
    signature: SamSignature,
    result: Ty,
    actual: Ty,
) -> Option<SelectedSamConstructor> {
    let callable = match actual.non_null() {
        function @ Ty::Fun(_) => function,
        nominal => crate::symbol_resolver::classifier_callable_signature(source, nominal)?,
    };
    let Ty::Fun(actual_function) = callable else {
        return None;
    };
    let expected = Ty::fun_with_shape(
        signature.params.clone(),
        signature.ret,
        signature.context_count,
        signature.has_receiver,
        signature.suspend,
    );
    let Ty::Fun(expected) = expected else {
        unreachable!("a SAM method always has a function shape")
    };
    let oracle = crate::symbol_resolver::SourceOracle(source);
    super::callable_reference_selection::is_compatible(
        &actual_function.params,
        actual_function.ret,
        actual_function.suspend,
        expected,
        false,
        |actual, target| {
            crate::assignable::is_assignable(
                &crate::assignable::TyCtx::new(),
                &oracle,
                actual,
                target,
            )
        },
    )
    .then_some(SelectedSamConstructor { result, signature })
}

fn unconstrained_argument(variance: TypeVariance, bounds: &[Ty]) -> Ty {
    let upper = bounds
        .first()
        .copied()
        .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")));
    match variance {
        TypeVariance::In if upper.is_nullable() => Ty::nullable(Ty::Nothing),
        TypeVariance::In => Ty::Nothing,
        TypeVariance::Invariant | TypeVariance::Out => upper,
    }
}

pub(super) fn select_sam_constructor(
    source: &dyn SymbolSource,
    mut signature: SamSignature,
    actual: Ty,
) -> Option<SelectedSamConstructor> {
    let callable = match actual.non_null() {
        function @ Ty::Fun(_) => function,
        nominal => crate::symbol_resolver::classifier_callable_signature(source, nominal)?,
    };
    let Ty::Fun(actual_function) = callable else {
        return None;
    };
    if actual_function.params.len() != signature.params.len()
        // Suspend conversion is one-way: an ordinary function value can implement a suspend SAM
        // through the same adapter used for a suspend function-type parameter. A suspend value
        // cannot implement an ordinary SAM because that would discard its continuation contract.
        || (actual_function.suspend && !signature.suspend)
    {
        return None;
    }

    let classifier = source.classifier(signature.internal)?;
    let formals = classifier.type_params.clone();
    if formals.is_empty() {
        let expected = Ty::fun_with_shape(
            signature.params.clone(),
            signature.ret,
            signature.context_count,
            signature.has_receiver,
            signature.suspend,
        );
        let Ty::Fun(expected) = expected else {
            unreachable!("a SAM method always has a function shape")
        };
        let oracle = crate::symbol_resolver::SourceOracle(source);
        return super::callable_reference_selection::is_compatible(
            &actual_function.params,
            actual_function.ret,
            actual_function.suspend,
            expected,
            false,
            |actual, target| {
                crate::assignable::is_assignable(
                    &crate::assignable::TyCtx::new(),
                    &oracle,
                    actual,
                    target,
                )
            },
        )
        .then_some(SelectedSamConstructor {
            result: Ty::obj_name(signature.internal),
            signature,
        });
    }

    let declared_function = Ty::fun_with_shape(
        signature.params.clone(),
        signature.ret,
        signature.context_count,
        signature.has_receiver,
        signature.suspend,
    );
    let mut bindings = GSigBinds::new();
    let inference_actual = Ty::fun_with_shape(
        actual_function.params.clone(),
        actual_function.ret,
        signature.context_count,
        signature.has_receiver,
        actual_function.suspend,
    );
    unify_ty_from_symbols(source, declared_function, inference_actual, &mut bindings);
    for (index, formal) in formals.iter().enumerate() {
        bindings.entry(formal.clone()).or_insert_with(|| {
            unconstrained_argument(
                classifier
                    .type_param_variances
                    .get(index)
                    .copied()
                    .unwrap_or_default(),
                classifier
                    .type_param_bounds
                    .get(index)
                    .map(Vec::as_slice)
                    .unwrap_or_default(),
            )
        });
    }
    let generic = GenericSig {
        formals: formals.clone(),
        formal_bounds: classifier.type_param_bounds.clone(),
        receiver: None,
        params: signature.params.clone(),
        ret: signature.ret,
        return_policy: GenericReturnPolicy::Exact,
    };
    let oracle = crate::symbol_resolver::SourceOracle(source);
    if !generic_bindings_satisfy_bounds(&generic, &bindings, |actual, bound| {
        crate::assignable::is_assignable(&crate::assignable::TyCtx::new(), &oracle, actual, bound)
    }) {
        return None;
    }

    signature.params = signature
        .params
        .iter()
        .map(|parameter| ty_subst_keep_unbound(*parameter, &bindings))
        .collect();
    signature.ret = ty_subst_keep_unbound(signature.ret, &bindings);
    let arguments = formals
        .iter()
        .map(|formal| bindings.get(formal).copied())
        .collect::<Option<Vec<_>>>()?;
    Some(SelectedSamConstructor {
        result: Ty::obj_args_name(signature.internal, &arguments),
        signature,
    })
}
