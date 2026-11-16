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
        &[],
    )
    .expect("IntRange initialization has checked member edges");
    let edge = facts
        .member_edges
        .get(&any_to_string)
        .expect("kotlin.Any.toString is reached from IntRange's initialization");
    assert_eq!(
        edge.semantic_role,
        Some(crate::types::SemanticCallRole::KotlinAnyToString)
    );
}
