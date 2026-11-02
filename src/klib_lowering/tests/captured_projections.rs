//! Calls kotlinc types through a captured projection: a receiver typed `Array<out T>` handed to a
//! member or generic function declared over `Array<T>`, on the Kotlin/Native stdlib bodies.

use super::super::*;
use super::demo_package::{demo, T};
use super::{distribution_root, frozen_function, stdlib, validated};
use crate::assignable::TypeOracle;
use crate::ir::IrExpr;
use crate::klib_libraries::KlibLibraries;
use crate::symbol_source::{SymbolNamespace, SymbolSource};
use crate::types::{type_name, Ty, TypeName, TypeVariance};

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
    let lowered = unit
        .lower_function_with_intrinsics_and_externals(
            root,
            libraries.declaration_bodies(),
            &facts,
            crate::klib_libraries::KlibIntrinsicFacts::empty(),
            &leaves,
        )
        .unwrap_or_else(|decline| panic!("{decline}"));
    validated(unit.ir());
    // `iterator()` on the `Array<out T>` receiver answers `Iterator<out T>`, which kotlinc writes
    // as the `Iterator<T>` it equals; the call is typed as serialized, over the root's own `T`.
    let ir = unit.ir();
    let [receiver, ..] = ir.functions[lowered as usize].params.as_slice() else {
        panic!("`toCollection` takes its receiver first")
    };
    let [element] = receiver.type_args() else {
        panic!("an array of one element type: {receiver:?}")
    };
    let root_t = element.projection_read_ty();
    assert!(matches!(root_t.kind(), crate::types::TyKind::TyParam(..)));
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
    assert_eq!(
        iterators,
        [Ty::obj_args("kotlin/collections/Iterator", &[root_t])]
    );
}

/// A class model in which `String` is an `Any` and `List` declares its parameter `out`, so
/// `List<String>` is a subtype of `List<Any?>`.
struct CovariantLists;

impl TypeOracle for CovariantLists {
    fn direct_supertypes(&self, ty: Ty) -> Vec<Ty> {
        if ty == Ty::String {
            vec![Ty::obj("kotlin/Any")]
        } else {
            Vec::new()
        }
    }

    fn type_param_variance(&self, owner: TypeName, _: usize) -> TypeVariance {
        if owner == type_name("kotlin/collections/List") {
            TypeVariance::Out
        } else {
            TypeVariance::Invariant
        }
    }
}

#[test]
fn a_call_typed_as_a_wider_application_of_its_result_declines() {
    // No projection is captured: `names(a)` answers `List<String>`, and the body types the call
    // as the wider `List<Any?>`, which is a supertype but not the callee's result.
    let demo = demo(
        &[
            ("names", &[T::ListString], T::ListString),
            ("f", &[T::ListString], T::ListNullableAny),
        ],
        |name, body| {
            let a = body.param(0);
            let value = match name {
                "f" => body.call("names", vec![a], T::ListNullableAny),
                _ => a,
            };
            Some(vec![body.ret(value)])
        },
    );
    let classes = CovariantLists;
    assert!(super::super::type_evidence::satisfies(
        Some(&classes),
        &super::super::klib_types::KlibTypeParameters::default(),
        T::ListString.semantic(),
        T::ListNullableAny.semantic(),
    ));
    let facts = demo.callees(&["f", "names"]).with_type_oracle(&classes);
    let mut unit = DependencyBodyUnit::default();
    let decline = unit
        .lower_function(demo.callable("f"), &demo.bodies, &facts)
        .expect_err("the body declines");
    super::demo_package::assert_unit_is_empty(&unit);
    assert_eq!(
        decline.to_string(),
        "the KLIB body of `demo.f` \
         (its serialized declaration disagrees with the selected one: \
         a call is typed apart from its callee's result)"
    );
}
