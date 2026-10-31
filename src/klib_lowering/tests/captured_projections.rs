//! Calls kotlinc types through a captured projection: a receiver typed `Array<out T>` handed to a
//! member or generic function declared over `Array<T>`, on the Kotlin/Native stdlib bodies.

use super::super::*;
use super::demo_package::{demo, source_body, Demo, T};
use super::{distribution_root, frozen_function, lowered_body, stdlib, validated};
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

/// The provider realization of the `kotlin.collections` function `name` over an object array,
/// or with no receiver, that takes `arity` value parameters.
fn collections_function(
    libraries: &KlibLibraries,
    name: &str,
    over_array: bool,
    arity: usize,
) -> crate::libraries::ExternalCallableRealization {
    let symbols = libraries.symbols(
        SymbolNamespace::Package(type_name("kotlin/collections")),
        name,
    );
    let matching = symbols
        .callables
        .functions()
        .iter()
        .filter(|function| {
            function
                .semantic_receiver()
                .map(|receiver| receiver.kotlin_class_internal())
                == over_array.then_some(Some(type_name("kotlin/Array")))
                && function.semantic_params().len() == arity
                && function.callable.vararg_index.is_none()
                && function.semantic_params().iter().all(|param| {
                    // `listOf(element)`, not the `listOf(vararg elements)` array parameter.
                    matches!(param.kind(), crate::types::TyKind::TyParam(..))
                })
        })
        .collect::<Vec<_>>();
    let [function] = matching.as_slice() else {
        panic!("one `{name}`, found {}", matching.len())
    };
    libraries
        .external_callable(function.callable.external_identity.unwrap())
        .unwrap()
}

