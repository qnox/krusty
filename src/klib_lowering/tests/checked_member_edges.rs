//! Edges the emission closure requires from a class initialization, not only from the constructor
//! body's own symbols.

use super::*;

/// `IntRange`'s initialization emits the members dispatch reaches. One of those bodies calls
/// `kotlin.Any.toString`. The checked graph has to freeze that edge before emission asks for it.
#[test]
fn int_range_initialization_freezes_any_tostring() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let any = libraries.classifier(type_name("kotlin/Any")).expect("Any");
    let any_to_string = libraries
        .external_callable(
            any.declared_callables["toString"].functions()[0]
                .callable
                .external_identity
                .unwrap(),
        )
        .expect("Any.toString realization");
    let any_to_string = any_to_string
        .klib_body_callable()
        .expect("Any.toString body")
        .identity()
        .clone();
    let range = libraries
        .classifier(type_name("kotlin/ranges/IntRange"))
        .expect("IntRange");
    let constructor = range
        .constructors
        .iter()
        .find(|constructor| constructor.params.is_empty())
        .or_else(|| range.constructors.first())
        .expect("IntRange constructor");
    let constructor = libraries
        .external_callable(constructor.external_identity.expect("constructor identity"))
        .expect("IntRange constructor realization");
    let selected = constructor
        .klib_body_callable()
        .expect("IntRange constructor body");
    let facts = crate::resolve::for_checked_klib_bodies(
        &libraries,
        libraries.declaration_bodies(),
        [selected],
    )
    .expect("IntRange initialization has checked member edges");
    let edge = facts
        .member_edges()
        .get(&any_to_string)
        .expect("kotlin.Any.toString is reached from IntRange's initialization");
    assert_eq!(
        edge.semantic_role,
        Some(crate::types::SemanticCallRole::KotlinAnyToString)
    );
}

/// `throwOnFailure` reads `Result.Failure.exception`. Emission lowers that accessor with the class
/// `Failure`'s primary constructor materializes, and closes that initialization with it, so the
/// checked graph reaches the constructor and the `kotlin.Any.hashCode` that `Failure.hashCode`
/// calls.
#[test]
fn a_reached_member_freezes_its_class_initialization() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let any = libraries.classifier(type_name("kotlin/Any")).expect("Any");
    let any_hash_code = libraries
        .external_callable(
            any.declared_callables["hashCode"].functions()[0]
                .callable
                .external_identity
                .unwrap(),
        )
        .expect("Any.hashCode realization")
        .klib_body_callable()
        .expect("Any.hashCode body")
        .identity()
        .clone();
    let symbols = libraries.symbols(
        SymbolNamespace::Package(type_name("kotlin")),
        "throwOnFailure",
    );
    let [throw_on_failure] = symbols.callables.functions() else {
        panic!("the stdlib's one throwOnFailure");
    };
    let throw_on_failure = libraries
        .external_callable(throw_on_failure.callable.external_identity.unwrap())
        .expect("throwOnFailure realization");
    let failure = libraries
        .classifier(type_name("kotlin/Result.Failure"))
        .expect("Result.Failure");
    let [constructor] = failure.constructors.as_slice() else {
        panic!("Failure's one primary constructor");
    };
    let constructor = libraries
        .external_callable(constructor.external_identity.expect("constructor identity"))
        .expect("Failure constructor realization")
        .klib_body_callable()
        .expect("Failure constructor body")
        .identity()
        .clone();
    let facts = crate::resolve::for_checked_klib_bodies(
        &libraries,
        libraries.declaration_bodies(),
        [throw_on_failure
            .klib_body_callable()
            .expect("throwOnFailure body")],
    )
    .expect("throwOnFailure has checked member edges");
    assert!(facts.reached_constructors.contains(&constructor));
    let edge = facts
        .member_edges
        .get(&any_hash_code)
        .expect("kotlin.Any.hashCode is reached from Failure's initialization");
    assert_eq!(
        edge.semantic_role,
        Some(crate::types::SemanticCallRole::KotlinAnyHashCode)
    );
}
