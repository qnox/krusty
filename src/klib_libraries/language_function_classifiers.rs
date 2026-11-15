//! The function classifiers the language declares (`FunctionN`, `SuspendFunctionN` and their
//! reflective families), as this provider publishes them.
//!
//! No KLIB serializes these classifiers, but serialized IR names their `invoke` by its public
//! signature like any other declaration (`kotlin/Function1.invoke|invoke(1:0){}[0]`). The record
//! is the common language declaration; each `invoke` it declares receives the identity of that
//! exact signature, so a class overriding `invoke` has an edge to the declaration it overrides.

use std::sync::Arc;

use super::external_identities::ExternalIdentities;
use crate::libraries::function_classifiers::{self, FunctionClassifier};
use crate::libraries::LibraryType;
use crate::metadata::id_signature::{
    callable_signature, CallableShape, ClassScope, DeclarationContainer, KlibPublicIdSignature,
    ManglingError, Placement, TypeParameterShape,
};
use crate::metadata::semantic::{KotlinType, KotlinTypeParameterId};

/// The record of `function`, with the identity of every `invoke` it declares itself. A reflective
/// classifier inherits its `invoke` from the callable classifier, which carries that identity.
pub(super) fn function_classifier_record(
    identities: &ExternalIdentities,
    function: FunctionClassifier,
) -> Arc<LibraryType> {
    let mut record = (*function_classifiers::synthetic(function)).clone();
    let owner = function.identity();
    let signature = invoke_signature(function)
        .expect("a language function classifier's `invoke` has a public signature");
    function_classifiers::identify_declared_invoke(&mut record, owner, |invoke| {
        identities.assign_language_member(signature.clone(), invoke);
    });
    Arc::new(record)
}

/// The public signature of `function`'s `invoke`: a member of the classifier whose parameters are
/// the classifier's leading type parameters, the last one being the result.
fn invoke_signature(function: FunctionClassifier) -> Result<KlibPublicIdSignature, ManglingError> {
    let classifier = KlibPublicIdSignature::classifier(function.identity());
    let [name] = classifier.declaration().segments() else {
        return Err(ManglingError::new(
            "a language function classifier is a top-level class",
        ));
    };
    let parameters = (0..=function.arity() as u64)
        .map(|index| KotlinType::Param {
            name: String::new(),
            id: KotlinTypeParameterId(index),
            nullable: false,
        })
        .collect::<Vec<_>>();
    let classes = [ClassScope {
        name,
        type_parameters: parameters
            .iter()
            .enumerate()
            .map(|(index, _)| TypeParameterShape {
                id: KotlinTypeParameterId(index as u64),
                bounds: Vec::new(),
            })
            .collect(),
        expect: false,
    }];
    callable_signature(
        DeclarationContainer {
            package: classifier.package().segments(),
            classes: &classes,
            native_interop_library: false,
        },
        &CallableShape {
            name: "invoke",
            contexts: Vec::new(),
            receiver: None,
            params: parameters[..function.arity()].iter().collect(),
            vararg: None,
            type_parameters: Vec::new(),
            expect: false,
            placement: Placement::Ordinary,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::type_name;

    fn invoke_of(classifier: &str) -> KlibPublicIdSignature {
        let function = function_classifiers::classifier(type_name(classifier))
            .expect("a language function classifier");
        invoke_signature(function).expect("an `invoke` signature")
    }

    #[test]
    fn invoke_has_the_signature_the_stdlib_ir_calls_it_by() {
        // The member ids the Kotlin/Native stdlib's serialized IR names `invoke` by (`run`, `let`
        // and `fold` call it): `invoke(){}`, `invoke(1:0){}` and `invoke(1:0;1:1){}`, each
        // parameter the declaring class's type parameter at that index.
        for (classifier, member_id) in [
            ("Function0", 13_783_652_741_089_290_743),
            ("Function1", 14_459_488_423_395_668_385),
            ("Function2", 3_397_681_596_704_175_240),
        ] {
            let invoke = invoke_of(&format!("kotlin/{classifier}"));
            assert_eq!(invoke.package().segments(), ["kotlin"]);
            assert_eq!(invoke.declaration().segments(), [classifier, "invoke"]);
            assert_eq!(invoke.member_id(), Some(member_id), "{classifier}");
            assert_eq!(invoke.mask(), 0, "{classifier}");
        }
        let suspend = invoke_of("kotlin/coroutines/SuspendFunction2");
        assert_eq!(suspend.package().segments(), ["kotlin", "coroutines"]);
        assert_eq!(
            suspend.declaration().segments(),
            ["SuspendFunction2", "invoke"]
        );
        assert_eq!(
            suspend.member_id(),
            invoke_of("kotlin/Function2").member_id(),
            "the same parameter shape mangles alike"
        );
    }

    #[test]
    fn every_record_of_one_classifier_names_the_same_invoke() {
        let identities = ExternalIdentities::default();
        let function = function_classifiers::classifier(type_name("kotlin/Function0"))
            .expect("a language function classifier");
        let identity = |record: &LibraryType| {
            let invoke = &record.declared_callables["invoke"].functions()[0];
            let member = record
                .members
                .iter()
                .find(|member| member.name == "invoke")
                .expect("the `invoke` member");
            assert_eq!(member.external_identity, invoke.callable.external_identity);
            invoke.callable.external_identity.expect("an identity")
        };
        let first = identity(&function_classifier_record(&identities, function));
        let second = identity(&function_classifier_record(&identities, function));
        assert_eq!(first, second);
        let realization = identities.realization(first).expect("its realization");
        assert_eq!(
            realization.callable.semantic_role,
            Some(crate::types::SemanticCallRole::KotlinFunctionInvoke)
        );
    }
}