#[test]
fn stdlib_array_copy_binds_its_captured_type_argument() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let to_list = collections_function(&libraries, "toList", true, 0);
    let root = to_list.klib_body_callable().expect("signed");
    let providers = [
        collections_function(&libraries, "copyOf", true, 0),
        collections_function(&libraries, "asList", true, 0),
        collections_function(&libraries, "emptyList", false, 0),
        collections_function(&libraries, "listOf", false, 1),
    ];
    let array = libraries.classifier(type_name("kotlin/Array")).unwrap();
    let size = libraries
        .external_callable(
            array.declared_callables["size"].properties()[0]
                .getter
                .external_identity
                .unwrap(),
        )
        .unwrap();
    let size = size.klib_body_callable().unwrap();
    let get = member(&libraries, "kotlin/Array", "get");
    let get = get.klib_body_callable().unwrap();
    let leaves = providers
        .each_ref()
        .map(|provider| provider.klib_body_callable().unwrap());
    let externals = leaves
        .iter()
        .enumerate()
        .map(|(index, leaf)| {
            (
                leaf.identity().clone(),
                super::super::EmittedDependencyCallable::new(
                    crate::fir::ExternalCallableId::from_raw(80 + index as u32),
                    None,
                    None,
                ),
            )
        })
        .chain([
            (
                size.identity().clone(),
                super::super::EmittedDependencyCallable::target_member(
                    crate::fir::ExternalCallableId::from_raw(90),
                ),
            ),
            (
                get.identity().clone(),
                super::super::EmittedDependencyCallable::target_member(
                    crate::fir::ExternalCallableId::from_raw(91),
                ),
            ),
        ])
        .collect();
    let oracle = crate::symbol_resolver::SourceOracle(&libraries);
    let facts = KlibCalleeFacts::with_declarations([root], leaves.into_iter().chain([size, get]))
        .with_type_oracle(&oracle);
    let mut unit = DependencyBodyUnit::default();
    let lowered = unit
        .lower_function_with_intrinsics_and_externals(
            root,
            libraries.declaration_bodies(),
            &facts,
            crate::klib_libraries::KlibIntrinsicFacts::empty(),
            &externals,
        )
        .unwrap_or_else(|decline| panic!("{decline}"));
    validated(unit.ir());
    // `copyOf()` on the `Array<out T>` receiver captures the projection: kotlinc serializes the
    // type argument approximated to `T`, and the call binds `out T` and answers `Array<out T>`, as
    // checked FIR lowering records `x.copyOf()` on an `x: Array<out T>`.
    let ir = unit.ir();
    let [receiver] = ir.functions[lowered as usize].params.as_slice() else {
        panic!("`toList` takes its receiver alone")
    };
    let [projected] = receiver.type_args() else {
        panic!("an array of one element type: {receiver:?}")
    };
    assert!(matches!(
        projected.kind(),
        crate::types::TyKind::OutProjection(..)
    ));
    let copies = ir
        .exprs
        .iter()
        .enumerate()
        .filter_map(|(index, expression)| match expression {
            IrExpr::Call { callee, .. } => match &**callee {
                crate::ir::Callee::External {
                    target,
                    ret,
                    substitutions,
                    ..
                } if target.raw() == 80 => Some((
                    *ret,
                    substitutions
                        .iter()
                        .map(|substitution| substitution.value)
                        .collect::<Vec<_>>(),
                    ir.checked_type(index as u32),
                )),
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>();
    let array = Ty::obj_args("kotlin/Array", &[*projected]);
    assert_eq!(copies, [(array, vec![*projected], Some(array))]);
}

/// A class model in which `String` is a `CharSequence`, `Int` and `CharSequence` are `Any`, and
/// `List` declares its parameter `out`, so `List<String>` is a subtype of `List<Any?>`.
struct CovariantLists;

impl TypeOracle for CovariantLists {
    fn direct_supertypes(&self, ty: Ty) -> Vec<Ty> {
        let any = T::Any.semantic();
        if ty == T::String.semantic() {
            vec![T::CharSequence.semantic(), any]
        } else if ty == T::Int.semantic() || ty == T::CharSequence.semantic() {
            vec![any]
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

/// `exchange(a: Array<P>, b: Array<P>)` and `first(a: Array<P>, b: Int)`, generic in `P`, with
/// `f(a: Array<out String>, b: Array<out Any?>)` calling `exchange(a, b)` at `P = Any?` and
/// `g(a: Array<out String>, b: Int)` calling `first(a, b)` at `P = String`.
fn two_input_calls() -> Demo {
    demo(
        &[
            ("exchange", &[T::ArrayP, T::ArrayP], T::Unit),
            ("first", &[T::ArrayP, T::Int], T::Unit),
            ("f", &[T::ArrayOutString, T::ArrayOutNullableAny], T::Unit),
            ("g", &[T::ArrayOutString, T::Int], T::Unit),
        ],
        |name, body| {
            let call = match name {
                "f" => {
                    let (a, b) = (body.param(0), body.param(1));
                    body.generic_call("exchange", vec![a, b], T::NullableAny, T::Unit)
                }
                "g" => {
                    let (a, b) = (body.param(0), body.param(1));
                    body.generic_call("first", vec![a, b], T::String, T::Unit)
                }
                _ => return Some(Vec::new()),
            };
            Some(vec![
                crate::metadata::klib_ir::tree::KlibIrStatement::Expression(call),
            ])
        },
    )
}

const TWO_INPUT_SOURCE: &str = "fun <P> first(a: Array<P>, b: Int) {\n}\n";

/// Lower `name` of [`two_input_calls`] into `unit` with its callee, in the class model where
/// `String` is an `Any` and `Array` is invariant.
fn lower_two_input(
    demo: &Demo,
    unit: &mut DependencyBodyUnit,
    name: &str,
    callee: &str,
) -> Result<crate::ir::FunId, KlibBodyDecline> {
    let facts = demo
        .callees(&[name, callee])
        .with_type_oracle(&CovariantLists);
    unit.lower_function(demo.callable(name), &demo.bodies, &facts)
}

#[test]
fn a_capture_from_one_of_several_operands_binds_as_the_source_call_does() {
    // kotlinc accepts `first(a, b)` on an `a: Array<out String>`: `P` is captured from `a` alone.
    let demo = two_input_calls();
    let mut unit = DependencyBodyUnit::default();
    let g = lower_two_input(&demo, &mut unit, "g", "first")
        .unwrap_or_else(|decline| panic!("{decline}"));
    validated(unit.ir());
    let source =
        format!("{TWO_INPUT_SOURCE}fun g(a: Array<out String>, b: Int) {{\n first(a, b)\n}}\n");
    assert_eq!(lowered_body(&unit, g), source_body(&source, "g", 2));
}

#[test]
fn a_capture_another_operand_also_constrains_declines() {
    // kotlinc rejects `exchange(a, b)` (and `exchange(a, a)`): each projected argument is
    // captured apart, so no single `P` admits both. The serialized call cannot be the one checked
    // FIR records, so the body declines and publishes nothing.
    let demo = two_input_calls();
    let mut unit = DependencyBodyUnit::default();
    let decline =
        lower_two_input(&demo, &mut unit, "f", "exchange").expect_err("the body declines");
    super::demo_package::assert_unit_is_empty(&unit);
    assert_eq!(
        decline.to_string(),
        "the KLIB body of `demo.f` \
         (its serialized declaration disagrees with the selected one: \
         a call captures a projection its other operands or bounds also constrain)"
    );
    // The declined lowering leaves no expression behind: the next root numbers its expressions
    // exactly as in an empty unit.
    let g = lower_two_input(&demo, &mut unit, "g", "first")
        .unwrap_or_else(|decline| panic!("{decline}"));
    let mut fresh = DependencyBodyUnit::default();
    let fresh_g = lower_two_input(&demo, &mut fresh, "g", "first")
        .unwrap_or_else(|decline| panic!("{decline}"));
    assert_eq!(g, fresh_g);
    assert_eq!(unit.ir().exprs.len(), fresh.ir().exprs.len());
    assert_eq!(lowered_body(&unit, g), lowered_body(&fresh, fresh_g));
}

/// `take(a: Array<Q>)`, generic in `Q : Any, Q : CharSequence`, with `h(a: Array<out Int>)`
/// calling `take(a)` at `Q = Int` and `k(a: Array<out String>)` calling it at `Q = String`.
fn bounded_calls() -> Demo {
    demo(
        &[
            ("take", &[T::ArrayQ], T::Unit),
            ("h", &[T::ArrayOutInt], T::Unit),
            ("k", &[T::ArrayOutString], T::Unit),
        ],
        |name, body| {
            let type_argument = match name {
                "h" => T::Int,
                "k" => T::String,
                _ => return Some(Vec::new()),
            };
            let a = body.param(0);
            let call = body.generic_call("take", vec![a], type_argument, T::Unit);
            Some(vec![
                crate::metadata::klib_ir::tree::KlibIrStatement::Expression(call),
            ])
        },
    )
}

const BOUNDED_SOURCE: &str = "fun <Q> take(a: Array<Q>) where Q : Any, Q : CharSequence {\n}\n";

#[test]
fn a_capture_every_declared_bound_admits_binds_as_the_source_call_does() {
    // kotlinc accepts `take(a)` on an `a: Array<out String>`: the captured `String` is both an
    // `Any` and a `CharSequence`.
    let demo = bounded_calls();
    let mut unit = DependencyBodyUnit::default();
    let k = lower_two_input(&demo, &mut unit, "k", "take")
        .unwrap_or_else(|decline| panic!("{decline}"));
    validated(unit.ir());
    let source = format!("{BOUNDED_SOURCE}fun k(a: Array<out String>) {{\n take(a)\n}}\n");
    assert_eq!(
        lowered_body(&unit, k),
        super::demo_package::source_body_with_stdlib(&source, "k", 1)
    );
}

#[test]
fn a_capture_a_secondary_bound_rejects_declines() {
    // kotlinc rejects `take(a)` on an `a: Array<out Int>` (an `Int` is not a `CharSequence`)
    // although the first bound `Any` admits it and the serialized header matches the selected
    // declaration. The call is not one checked FIR records, so the body declines and publishes
    // nothing.
    let demo = bounded_calls();
    let mut unit = DependencyBodyUnit::default();
    let decline = lower_two_input(&demo, &mut unit, "h", "take").expect_err("the body declines");
    super::demo_package::assert_unit_is_empty(&unit);
    assert_eq!(
        decline.to_string(),
        "the KLIB body of `demo.h` \
         (its serialized declaration disagrees with the selected one: \
         a call captures a projection its other operands or bounds also constrain)"
    );
    let k = lower_two_input(&demo, &mut unit, "k", "take")
        .unwrap_or_else(|decline| panic!("{decline}"));
    let mut fresh = DependencyBodyUnit::default();
    let fresh_k = lower_two_input(&demo, &mut fresh, "k", "take")
        .unwrap_or_else(|decline| panic!("{decline}"));
    assert_eq!(k, fresh_k);
    assert_eq!(unit.ir().exprs.len(), fresh.ir().exprs.len());
    assert_eq!(lowered_body(&unit, k), lowered_body(&fresh, fresh_k));
}

/// `both(a: Array<P>)`, generic in `P` and an unused `R : P`, with `h(a: Array<out String>)`
/// calling `both<_, String>(a)`; and `pick(a: Array<P>)`, generic in `P` and an unused,
/// independent `S : CharSequence`, with `k(a: Array<out String>)` calling `pick<_, String>(a)`.
fn peer_calls() -> Demo {
    demo(
        &[
            ("both", &[T::ArrayPBoundingR], T::Unit),
            ("h", &[T::ArrayOutString], T::Unit),
            ("pick", &[T::ArrayPBesideS], T::Unit),
            ("k", &[T::ArrayOutString], T::Unit),
        ],
        |name, body| {
            let callee = match name {
                "h" => "both",
                "k" => "pick",
                _ => return Some(Vec::new()),
            };
            let a = body.param(0);
            let call = body.generic_call_with(callee, vec![a], &[T::String, T::String], T::Unit);
            Some(vec![
                crate::metadata::klib_ir::tree::KlibIrStatement::Expression(call),
            ])
        },
    )
}

#[test]
fn a_capture_beside_an_independent_peer_binds_as_the_source_call_does() {
    // kotlinc accepts `pick<_, String>(a)` on an `a: Array<out String>`: `S : CharSequence` does
    // not name `P`, so `P` is captured from `a` alone.
    let demo = peer_calls();
    let mut unit = DependencyBodyUnit::default();
    let k = lower_two_input(&demo, &mut unit, "k", "pick")
        .unwrap_or_else(|decline| panic!("{decline}"));
    validated(unit.ir());
    let source = "fun <P, S : CharSequence> pick(a: Array<P>) {\n}\n\
                  fun k(a: Array<out String>) {\n pick<_, String>(a)\n}\n";
    assert_eq!(
        lowered_body(&unit, k),
        super::demo_package::source_body_with_stdlib(source, "k", 1)
    );
}

#[test]
fn a_capture_an_unused_peer_bound_names_declines() {
    // kotlinc rejects `both<_, String>(a)` on an `a: Array<out String>`: the unused `R : P`
    // constrains `P` too, and `String` is no subtype of the captured `out String`. No operand,
    // receiver or bound of `P` names `R`, so only `R`'s own bound reveals the constraint. The
    // body declines and publishes nothing.
    let demo = peer_calls();
    let mut unit = DependencyBodyUnit::default();
    let decline = lower_two_input(&demo, &mut unit, "h", "both").expect_err("the body declines");
    super::demo_package::assert_unit_is_empty(&unit);
    assert_eq!(
        decline.to_string(),
        "the KLIB body of `demo.h` \
         (its serialized declaration disagrees with the selected one: \
         a call captures a projection its other operands or bounds also constrain)"
    );
    let k = lower_two_input(&demo, &mut unit, "k", "pick")
        .unwrap_or_else(|decline| panic!("{decline}"));
    let mut fresh = DependencyBodyUnit::default();
    let fresh_k = lower_two_input(&demo, &mut fresh, "k", "pick")
        .unwrap_or_else(|decline| panic!("{decline}"));
    assert_eq!(k, fresh_k);
    assert_eq!(unit.ir().exprs.len(), fresh.ir().exprs.len());
    assert_eq!(lowered_body(&unit, k), lowered_body(&fresh, fresh_k));
}
