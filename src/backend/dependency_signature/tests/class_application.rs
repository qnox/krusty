//! Checked class applications never become callable-ordinal substitutions or inferred bindings.

use super::*;
use crate::fir::ResolvedParameterIdentity;
use crate::libraries::{klib_body_class_fixture, ExternalCallableKind};
use crate::types::{type_name, SemanticCallableOwner};

fn member() -> BackendCallableFact {
    let (class_contract, dependency_callable) =
        klib_body_class_fixture::generic_member_declaration();
    let class_formal = class_contract.parameters.formals[0];
    let mut signature = generic();
    signature.receiver = None;
    signature.params = vec![Ty::String, class_formal, signature.ret];
    signature.ret = class_formal;
    let mut fact = frozen(signature);
    fact.context_count = 0;
    fact.kind = ExternalCallableKind::Member;
    let owner = class_contract.classifier;
    fact.declaration_owner = Some(SemanticCallableOwner::Classifier(owner));
    fact.parameter_identities = ["fixed", "classValue", "methodValue"]
        .map(|name| ResolvedParameterIdentity::Source(name.into()))
        .into();
    fact.dependency_callable = Some(dependency_callable);
    fact.klib_body_class_contract = Some(class_contract);
    fact
}

#[test]
fn checked_class_slots_and_callable_arguments_keep_separate_declaration_identities() {
    let fact = member();
    let declared = fact.params.clone();
    for class_actual in [Ty::Int, Ty::String, Ty::Long, Ty::nullable(Ty::String)] {
        let checked = vec![Ty::String, class_actual, Ty::Long];
        let application = checked_call(
            checked.clone(),
            class_actual,
            vec![argument(7, 0, Ty::Long)],
        );
        let signature = instantiate_dependency_signature(&fact, &application).unwrap();
        assert_eq!(&*signature.parameters, checked);
        assert_eq!(signature.result, class_actual);
        assert_eq!(signature.receiver, None);
    }
    assert_eq!(fact.params, declared);
    assert_eq!(
        fact.ret,
        fact.klib_body_class_contract
            .as_ref()
            .unwrap()
            .parameters
            .formals[0]
    );
}

#[test]
fn fixed_class_member_slots_results_and_roles_still_require_exact_agreement() {
    let mut fact = member();
    let good = checked_call(
        vec![Ty::String, Ty::Int, Ty::Long],
        Ty::Int,
        vec![argument(7, 0, Ty::Long)],
    );
    let bad_fixed = checked_call(
        vec![Ty::Int, Ty::Int, Ty::Long],
        Ty::Int,
        vec![argument(7, 0, Ty::Long)],
    );
    assert_eq!(
        instantiate_dependency_signature(&fact, &bad_fixed).err(),
        Some("a fixed dependency parameter disagrees with its checked class application")
    );
    let bad_count = checked_call(Vec::new(), Ty::Int, vec![argument(7, 0, Ty::Long)]);
    assert_eq!(
        instantiate_dependency_signature(&fact, &bad_count).err(),
        Some("a dependency class application changed its parameter count")
    );
    let mut bad_role = good.clone();
    let Callee::External {
        extension_receiver_parameter,
        ..
    } = &mut bad_role
    else {
        unreachable!()
    };
    *extension_receiver_parameter = Some(0);
    assert_eq!(
        instantiate_dependency_signature(&fact, &bad_role).err(),
        Some("a dependency class application changed its receiver roles")
    );
    fact.ret = Ty::String;
    fact.generic_sig.as_mut().unwrap().ret = Ty::String;
    assert_eq!(
        instantiate_dependency_signature(&fact, &good).err(),
        Some("a fixed dependency result disagrees with its checked class application")
    );
}

#[test]
fn a_class_generic_member_still_authenticates_every_callable_substitution() {
    let fact = member();
    for (substitutions, expected) in [
        (vec![], "a dependency call is missing a type substitution"),
        (
            vec![argument(8, 0, Ty::Long)],
            "a dependency call carries a foreign type substitution",
        ),
        (
            vec![argument(7, 1, Ty::Long)],
            "a dependency call has an out-of-range type substitution",
        ),
        (
            vec![argument(7, 0, Ty::Long), argument(7, 0, Ty::Long)],
            "a dependency call repeats a type substitution",
        ),
    ] {
        let application = checked_call(vec![Ty::String, Ty::Int, Ty::Long], Ty::Int, substitutions);
        assert_eq!(
            instantiate_dependency_signature(&fact, &application).err(),
            Some(expected)
        );
    }
}

