//! A final member of an ordinary class, or any member of a final ordinary class, lowers from the
//! body contract the provider already published. The call is that declaration: the class is final
//! or the member is, so nothing virtual replaces it.

use super::*;
use crate::klib_libraries::{KlibCalleeOrigin, KlibIntrinsicFacts};
use crate::libraries::{ExternalCallableRealization, KlibDeclarationSignature};
use crate::metadata::klib_ir::KlibIrSignature;
use crate::symbol_source::SymbolSource;

fn member_realization(
    libraries: &KlibLibraries,
    classifier: &str,
    name: &str,
    property: bool,
) -> ExternalCallableRealization {
    let class = libraries
        .classifier(type_name(classifier))
        .unwrap_or_else(|| panic!("missing {classifier}"));
    let target = if property {
        class.declared_callables[name].properties()[0]
            .getter
            .external_identity
    } else {
        class.declared_callables[name].functions()[0]
            .callable
            .external_identity
    }
    .unwrap_or_else(|| panic!("{classifier}.{name} has no external identity"));
    libraries
        .external_callable(target)
        .unwrap_or_else(|| panic!("{classifier}.{name} has no realization"))
}

fn provider_identities(
    libraries: &KlibLibraries,
    bodies: &KlibDeclarationBodies,
    root: &crate::libraries::KlibDependencyCallable,
    found: &mut Vec<crate::libraries::KlibDependencyCallable>,
    bodyless: &mut Vec<crate::libraries::KlibDependencyCallable>,
    seen: &mut crate::fx_hash::HashSet<crate::libraries::KlibDependencyCallable>,
) {
    for callee in bodies.emitted_body_callables(root, crate::klib_lowering::EMITTED_CALL_FORMS) {
        if !seen.insert(callee.clone()) {
            continue;
        }
        if bodies.function_has_body(&callee) == Some(false) {
            bodyless.push(callee.clone());
        } else if libraries
            .external_callable_by_dependency(&callee)
            .is_some_and(|(_, realization)| realization.klib_body_callable().is_some())
        {
            found.push(callee.clone());
        }
        provider_identities(libraries, bodies, &callee, found, bodyless, seen);
    }
}

#[test]
fn a_member_of_a_final_ordinary_class_lowers_from_its_body_contract() {
    let Some(root) = super::distribution_root() else {
        return;
    };
    let libraries = super::stdlib(&root);
    let bodies = libraries.declaration_bodies();
    for (classifier, name, property) in [
        ("kotlin/ranges/IntRange", "endInclusive", true),
        ("kotlin/text/StringBuilder", "toString", false),
    ] {
        let realization = member_realization(&libraries, classifier, name, property);
        let root_identity = {
            let callable = realization
                .klib_body_callable()
                .unwrap_or_else(|| panic!("{classifier}.{name} has no body contract"));
            assert!(
                callable.closes_as_ordinary_member(),
                "{classifier}.{name} is a final class's ordinary member"
            );
            callable.identity().clone()
        };
        let (arena, function) = bodies
            .function(&root_identity)
            .unwrap_or_else(|| panic!("{classifier}.{name} has no serialized body"));
        for symbol in
            arena.emitted_callable_symbols(function, crate::klib_lowering::EMITTED_CALL_FORMS)
        {
            let KlibIrSignature::Accessor(accessor) = &symbol.signature else {
                continue;
            };
            if accessor.property().declaration().segments() != ["IntRange", "last"] {
                continue;
            }
            let reference = KlibDeclarationSignature::Accessor(accessor.clone());
            let KlibCalleeOrigin::Exact(linked) =
                bodies.linked_origin(&root_identity.module, &reference)
            else {
                panic!("IntRange.last links to no declaration");
            };
            let KlibDeclarationSignature::Accessor(linked) = linked.declaration else {
                panic!("IntRange.last links to {linked:?}");
            };
            assert_eq!(
                linked.property().declaration().segments(),
                ["IntProgression", "last"]
            );
        }
        let mut identities = Vec::new();
        let mut bodyless = Vec::new();
        let mut seen = crate::fx_hash::HashSet::default();
        seen.insert(root_identity.clone());
        provider_identities(
            &libraries,
            bodies,
            &root_identity,
            &mut identities,
            &mut bodyless,
            &mut seen,
        );
        let mut realizations = vec![realization];
        for identity in &identities {
            if let Some((_, found)) = libraries.external_callable_by_dependency(identity) {
                realizations.push(found);
            }
        }
        // A declaration the library publishes without a body stays the provider leaf the target
        // realizes. Its callers still lower.
        let mut externals = crate::fx_hash::HashMap::default();
        for identity in &bodyless {
            if let Some((external, _)) = libraries.external_callable_by_dependency(identity) {
                externals.insert(
                    identity.clone(),
                    super::super::body_unit::EmittedDependencyCallable::provider_leaf(external),
                );
            }
        }
        let oracle = crate::symbol_resolver::SourceOracle(&libraries);
        let facts = KlibCalleeFacts::new(
            realizations
                .iter()
                .filter_map(ExternalCallableRealization::klib_body_callable),
        )
        .with_type_oracle(&oracle);
        let callable = realizations[0]
            .klib_body_callable()
            .unwrap_or_else(|| panic!("{classifier}.{name} has no body contract"));
        let mut unit = DependencyBodyUnit::default();
        let function = unit
            .lower_function_with_declarations(
                callable,
                bodies,
                &facts,
                KlibIntrinsicFacts::empty(),
                &externals,
                &libraries,
            )
            .unwrap_or_else(|decline| panic!("{classifier}.{name}: {decline}"));
        let ir = unit.ir();
        let declared = &ir.functions[function as usize];
        assert!(
            !declared.is_static,
            "{name} reads the object it is called on"
        );
        assert_eq!(
            declared.dispatch_receiver,
            Some(type_name(classifier)),
            "{name}"
        );
        assert!(declared.body.is_some(), "{name} has a body");
        match name {
            "endInclusive" => {
                assert!(
                    ir.functions.iter().any(|function| {
                        function.dispatch_receiver
                            == Some(type_name("kotlin/ranges/IntProgression"))
                    }),
                    "endInclusive calls the final last it inherits"
                );
                assert!(
                    ir.exprs
                        .iter()
                        .any(|expression| matches!(expression, IrExpr::GetField { .. })),
                    "last reads the field it declares"
                );
            }
            "toString" => {
                assert!(
                    ir.functions.len() > 1,
                    "the private getters toString calls are emitted with it"
                );
                assert!(
                    ir.exprs.iter().any(|expression| {
                        matches!(
                            expression,
                            IrExpr::Call { callee, .. }
                                if matches!(**callee, Callee::External { .. })
                        )
                    }),
                    "toString calls the bodyless array copy the target realizes"
                );
            }
            _ => {}
        }
    }
}
