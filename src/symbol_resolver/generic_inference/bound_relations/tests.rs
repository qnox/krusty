use super::*;
use crate::libraries::EmptySymbolSource;
use crate::symbol_resolver::{infer_generic_call_bindings_from_symbols, resolution_subtype};
use crate::types::{declaration_type_parameter, TyParamName};

fn local_signature() -> (GenericSig, Ty) {
    // Compilation zero is outside the production SymbolTable allocation domain (which starts
    // at one), so these fixture identities cannot replace a concurrent compiler's source names.
    let outer = declaration_type_parameter(0, 3123, 10, 0, "T");
    let inner = declaration_type_parameter(0, 3123, 50, 0, "S");
    let caller = Ty::ty_param(outer, Ty::nullable(Ty::obj("kotlin/Any")));
    let callee = Ty::ty_param(inner, caller);
    let signature = GenericSig {
        formals: vec![TyParamName::new(inner)],
        formal_bounds: vec![vec![caller]],
        receiver: None,
        params: vec![callee],
        ret: callee,
        return_policy: Default::default(),
    };
    assert_eq!(signature.formals.len(), 1);
    assert_ne!(signature.formals[0], TyParamName::new(outer));
    (signature, caller)
}

#[test]
fn inferred_caller_parameter_satisfies_its_exact_foreign_bound_at_both_validation_boundaries() {
    let (signature, caller) = local_signature();
    let source = EmptySymbolSource;
    let bindings =
        infer_generic_call_bindings_from_symbols(&source, &signature, [(0, caller, true)], None);
    assert_eq!(bindings.get(&signature.formals[0]), Some(&caller));
    assert!(!bindings.contains_key(caller.ty_param_name().unwrap()));
    assert_eq!(
        generic_binding_bound_violation(&signature, &bindings, |actual, bound| {
            resolution_subtype(&source, actual, bound)
        }),
        None,
    );
    for expected in [None, Some(&bindings)] {
        assert!(generic_bindings_admit_expected_return_intersection(
            &signature,
            &bindings,
            expected,
            |actual, bound| resolution_subtype(&source, actual, bound),
        ));
    }
}

#[test]
fn an_unrelated_argument_cannot_satisfy_an_enclosing_parameter_by_its_erased_upper_bound() {
    let (signature, caller) = local_signature();
    let source = EmptySymbolSource;
    let bindings = infer_generic_call_bindings_from_symbols(
        &source,
        &signature,
        [(0, Ty::String, true)],
        None,
    );
    assert_eq!(bindings.get(&signature.formals[0]), Some(&Ty::String));
    assert_eq!(
        generic_binding_bound_violation(&signature, &bindings, |actual, bound| {
            resolution_subtype(&source, actual, bound)
        }),
        Some((caller, Ty::String)),
    );
    for expected in [None, Some(&bindings)] {
        assert!(!generic_bindings_admit_expected_return_intersection(
            &signature,
            &bindings,
            expected,
            |actual, bound| resolution_subtype(&source, actual, bound),
        ));
    }
}

#[test]
fn a_return_only_formal_keeps_the_existing_expected_intersection_policy() {
    let (mut signature, _) = local_signature();
    signature.params.clear();
    let source = EmptySymbolSource;
    let bindings = [(signature.formals[0], Ty::String)]
        .into_iter()
        .collect::<GSigBinds>();
    assert!(!generic_bindings_admit_expected_return_intersection(
        &signature,
        &bindings,
        None,
        |actual, bound| resolution_subtype(&source, actual, bound),
    ));
    assert!(generic_bindings_admit_expected_return_intersection(
        &signature,
        &bindings,
        Some(&bindings),
        |actual, bound| resolution_subtype(&source, actual, bound),
    ));
}