#[test]
fn a_zero_argument_class_result_consumes_its_checked_application_without_receiver_lookup() {
    let mut fact = member();
    fact.params.clear();
    fact.parameter_identities = Box::new([]);
    fact.generic_sig = None;
    let application = checked_call(Vec::new(), Ty::nullable(Ty::String), Vec::new());
    let signature = instantiate_dependency_signature(&fact, &application).unwrap();
    assert!(signature.parameters.is_empty());
    assert_eq!(signature.result, Ty::nullable(Ty::String));
}

#[test]
fn a_generic_member_export_preserves_its_declared_receiver_application() {
    let fact = member();
    let owner = fact.klib_body_class_contract.as_ref().unwrap();
    let receiver = Ty::obj_args_name(owner.classifier, &owner.parameters.formals);
    let mut ir = IrFile::default();
    let function = function(&mut ir, fact.params.clone(), fact.ret);
    ir.functions[function as usize].dispatch_receiver = Some(owner.classifier);
    ir.functions[function as usize].is_static = false;
    let mut exports = super::super::super::DependencyExportSignatures::default();
    exports.record_member_receiver(function, receiver);
    let signature =
        DependencyFunctionSignature::from_ir(&ir, function, exports.member_receiver(function))
            .unwrap();
    assert_eq!(
        signature.parameters(),
        [vec![receiver], fact.params].concat()
    );
    assert_eq!(signature.result(), fact.ret);
}

#[test]
fn a_class_application_requires_a_complete_frozen_member_contract() {
    let application = checked_call(
        vec![Ty::String, Ty::Int, Ty::Long],
        Ty::Int,
        vec![argument(7, 0, Ty::Long)],
    );
    let mut wrong_owner = member();
    wrong_owner.declaration_owner =
        Some(SemanticCallableOwner::Classifier(type_name("sample/Other")));
    let mut wrong_module = member();
    wrong_module
        .klib_body_class_contract
        .as_mut()
        .unwrap()
        .module = "other".into();
    let mut missing_identity = member();
    missing_identity.parameter_identities = Box::new([]);
    let mut missing_bounds = member();
    missing_bounds
        .klib_body_class_contract
        .as_mut()
        .unwrap()
        .parameters
        .bounds = Box::new([]);
    for fact in [wrong_owner, wrong_module, missing_identity, missing_bounds] {
        assert_eq!(
            instantiate_dependency_signature(&fact, &application).err(),
            Some("a dependency class application has an inconsistent declaring-class contract")
        );
    }
    let mut non_formal = member();
    non_formal
        .klib_body_class_contract
        .as_mut()
        .unwrap()
        .parameters
        .formals[0] = Ty::Int;
    assert_eq!(
        instantiate_dependency_signature(&non_formal, &application).err(),
        Some("a dependency class application has a non-formal class parameter")
    );
    let mut repeated = member();
    let formal = repeated
        .klib_body_class_contract
        .as_ref()
        .unwrap()
        .parameters
        .formals[0];
    repeated.generic_sig.as_mut().unwrap().formals[0] = formal.ty_param_name().unwrap().to_owned();
    assert_eq!(
        instantiate_dependency_signature(&repeated, &application).err(),
        Some("a dependency class application repeats a formal identity")
    );
    let mut reified = argument(7, 0, Ty::Long);
    reified.reified = true;
    assert_eq!(
        instantiate_dependency_signature(
            &member(),
            &checked_call(vec![Ty::String, Ty::Int, Ty::Long], Ty::Int, vec![reified],)
        )
        .err(),
        Some("a reified dependency cannot use an ordinary generic export")
    );
}

#[test]
fn empty_callable_formals_do_not_bypass_frozen_signature_authentication() {
    let mut fact = member();
    let class_formal = fact
        .klib_body_class_contract
        .as_ref()
        .unwrap()
        .parameters
        .formals[0];
    fact.params = vec![class_formal];
    fact.parameter_identities =
        vec![ResolvedParameterIdentity::Source("value".into())].into_boxed_slice();
    let signature = fact.generic_sig.as_mut().unwrap();
    signature.formals.clear();
    signature.formal_bounds.clear();
    signature.params = vec![class_formal];
    let application = checked_call(vec![Ty::Int], Ty::Int, Vec::new());
    assert_eq!(
        instantiate_dependency_signature(&fact, &application)
            .unwrap()
            .result,
        Ty::Int
    );
    fact.generic_sig.as_mut().unwrap().ret = Ty::String;
    assert_eq!(
        instantiate_dependency_signature(&fact, &application).err(),
        Some("a dependency generic signature disagrees with its frozen declaration")
    );
    fact.generic_sig.as_mut().unwrap().ret = class_formal;
    fact.generic_sig.as_mut().unwrap().params[0] = Ty::Long;
    assert_eq!(
        instantiate_dependency_signature(&fact, &application).err(),
        Some("a dependency generic signature disagrees with its frozen declaration")
    );
}
