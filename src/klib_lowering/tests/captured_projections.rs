//! Calls kotlinc types through a captured projection: a receiver typed `Array<out T>` handed to a
//! member or generic function declared over `Array<T>`, on the Kotlin/Native stdlib bodies.

use super::super::*;
use super::{distribution_root, frozen_function, stdlib, validated};
use crate::ir::IrExpr;
use crate::klib_libraries::KlibLibraries;
use crate::symbol_source::{SymbolNamespace, SymbolSource};
use crate::types::Ty;

/// The provider realization of the member `name` of `classifier`, which the libraries declare
/// once.
fn member(
    libraries: &KlibLibraries,
    classifier: &str,
    name: &str,
) -> crate::libraries::ExternalCallableRealization {
    let class = libraries
        .classifier(crate::types::type_name(classifier))
        .unwrap();
    let [function] = class.declared_callables[name].functions() else {
        panic!("one `{classifier}.{name}`")
    };
    libraries
        .external_callable(function.callable.external_identity.unwrap())
        .unwrap()
}

#[test]
fn stdlib_array_walk_reads_its_captured_iterator() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let symbols = libraries.symbols(
        SymbolNamespace::Package(crate::types::type_name("kotlin/collections")),
        "toCollection",
    );
    let helper = symbols
        .callables
        .functions()
        .iter()
        .find(|function| {
            function.semantic_receiver().is_some_and(|receiver| {
                receiver.kotlin_class_internal() == Some(crate::types::type_name("kotlin/Array"))
            })
        })
        .expect("`toCollection` over an object array");
    let fact = frozen_function(
        &libraries,
        "kotlin/collections",
        "toCollection",
        helper.semantic_receiver(),
        helper.semantic_params().as_ref(),
    );
    let root = fact.klib_body_callable().expect("signed");
    let providers = [
        member(&libraries, "kotlin/Array", "iterator"),
        member(&libraries, "kotlin/collections/Iterator", "hasNext"),
        member(&libraries, "kotlin/collections/Iterator", "next"),
        member(&libraries, "kotlin/collections/MutableCollection", "add"),
    ];
    let [iterator, has_next, next, add] = providers
        .each_ref()
        .map(|provider| provider.klib_body_callable().unwrap());
    let leaves = [
        (
            iterator.identity().clone(),
            super::super::EmittedDependencyCallable::target_member(
                crate::fir::ExternalCallableId::from_raw(70),
            ),
        ),
        (
            has_next.identity().clone(),
            super::super::EmittedDependencyCallable::new(
                crate::fir::ExternalCallableId::from_raw(71),
                None,
                None,
            ),
        ),
        (
            next.identity().clone(),
            super::super::EmittedDependencyCallable::new(
                crate::fir::ExternalCallableId::from_raw(72),
                None,
                None,
            ),
        ),
        (
            add.identity().clone(),
            super::super::EmittedDependencyCallable::new(
                crate::fir::ExternalCallableId::from_raw(73),
                None,
                None,
            ),
        ),
    ]
    .into_iter()
    .collect();
    let oracle = crate::symbol_resolver::SourceOracle(&libraries);
    let facts = KlibCalleeFacts::with_declarations([root], [iterator, has_next, next, add])
        .with_type_oracle(&oracle);
    let mut unit = DependencyBodyUnit::default();
    unit.lower_function_with_intrinsics_and_externals(
        root,
        libraries.declaration_bodies(),
        &facts,
        crate::klib_libraries::KlibIntrinsicFacts::empty(),
        &leaves,
    )
    .unwrap_or_else(|decline| panic!("{decline}"));
    validated(unit.ir());
    // `iterator()` on the `Array<out T>` receiver answers `Iterator<out T>`, which kotlinc writes
    // as the `Iterator<T>` it equals; the call is typed as serialized.
    let ir = unit.ir();
    let iterators = ir
        .exprs
        .iter()
        .enumerate()
        .filter_map(|(index, expression)| match expression {
            IrExpr::Call { callee, .. }
                if matches!(
                    &**callee,
                    crate::ir::Callee::External { target, .. } if target.raw() == 70
                ) =>
            {
                ir.checked_type(index as u32)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let [iterator] = iterators.as_slice() else {
        panic!("one `iterator()` call: {iterators:?}")
    };
    assert_eq!(
        iterator.kotlin_class_internal(),
        Some(crate::types::type_name("kotlin/collections/Iterator"))
    );
    let [element] = iterator.type_args() else {
        panic!("an iterator of one element type: {iterator:?}")
    };
    assert!(matches!(element.kind(), crate::types::TyKind::TyParam(..)));
    let _: Ty = *element;
}
