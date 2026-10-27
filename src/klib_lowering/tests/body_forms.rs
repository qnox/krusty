//! Lowering of the KLIB body forms beyond a single relation: calls of dependency functions, the
//! equality and negation built-ins, local variables, no-op implicit casts and multi-branch
//! `when`s. Each lowered body is compared with checked FIR lowering of the equivalent source, on
//! hand-built trees and on the Kotlin/Native stdlib bodies that use the form.

use crate::fx_hash::HashMap;

use super::super::*;
use super::demo_package::*;
use super::{distribution_root, frozen_function, kotlin_class, lowered_body, stdlib, validated};
use crate::klib_libraries::{KlibBodyLibrary, KlibDeclarationBodies};
use crate::metadata::klib_ir::tree::{
    KlibIrArena, KlibIrArguments, KlibIrExprId, KlibIrExprKind, KlibIrStatement, KlibIrTypeOperator,
};
use crate::metadata::klib_ir::{
    read_declaration_trees, KlibIrConstant, KlibIrModuleTrees, KlibIrSignature,
};
use crate::metadata::semantic::semantic_ty;
use crate::types::Ty;

// --- Calls --------------------------------------------------------------------------------------

pub(super) const IDENTITY_AND_CALLER: [Shape; 2] =
    [("b", &[T::Int], T::Int), ("a", &[T::Int], T::Int)];

pub(super) const IDENTITY_AND_CALLER_SOURCE: &str = "fun b(a: Int): Int {\n return a\n}\n\
     fun a(a: Int): Int {\n return b(a)\n}\n";

pub(super) fn identity_and_caller(name: &str, body: &mut Body) -> Option<Vec<KlibIrStatement>> {
    let a = body.param(0);
    let value = match name {
        "a" => body.call("b", vec![a], T::Int),
        _ => a,
    };
    Some(vec![body.ret(value)])
}

#[test]
fn a_call_of_a_dependency_function_lowers_its_callee_into_the_unit() {
    let demo = demo(&IDENTITY_AND_CALLER, identity_and_caller);
    let mut unit = DependencyBodyUnit::default();
    let a = demo.lower(&mut unit, "a", &["a", "b"]).expect("lowers");
    validated(unit.ir());
    assert_eq!(unit.ir().functions.len(), 2);
    assert_eq!(
        lowered_body(&unit, a),
        source_body(IDENTITY_AND_CALLER_SOURCE, "a", 1)
    );
    assert_eq!(
        lowered_body(&unit, a),
        "scope {scope {return@Some(0) call b(v0!: Int): Int: Nothing}: Nothing}"
    );
    // The callee was lowered once, under its own signature.
    let expressions = unit.ir().exprs.len();
    let b = demo.lower(&mut unit, "b", &[]).expect("already lowered");
    assert_eq!(unit.ir().exprs.len(), expressions);
    assert_eq!(
        lowered_body(&unit, b),
        source_body(IDENTITY_AND_CALLER_SOURCE, "b", 1)
    );
    let calls = unit
        .ir()
        .exprs
        .iter()
        .filter_map(|expression| match expression {
            crate::ir::IrExpr::Call {
                callee: call_callee,
                ..
            } if let crate::ir::Callee::Local(callee) = &**call_callee => Some(*callee),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(calls, [b]);
}

#[test]
fn overlapping_roots_share_their_transitive_callee_when_selected_in_reverse_order() {
    let demo = demo(&IDENTITY_AND_CALLER, identity_and_caller);
    let mut unit = DependencyBodyUnit::default();
    let b = demo
        .lower(&mut unit, "b", &["a", "b"])
        .expect("the callee root lowers first");
    let expressions_after_b = unit.ir().exprs.len();
    let a = demo
        .lower(&mut unit, "a", &["a", "b"])
        .expect("the caller root reuses its callee");
    validated(unit.ir());
    assert_eq!(unit.ir().functions.len(), 2);
    assert!(unit.ir().exprs.len() > expressions_after_b);
    let calls = unit
        .ir()
        .exprs
        .iter()
        .filter_map(|expression| match expression {
            crate::ir::IrExpr::Call {
                callee: call_callee,
                ..
            } if let crate::ir::Callee::Local(callee) = &**call_callee => Some(*callee),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(calls, [b]);
    assert_ne!(a, b);
}

#[test]
fn a_later_support_unit_links_an_exact_callee_emitted_by_an_earlier_unit() {
    let demo = demo(&IDENTITY_AND_CALLER, identity_and_caller);
    let b = demo.callable("b");
    let external = crate::fir::ExternalCallableId::from_raw(17);
    let emitted = [(
        b.identity().clone(),
        super::super::EmittedDependencyCallable::new(external, None, None),
    )]
    .into_iter()
    .collect();
    let callees = demo.callees(&["a", "b"]);
    let mut unit = DependencyBodyUnit::default();
    unit.lower_function_with_intrinsics_and_externals(
        demo.callable("a"),
        &demo.bodies,
        &callees,
        crate::klib_libraries::KlibIntrinsicFacts::empty(),
        &emitted,
    )
    .expect("the later root links its already-emitted callee");
    validated(unit.ir());
    assert_eq!(unit.ir().functions.len(), 1, "the callee is not re-emitted");
    let calls = unit
        .ir()
        .exprs
        .iter()
        .filter_map(|expression| match expression {
            crate::ir::IrExpr::Call {
                callee: call_callee,
                ..
            } if let crate::ir::Callee::External { target, .. } = &**call_callee => Some(*target),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(calls, [external]);
}

#[test]
fn a_call_cycle_links_back_to_the_declared_function() {
    let shapes: [Shape; 2] = [("a", &[T::Int], T::Int), ("b", &[T::Int], T::Int)];
    let demo = demo(&shapes, |name, body| {
        let (relation_name, callee) = match name {
            "a" => ("less", "b"),
            _ => ("greater", "a"),
        };
        let lhs = body.param(0);
        let zero = body.int(0);
        let condition = body.builtin(relation(relation_name), vec![lhs, zero], None);
        let argument = body.param(0);
        let call = body.call(callee, vec![argument], T::Int);
        let otherwise = body.param(0);
        let conditional = body.when_("IF", vec![(condition, call)], otherwise, T::Int);
        Some(vec![body.ret(conditional)])
    });
    let source = "fun a(a: Int): Int {\n return if (a < 0) b(a) else a\n}\n\
                  fun b(a: Int): Int {\n return if (a > 0) a(a) else a\n}\n";
    let mut unit = DependencyBodyUnit::default();
    let a = demo.lower(&mut unit, "a", &["a", "b"]).expect("lowers");
    let b = demo.lower(&mut unit, "b", &[]).expect("already lowered");
    validated(unit.ir());
    assert_eq!(unit.ir().functions.len(), 2);
    assert_eq!(lowered_body(&unit, a), source_body(source, "a", 1));
    assert_eq!(lowered_body(&unit, b), source_body(source, "b", 1));
}

#[test]
fn a_declining_callee_leaves_the_unit_as_it_was() {
    let shapes: [Shape; 3] = [
        ("a", &[T::Int], T::Int),
        ("b", &[T::Int], T::Int),
        ("c", &[T::Int], T::Int),
    ];
    let demo = demo(&shapes, |name, body| {
        let a = body.param(0);
        let value = match name {
            "a" => body.call("b", vec![a], T::Int),
            "b" => body.type_operator(KlibIrTypeOperator::SafeCast, a, T::Int),
            _ => a,
        };
        Some(vec![body.ret(value)])
    });
    let mut unit = DependencyBodyUnit::default();
    let c = demo.lower(&mut unit, "c", &[]).expect("lowers");
    let before = lowered_body(&unit, c);
    let expressions = unit.ir().exprs.len();
    let decline = demo
        .lower(&mut unit, "a", &["b"])
        .expect_err("the callee declines");
    assert_eq!(
        decline.to_string(),
        "the KLIB body of `demo.a` (it reaches the KLIB body of `demo.b` (it uses a safe cast))"
    );
    assert_eq!(unit.ir().functions.len(), 1);
    assert_eq!(unit.ir().exprs.len(), expressions);
    assert_eq!(lowered_body(&unit, c), before);
    validated(unit.ir());
    // Neither the caller nor the callee is remembered: lowering the callee alone declines by its
    // own form.
    assert_eq!(
        demo.lower(&mut unit, "b", &[])
            .expect_err("it casts safely")
            .to_string(),
        "the KLIB body of `demo.b` (it uses a safe cast)"
    );
}

#[test]
fn a_callee_without_a_body_declines_by_its_own_reason() {
    let demo = demo(&IDENTITY_AND_CALLER, |name, body| match name {
        "a" => identity_and_caller(name, body),
        _ => None,
    });
    assert_eq!(
        demo.declined("a", &["b"]),
        "the KLIB body of `demo.a` (it reaches the KLIB body of `demo.b` \
         (its library serializes none))"
    );
}

#[test]
fn a_callee_two_frozen_selections_describe_declines_by_name() {
    let demo = demo(&IDENTITY_AND_CALLER, identity_and_caller);
    let mut conflicting = demo.facts["b"].clone();
    conflicting.name = "different-provider-shape".to_owned();
    let callees = KlibCalleeFacts::new([
        demo.callable("b"),
        conflicting
            .klib_body_callable()
            .expect("the conflicting record retains the exact identity"),
    ]);
    let mut unit = DependencyBodyUnit::default();
    assert_eq!(
        unit.lower_function(demo.callable("a"), &demo.bodies, &callees)
            .expect_err("two incompatible records claim the callee identity")
            .to_string(),
        "the KLIB body of `demo.a` (it calls `demo.b`, which two selected declarations describe)"
    );
    assert_unit_is_empty(&unit);
}

#[test]
fn a_call_of_an_already_lowered_function_follows_the_active_selection() {
    let shapes: [Shape; 3] = [
        ("a", &[T::Int], T::Int),
        ("b", &[T::Int], T::Int),
        ("c", &[T::Int], T::Int),
    ];
    let demo = demo(&shapes, |name, body| {
        let a = body.param(0);
        let value = match name {
            "b" => a,
            _ => body.call("b", vec![a], T::Int),
        };
        Some(vec![body.ret(value)])
    });
    let mut unit = DependencyBodyUnit::default();
    demo.lower(&mut unit, "a", &["b"]).expect("lowers");
    let functions = unit.ir().functions.len();
    let expressions = unit.ir().exprs.len();
    let mut conflicting = demo.facts["b"].clone();
    conflicting.name = "different-provider-shape".to_owned();
    let ambiguous = KlibCalleeFacts::new([
        demo.callable("b"),
        conflicting
            .klib_body_callable()
            .expect("the conflicting record retains the exact identity"),
    ]);
    assert_eq!(
        unit.lower_function(demo.callable("c"), &demo.bodies, &ambiguous)
            .expect_err("`b` is ambiguous for this lowering")
            .to_string(),
        "the KLIB body of `demo.c` (it calls `demo.b`, which two selected declarations describe)"
    );
    assert_eq!(unit.ir().functions.len(), functions);
    assert_eq!(unit.ir().exprs.len(), expressions);
    // With no selection of its own, the call links to the function already in the unit, which its
    // serialized header describes as the earlier selection did.
    let c = demo.lower(&mut unit, "c", &[]).expect("lowers");
    assert_eq!(unit.ir().functions.len(), functions + 1);
    assert_eq!(
        lowered_body(&unit, c),
        "scope {scope {return@Some(0) call b(v0!: Int): Int: Nothing}: Nothing}"
    );
    validated(unit.ir());
}

#[test]
fn a_friend_body_local_call_never_uses_an_equal_signature_fact_from_another_module() {
    let demo = demo_in_module("friend", &IDENTITY_AND_CALLER, identity_and_caller);
    let mut ordinary = demo.facts["b"].clone();
    ordinary
        .dependency_callable
        .as_mut()
        .expect("a named KLIB callable")
        .module = "ordinary".to_owned();
    // Kotlin signatures do not include the return type. An unrelated module may therefore carry
    // this equal serialized signature with a different declaration header.
    ordinary.ret = Ty::String;
    let facts = KlibCalleeFacts::new([ordinary.klib_body_callable().expect("signed")]);
    let mut unit = DependencyBodyUnit::default();
    let root = unit
        .lower_function(demo.callable("a"), &demo.bodies, &facts)
        .expect("the friend-local serialized header, not the ordinary fact, describes the call");

    validated(unit.ir());
    assert_eq!(
        lowered_body(&unit, root),
        "scope {scope {return@Some(0) call b(v0!: Int): Int: Nothing}: Nothing}"
    );
}

#[test]
fn a_body_call_uses_its_producer_dependency_in_both_input_orders() {
    fn lower(unrelated_first: bool) {
        let (caller, caller_trees) =
            selected_demo_module("caller", &IDENTITY_AND_CALLER, &["a"], identity_and_caller);
        let (_, friend_trees) =
            selected_demo_module("friend", &IDENTITY_AND_CALLER, &["b"], identity_and_caller);
        let (mut unrelated, unrelated_trees) = selected_demo_module(
            "unrelated",
            &IDENTITY_AND_CALLER,
            &["b"],
            identity_and_caller,
        );
        // Public signatures do not include the return type. This deliberately makes selecting
        // the unrelated module observably incompatible with the body it serialized.
        unrelated.get_mut("b").expect("unrelated b").ret = Ty::String;
        let caller_library =
            KlibBodyLibrary::new("caller".to_owned(), ["friend".to_owned()], caller_trees);
        let friend_library = KlibBodyLibrary::new("friend".to_owned(), Vec::new(), friend_trees);
        let unrelated_library =
            KlibBodyLibrary::new("unrelated".to_owned(), Vec::new(), unrelated_trees);
        let libraries = if unrelated_first {
            vec![caller_library, unrelated_library, friend_library]
        } else {
            vec![caller_library, friend_library, unrelated_library]
        };
        let bodies = KlibDeclarationBodies::from_body_libraries(libraries)
            .expect("equal signatures in three distinct modules are legal");
        let root = caller["a"]
            .klib_body_callable()
            .expect("the caller is signed");
        let selected = unrelated["b"]
            .klib_body_callable()
            .expect("the unrelated source selection is signed");

        let mut unit = DependencyBodyUnit::default();
        let root = unit
            .lower_function(root, &bodies, &KlibCalleeFacts::new([selected]))
            .expect("the producer edge authenticates the friend, not the source selection");
        validated(unit.ir());
        assert_eq!(unit.ir().functions.len(), 2);
        assert_eq!(
            lowered_body(&unit, root),
            "scope {scope {return@Some(0) call b(v0!: Int): Int: Nothing}: Nothing}"
        );
    }

    lower(false);
    lower(true);
}

#[test]
fn a_body_call_without_a_producer_edge_never_uses_an_unrelated_source_selection() {
    let (caller, caller_trees) =
        selected_demo_module("caller", &IDENTITY_AND_CALLER, &["a"], identity_and_caller);
    let (unrelated, unrelated_trees) = selected_demo_module(
        "unrelated",
        &IDENTITY_AND_CALLER,
        &["b"],
        identity_and_caller,
    );
    let bodies = KlibDeclarationBodies::from_body_libraries(vec![
        KlibBodyLibrary::new("caller".to_owned(), Vec::new(), caller_trees),
        KlibBodyLibrary::new("unrelated".to_owned(), Vec::new(), unrelated_trees),
    ])
    .expect("two modules");
    let root = caller["a"].klib_body_callable().expect("signed caller");
    let selected = unrelated["b"]
        .klib_body_callable()
        .expect("signed unrelated selection");

    let mut unit = DependencyBodyUnit::default();
    let decline = unit
        .lower_function(root, &bodies, &KlibCalleeFacts::new([selected]))
        .expect_err("an absent producer edge cannot be reconstructed from a source selection");
    assert_unit_is_empty(&unit);
    assert_eq!(
        decline.to_string(),
        "the KLIB body of `demo.a` (it calls `demo.b`, which no reachable library serializes its declaration)"
    );
}

#[test]
fn two_reachable_modules_with_the_same_call_signature_are_ambiguous() {
    let (caller, caller_trees) =
        selected_demo_module("caller", &IDENTITY_AND_CALLER, &["a"], identity_and_caller);
    let (_, first_trees) =
        selected_demo_module("first", &IDENTITY_AND_CALLER, &["b"], identity_and_caller);
    let (_, second_trees) =
        selected_demo_module("second", &IDENTITY_AND_CALLER, &["b"], identity_and_caller);
    let bodies = KlibDeclarationBodies::from_body_libraries(vec![
        KlibBodyLibrary::new(
            "caller".to_owned(),
            ["first".to_owned(), "second".to_owned()],
            caller_trees,
        ),
        KlibBodyLibrary::new("first".to_owned(), Vec::new(), first_trees),
        KlibBodyLibrary::new("second".to_owned(), Vec::new(), second_trees),
    ])
    .expect("three modules");
    let root = caller["a"].klib_body_callable().expect("signed caller");

    let mut unit = DependencyBodyUnit::default();
    let decline = unit
        .lower_function(root, &bodies, &KlibCalleeFacts::default())
        .expect_err("the producer graph has two declaring modules");
    assert_unit_is_empty(&unit);
    assert_eq!(
        decline.to_string(),
        "the KLIB body of `demo.a` (it calls `demo.b`, whose declaration multiple producer dependencies serialize)"
    );
}

#[test]
fn a_local_body_declaration_wins_over_an_incompatible_dependency_duplicate() {
    let (caller, caller_trees) = selected_demo_module(
        "caller",
        &IDENTITY_AND_CALLER,
        &["a", "b"],
        identity_and_caller,
    );
    let dependency_shape: [Shape; 1] = [("b", &[T::Int], T::String)];
    let (dependency, dependency_trees) =
        selected_demo_module("dependency", &dependency_shape, &["b"], identity_and_caller);
    assert_eq!(
        caller["b"].declaration_signature, dependency["b"].declaration_signature,
        "return types do not distinguish public call signatures"
    );
    let bodies = KlibDeclarationBodies::from_body_libraries(vec![
        KlibBodyLibrary::new("caller".to_owned(), ["dependency".to_owned()], caller_trees),
        KlibBodyLibrary::new("dependency".to_owned(), Vec::new(), dependency_trees),
    ])
    .expect("two modules");
    let root = caller["a"].klib_body_callable().expect("signed caller");

    let mut unit = DependencyBodyUnit::default();
    unit.lower_function(root, &bodies, &KlibCalleeFacts::default())
        .expect("the caller-local declaration wins");
    validated(unit.ir());
    assert_eq!(unit.ir().functions.len(), 2);
}

#[test]
fn duplicate_producer_edges_still_select_one_dependency_origin() {
    let (caller, caller_trees) =
        selected_demo_module("caller", &IDENTITY_AND_CALLER, &["a"], identity_and_caller);
    let (_, dependency_trees) = selected_demo_module(
        "dependency",
        &IDENTITY_AND_CALLER,
        &["b"],
        identity_and_caller,
    );
    let bodies = KlibDeclarationBodies::from_body_libraries(vec![
        KlibBodyLibrary::new(
            "caller".to_owned(),
            ["dependency".to_owned(), "dependency".to_owned()],
            caller_trees,
        ),
        KlibBodyLibrary::new("dependency".to_owned(), Vec::new(), dependency_trees),
    ])
    .expect("two modules");
    let root = caller["a"].klib_body_callable().expect("signed caller");

    let mut unit = DependencyBodyUnit::default();
    unit.lower_function(root, &bodies, &KlibCalleeFacts::default())
        .expect("duplicate edges name one exact dependency");
    validated(unit.ir());
    assert_eq!(unit.ir().functions.len(), 2);
}

#[test]
fn a_body_call_reaches_a_transitive_producer_dependency() {
    let (caller, caller_trees) =
        selected_demo_module("caller", &IDENTITY_AND_CALLER, &["a"], identity_and_caller);
    let (_, dependency_trees) = selected_demo_module(
        "dependency",
        &IDENTITY_AND_CALLER,
        &["b"],
        identity_and_caller,
    );
    let bodies = KlibDeclarationBodies::from_body_libraries(vec![
        KlibBodyLibrary::new("caller".to_owned(), ["bridge".to_owned()], caller_trees),
        KlibBodyLibrary::new(
            "bridge".to_owned(),
            ["dependency".to_owned()],
            KlibIrModuleTrees::default(),
        ),
        KlibBodyLibrary::new("dependency".to_owned(), Vec::new(), dependency_trees),
    ])
    .expect("three modules");
    let root = caller["a"].klib_body_callable().expect("signed caller");

    let mut unit = DependencyBodyUnit::default();
    unit.lower_function(root, &bodies, &KlibCalleeFacts::default())
        .expect("the producer graph is transitive");
    validated(unit.ir());
    assert_eq!(unit.ir().functions.len(), 2);
}

// --- Equality and negation ----------------------------------------------------------------------

/// `f(a, b) = a OP b`, with the built-in call `build` makes of the two parameter reads.
fn binary(
    params: &'static [T],
    build: impl Fn(&mut Body, KlibIrExprId, KlibIrExprId) -> KlibIrExprId,
) -> Demo {
    demo(&[("f", params, T::Boolean)], |_, body| {
        let lhs = body.param(0);
        let rhs = body.param(1);
        let value = build(body, lhs, rhs);
        Some(vec![body.ret(value)])
    })
}

#[test]
fn equality_built_ins_lower_as_the_source_operators_do() {
    let int: &[T] = &[T::Int, T::Int];
    let double: &[T] = &[T::Double, T::Double];
    let string: &[T] = &[T::String, T::String];
    let any: &[T] = &[T::NullableAny, T::NullableAny];
    type Build = fn(&mut Body, KlibIrExprId, KlibIrExprId) -> KlibIrExprId;
    let cases: [(&'static [T], &str, &str, Build); 8] = [
        (int, "Int", "a == b", |body, a, b| {
            body.builtin(eqeq(), vec![a, b], Some("EQEQ"))
        }),
        (int, "Int", "a != b", |body, a, b| {
            let equal = body.builtin(eqeq(), vec![a, b], Some("EXCLEQ"));
            body.builtin(not(), vec![equal], Some("EXCLEQ"))
        }),
        (int, "Int", "!(a == b)", |body, a, b| {
            let equal = body.builtin(eqeq(), vec![a, b], Some("EQEQ"));
            body.builtin(not(), vec![equal], None)
        }),
        (double, "Double", "a == b", |body, a, b| {
            body.builtin(ieee754equals("Double"), vec![a, b], Some("EQEQ"))
        }),
        (double, "Double", "a != b", |body, a, b| {
            let equal = body.builtin(ieee754equals("Double"), vec![a, b], Some("EXCLEQ"));
            body.builtin(not(), vec![equal], Some("EXCLEQ"))
        }),
        (string, "String", "a == b", |body, a, b| {
            body.builtin(eqeq(), vec![a, b], Some("EQEQ"))
        }),
        (any, "Any?", "a === b", |body, a, b| {
            body.builtin(eqeqeq(), vec![a, b], Some("EQEQEQ"))
        }),
        (any, "Any?", "a !== b", |body, a, b| {
            let identical = body.builtin(eqeqeq(), vec![a, b], Some("EXCLEQEQ"));
            body.builtin(not(), vec![identical], Some("EXCLEQEQ"))
        }),
    ];
    for (params, source_type, operator, build) in cases {
        let source = format!(
            "fun f(a: {source_type}, b: {source_type}): Boolean {{\n return {operator}\n}}\n"
        );
        assert_eq!(
            lowered_f(&binary(params, build)),
            source_body(&source, "f", 2),
            "{operator} on {source_type}"
        );
    }
}

#[test]
fn negation_lowers_as_the_source_operator_does() {
    let demo = demo(&[("f", &[T::Boolean], T::Boolean)], |_, body| {
        let a = body.param(0);
        let negated = body.builtin(not(), vec![a], None);
        Some(vec![body.ret(negated)])
    });
    let lowered = lowered_f(&demo);
    assert_eq!(
        lowered,
        source_body("fun f(a: Boolean): Boolean {\n return !a\n}\n", "f", 1)
    );
    assert_eq!(
        lowered,
        "scope {scope {return@Some(0) negation Eq(v0!: Boolean, Boolean(false)): Boolean: \
         Nothing}: Nothing}"
    );
}

#[test]
fn an_equality_the_checker_would_classify_otherwise_declines() {
    let doubles = binary(&[T::Double, T::Double], |body, a, b| {
        body.builtin(eqeq(), vec![a, b], Some("EQEQ"))
    });
    assert_eq!(
        doubles.declined("f", &[]),
        "the KLIB body of `demo.f` (its serialized declaration disagrees with the selected one: \
         `EQEQ` compares two operands of one floating-point type)"
    );
    let ints = binary(&[T::Int, T::Int], |body, a, b| {
        body.builtin(ieee754equals("Double"), vec![a, b], Some("EQEQ"))
    });
    assert_eq!(
        ints.declined("f", &[]),
        "the KLIB body of `demo.f` (it uses an IEEE 754 equality of operands other than one \
         floating-point type)"
    );
    let float_operator = binary(&[T::Double, T::Double], |body, a, b| {
        body.builtin(ieee754equals("Float"), vec![a, b], Some("EQEQ"))
    });
    assert_eq!(
        float_operator.declined("f", &[]),
        "the KLIB body of `demo.f` (its serialized declaration disagrees with the selected one: \
         `ieee754equals` compares operands of another type than its own)"
    );
}

#[test]
fn a_negation_of_another_source_form_declines() {
    let not_in = demo(&[("f", &[T::Boolean], T::Boolean)], |_, body| {
        let a = body.param(0);
        let negated = body.builtin(not(), vec![a], Some("NOT_IN"));
        Some(vec![body.ret(negated)])
    });
    assert_eq!(
        not_in.declined("f", &[]),
        "the KLIB body of `demo.f` (it uses a `!in` of another form than a membership in a range literal)"
    );
    let bare = demo(&[("f", &[T::Boolean], T::Boolean)], |_, body| {
        let a = body.param(0);
        let negated = body.builtin(not(), vec![a], Some("EXCLEQ"));
        Some(vec![body.ret(negated)])
    });
    assert_eq!(
        bare.declined("f", &[]),
        "the KLIB body of `demo.f` (its serialized declaration disagrees with the selected one: \
         a `!=` negates no built-in equality)"
    );
}

// --- Local variables ----------------------------------------------------------------------------

#[test]
fn local_variables_lower_as_the_source_declarations_do() {
    let demo = demo(&[("f", &[T::Int], T::Int)], |_, body| {
        let a = body.param(0);
        let (x_declaration, x) = body.val(T::Int, a);
        let x_read = body.get(&x, T::Int);
        let (y_declaration, y) = body.var(T::Int, x_read);
        let a = body.param(0);
        let assignment = body.set(&y, a);
        let y_read = body.get(&y, T::Int);
        Some(vec![
            x_declaration,
            y_declaration,
            assignment,
            body.ret(y_read),
        ])
    });
    let lowered = lowered_f(&demo);
    assert_eq!(
        lowered,
        source_body(
            "fun f(a: Int): Int {\n val x = a\n var y = x\n y = a\n return y\n}\n",
            "f",
            1
        )
    );
    assert_eq!(
        lowered,
        "scope {scope {val v1: Int = v0!: Int; val v2: Int = v1!: Int; v2 = v0!: Int: Unit; \
         return@Some(0) v2~: Int: Nothing}: Nothing}"
    );
}

#[test]
fn unmodelled_local_variables_decline_by_form() {
    let cases: [(u64, &str, bool, &str); 4] = [
        (
            0b1000 | 0b10,
            "DEFINED",
            true,
            "it uses a `lateinit` variable",
        ),
        (0b100, "DEFINED", true, "it uses a `const` variable"),
        (
            0,
            "IR_TEMPORARY_VARIABLE",
            true,
            "it uses a compiler-introduced variable",
        ),
        (
            0,
            "DEFINED",
            false,
            "it uses a variable without an initializer",
        ),
    ];
    for (flags, origin, initialized, reason) in cases {
        let demo = demo(&[("f", &[T::Int], T::Int)], |_, body| {
            let a = body.param(0);
            let (declaration, x) =
                body.variable_with(T::Int, initialized.then_some(a), flags, origin);
            let read = body.get(&x, T::Int);
            Some(vec![declaration, body.ret(read)])
        });
        assert_eq!(
            demo.declined("f", &[]),
            format!("the KLIB body of `demo.f` ({reason})")
        );
    }
}

#[test]
fn an_assignment_to_a_val_declines() {
    let demo = demo(&[("f", &[T::Int], T::Int)], |_, body| {
        let a = body.param(0);
        let (declaration, x) = body.val(T::Int, a);
        let a = body.param(0);
        let assignment = body.set(&x, a);
        let read = body.get(&x, T::Int);
        Some(vec![declaration, assignment, body.ret(read)])
    });
    assert_eq!(
        demo.declined("f", &[]),
        "the KLIB body of `demo.f` (it uses an assignment that initializes a `val`)"
    );
}

// --- Implicit casts -----------------------------------------------------------------------------

#[test]
fn an_implicit_cast_to_the_value_type_adds_nothing() {
    let demo = demo(&[("f", &[T::Int], T::Int)], |_, body| {
        let a = body.param(0);
        let cast = body.type_operator(KlibIrTypeOperator::ImplicitCast, a, T::Int);
        Some(vec![body.ret(cast)])
    });
    assert_eq!(
        lowered_f(&demo),
        source_body("fun f(a: Int): Int {\n return a\n}\n", "f", 1)
    );
}

#[test]
fn an_implicit_cast_to_the_value_nullable_type_is_the_widening_coercion() {
    let demo = demo(&[("f", &[T::Int], T::NullableInt)], |_, body| {
        let a = body.param(0);
        let cast = body.type_operator(KlibIrTypeOperator::ImplicitCast, a, T::NullableInt);
        Some(vec![body.ret(cast)])
    });
    assert_eq!(
        lowered_f(&demo),
        source_body("fun f(a: Int): Int? {\n return a\n}\n", "f", 1)
    );
}

#[test]
fn an_implicit_cast_of_null_to_a_nullable_type_adds_nothing() {
    let demo = demo(&[("f", &[], T::NullableString)], |_, body| {
        let null = body.constant(T::NullableNothing, KlibIrConstant::Null);
        let cast = body.type_operator(KlibIrTypeOperator::ImplicitCast, null, T::NullableString);
        Some(vec![body.ret(cast)])
    });
    assert_eq!(
        lowered_f(&demo),
        source_body("fun f(): String? {\n return null\n}\n", "f", 0)
    );
}

#[test]
fn a_converting_or_unmodelled_type_operator_declines() {
    let converting = demo(&[("f", &[T::NullableAny], T::NullableAny)], |_, body| {
        let a = body.param(0);
        let cast = body.type_operator(KlibIrTypeOperator::ImplicitCast, a, T::String);
        let widened = body.type_operator(KlibIrTypeOperator::ImplicitCast, cast, T::NullableAny);
        Some(vec![body.ret(widened)])
    });
    assert_eq!(
        converting.declined("f", &[]),
        "the KLIB body of `demo.f` (it converts a value implicitly)"
    );
    let not_null = demo(&[("f", &[T::Int], T::Int)], |_, body| {
        let a = body.param(0);
        let cast = body.type_operator(KlibIrTypeOperator::ImplicitNotNull, a, T::Int);
        Some(vec![body.ret(cast)])
    });
    assert_eq!(
        not_null.declined("f", &[]),
        "the KLIB body of `demo.f` (it uses an implicit non-null assertion)"
    );
}

// --- Multi-branch `when` ------------------------------------------------------------------------

/// `f(a)` returning -1, 1 or 0 by the sign of `a`, as a `when` of `origin`.
fn sign(origin: &'static str) -> Demo {
    demo(&[("f", &[T::Int], T::Int)], move |_, body| {
        let a = body.param(0);
        let zero = body.int(0);
        let negative = body.builtin(relation("less"), vec![a, zero], Some("LT"));
        let minus_one = body.int(-1);
        let a = body.param(0);
        let zero = body.int(0);
        let positive = body.builtin(relation("greater"), vec![a, zero], Some("GT"));
        let one = body.int(1);
        let otherwise = body.int(0);
        let when = body.when_(
            origin,
            vec![(negative, minus_one), (positive, one)],
            otherwise,
            T::Int,
        );
        Some(vec![body.ret(when)])
    })
}

#[test]
fn a_flat_when_lowers_as_the_source_when_does() {
    let source =
        "fun f(a: Int): Int {\n return when {\n a < 0 -> -1\n a > 0 -> 1\n else -> 0\n }\n}\n";
    let lowered = lowered_f(&sign("WHEN"));
    assert_eq!(lowered, source_body(source, "f", 1));
    assert_eq!(
        lowered,
        "scope {scope {return@Some(0) exhaustive:Int when[Lt(v0!: Int, Int(0): Int): Boolean -> \
         Int(-1): Int, Gt(v0!: Int, Int(0): Int): Boolean -> Int(1): Int, else -> Int(0): Int]: \
         Int: Nothing}: Nothing}"
    );
}

#[test]
fn a_flat_if_chain_lowers_as_the_source_else_if_chain_does() {
    let source = "fun f(a: Int): Int {\n return if (a < 0) -1 else if (a > 0) 1 else 0\n}\n";
    assert_eq!(lowered_f(&sign("IF")), source_body(source, "f", 1));
}

// --- The Kotlin/Native stdlib -------------------------------------------------------------------

/// The semantic type of the class `kotlin.<name>`.
pub(super) fn kotlin(name: &str) -> Ty {
    semantic_ty(&kotlin_class(name), &HashMap::default())
}

#[test]
fn stdlib_structural_equality_lowers_as_its_source_does() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let bodies = libraries.declaration_bodies();
    let any = kotlin("Any");
    let fact = frozen_function(
        &libraries,
        "kotlin/native/internal",
        "Kotlin_equals",
        None,
        &[any, any],
    );
    let mut unit = DependencyBodyUnit::default();
    let function = unit
        .lower_function(
            fact.klib_body_callable().expect("signed"),
            bodies,
            &KlibCalleeFacts::default(),
        )
        .unwrap_or_else(|decline| panic!("{decline}"));
    validated(unit.ir());
    let source = "fun Kotlin_equals(a: Any, b: Any): Boolean {\n return a == b\n}\n";
    let lowered = lowered_body(&unit, function);
    assert_eq!(lowered, source_body(source, "Kotlin_equals", 2));
    assert_eq!(
        lowered,
        "scope {scope {return@Some(0) Eq.Structural(v0!: kotlin/Any, v1!: kotlin/Any): Boolean: Nothing}: \
         Nothing}"
    );
}

/// `kotlin.math.max` on `ULong` calls `kotlin.comparisons.maxOf`, whose body the stdlib
/// serializes with an inlined function block: the caller declines through its callee, whether a
/// checked call selected the callee or its serialized header alone describes it.
#[test]
fn stdlib_unsigned_max_declines_through_its_callee() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let bodies = libraries.declaration_bodies();
    let unsigned = kotlin("ULong");
    let max = frozen_function(
        &libraries,
        "kotlin/math",
        "max",
        None,
        &[unsigned, unsigned],
    );
    let max_of = frozen_function(
        &libraries,
        "kotlin/comparisons",
        "maxOf",
        None,
        &[unsigned, unsigned],
    );
    let callable = max.klib_body_callable().expect("signed");
    let mut unit = DependencyBodyUnit::default();
    let selected = KlibCalleeFacts::new([max_of.klib_body_callable().expect("signed")]);
    let expected = "the KLIB body of `kotlin.math.max` (it reaches the KLIB body of \
         `kotlin.comparisons.maxOf` (it uses a call through a property accessor signature))";
    assert_eq!(
        unit.lower_function(callable, bodies, &selected)
            .expect_err("the callee declines")
            .to_string(),
        expected
    );
    assert_unit_is_empty(&unit);
    assert_eq!(
        unit.lower_function(callable, bodies, &KlibCalleeFacts::default())
            .expect_err("the callee declines")
            .to_string(),
        expected
    );
    assert_unit_is_empty(&unit);
}

/// Every call the stdlib serializes of a built-in operator lowering signs passes operands of the
/// operator's own type: the table's identities are the stdlib's, operand type for operand type.
#[test]
fn stdlib_built_in_calls_have_the_signed_operands() {
    let Some(root) = distribution_root() else {
        return;
    };
    let path = root.join("klib/common/stdlib");
    let archive = crate::klib::KlibArchive::open(&path).expect("the stdlib opens");
    let trees = read_declaration_trees(&archive).expect("the stdlib IR decodes");
    let builtins = super::super::ir_builtins::IrBuiltinOperators::new();
    let mut checked = HashMap::<&'static str, usize>::default();
    for tree in trees.trees() {
        let arena = &tree.arena;
        for index in 0..arena.expr_count() {
            let KlibIrExprKind::Call { access, .. } = &arena.expr(expr_id(arena, index)).kind
            else {
                continue;
            };
            let KlibIrSignature::Public(callee) = &access.symbol.signature else {
                continue;
            };
            let Some(operator) = builtins.operator(callee) else {
                continue;
            };
            let KlibIrArguments::Flat(arguments) = &access.arguments else {
                panic!("the 2.4.20 stdlib serializes flat arguments");
            };
            let operand_types = arguments
                .iter()
                .map(|argument| {
                    let argument = argument.expect("a built-in call supplies every argument");
                    let ty = arena.expr(argument).ty.expect("an argument is typed");
                    super::super::klib_types::semantic_type(arena, &Default::default(), ty).ok()
                })
                .collect::<Vec<_>>();
            let (kind, expected) = match operator {
                super::super::ir_builtins::BuiltinOperator::Relation(relation) => {
                    ("relation", Some(relation.operand))
                }
                super::super::ir_builtins::BuiltinOperator::Ieee754Equals { operand } => {
                    ("ieee754equals", Some(operand))
                }
                super::super::ir_builtins::BuiltinOperator::IsNan { operand } => {
                    ("isNaN", Some(operand))
                }
                super::super::ir_builtins::BuiltinOperator::Not => ("not", Some(Ty::Boolean)),
                super::super::ir_builtins::BuiltinOperator::NumericConversion { from, .. } => {
                    ("numeric conversion", Some(from))
                }
                super::super::ir_builtins::BuiltinOperator::Equals => ("EQEQ", None),
                super::super::ir_builtins::BuiltinOperator::Identical => ("EQEQEQ", None),
                super::super::ir_builtins::BuiltinOperator::CheckNotNull => {
                    ("CHECK_NOT_NULL", None)
                }
            };
            if let Some(expected) = expected {
                for ty in operand_types.into_iter().flatten() {
                    assert_eq!(ty.non_null(), expected, "{kind} {callee:?}");
                }
            }
            *checked.entry(kind).or_default() += 1;
        }
    }
    for kind in [
        "relation",
        "ieee754equals",
        "isNaN",
        "not",
        "numeric conversion",
        "EQEQ",
        "EQEQEQ",
        "CHECK_NOT_NULL",
    ] {
        assert!(
            checked.get(kind).copied().unwrap_or(0) > 0,
            "no {kind} call: {checked:?}"
        );
    }
}

fn expr_id(arena: &KlibIrArena, index: usize) -> KlibIrExprId {
    arena.expr_id(index)
}

/// The local-variable flag layout lowering reads is the serializer's: every stdlib local that is
/// initialized and assigned again is flagged `var`, and the annotations flag is set exactly on the
/// locals that carry annotations.
#[test]
fn stdlib_local_variable_flags_have_the_decoded_layout() {
    let Some(root) = distribution_root() else {
        return;
    };
    let path = root.join("klib/common/stdlib");
    let archive = crate::klib::KlibArchive::open(&path).expect("the stdlib opens");
    let trees = read_declaration_trees(&archive).expect("the stdlib IR decodes");
    let (mut reassigned, mut annotated) = (0, 0);
    for tree in trees.trees() {
        let arena = &tree.arena;
        let assigned = (0..arena.expr_count())
            .filter_map(|index| match &arena.expr(expr_id(arena, index)).kind {
                KlibIrExprKind::SetValue { symbol, .. } => Some(symbol.clone()),
                _ => None,
            })
            .collect::<crate::fx_hash::HashSet<_>>();
        for index in 0..arena.variable_count() {
            let variable = arena.variable(arena.variable_id(index));
            let flags = variable.base.flags;
            assert_eq!(
                flags & 1 != 0,
                !variable.base.annotations.is_empty(),
                "{:?} flags {flags:#x}",
                variable.base.symbol
            );
            annotated += usize::from(flags & 1 != 0);
            if variable.initializer.is_some() && assigned.contains(&variable.base.symbol) {
                assert_eq!(
                    flags & 0b10,
                    0b10,
                    "{:?} is reassigned",
                    variable.base.symbol
                );
                reassigned += 1;
            }
        }
    }
    assert!(reassigned > 0 && annotated > 0, "{reassigned} {annotated}");
}

/// Every value node of a lowered body has a checked type a target can read: a backend that
/// types each value it emits (Wasm) never meets an untyped one. A local declaration is a
/// statement, and a function's body root is the callable's own scope; neither is a value.
#[test]
fn every_lowered_value_has_a_checked_type() {
    let shapes: [Shape; 5] = [
        ("callee", &[T::Int], T::Int),
        ("f", &[T::Int, T::Double], T::Boolean),
        ("g", &[T::Int], T::Int),
        ("h", &[T::Boolean, T::Int, T::NullableAny], T::String),
        ("t", &[T::Throwable], T::Nothing),
    ];
    let demo = demo(&shapes, |name, body| match name {
        // var x = b
        // while (x > 0) { if (a && x > 5) break; if (a) callee(x); x = callee(x) }
        // if (c is String) return c as String
        // return "x$x"
        "h" => {
            let b = body.param(1);
            let (x_declaration, x) = body.var(T::Int, b);
            let a = body.param(0);
            let x_read = body.get(&x, T::Int);
            let five = body.int(5);
            let above = body.builtin(relation("greater"), vec![x_read, five], Some("GT"));
            let otherwise = body.otherwise();
            let constant = body.boolean(false);
            let both = body.branches(
                "ANDAND",
                vec![(a, above), (otherwise, constant)],
                T::Boolean,
            );
            let broken = body.break_(1);
            let leave = body.branches("IF", vec![(both, broken)], T::Unit);
            let a = body.param(0);
            let x_read = body.get(&x, T::Int);
            let called = body.call("callee", vec![x_read], T::Int);
            let coerced = body.coerced(called);
            let effect = body.branches("IF", vec![(a, coerced)], T::Unit);
            let x_read = body.get(&x, T::Int);
            let called = body.call("callee", vec![x_read], T::Int);
            let assignment = body.set(&x, called);
            let loop_body = body.block(
                vec![
                    KlibIrStatement::Expression(leave),
                    KlibIrStatement::Expression(effect),
                    assignment,
                ],
                T::Unit,
            );
            let x_read = body.get(&x, T::Int);
            let zero = body.int(0);
            let positive = body.builtin(relation("greater"), vec![x_read, zero], Some("GT"));
            let looped = body.loop_(1, positive, loop_body, false);
            let c = body.param(2);
            let string = body.ty(T::String);
            let check = body.expr(
                T::Boolean,
                KlibIrExprKind::TypeOperator {
                    operator: KlibIrTypeOperator::InstanceOf,
                    operand: string,
                    argument: c,
                },
            );
            let c = body.param(2);
            let cast = body.type_operator(KlibIrTypeOperator::Cast, c, T::String);
            let returned = body.return_(cast);
            let guard = body.branches("IF", vec![(check, returned)], T::Unit);
            let text = body.string("x");
            let x_read = body.get(&x, T::Int);
            let template = body.concat(vec![text, x_read]);
            Some(vec![
                x_declaration,
                looped,
                KlibIrStatement::Expression(guard),
                body.ret(template),
            ])
        }
        "t" => {
            let a = body.param(0);
            Some(vec![KlibIrStatement::Expression(body.throw(a))])
        }
        "callee" => {
            let a = body.param(0);
            Some(vec![body.ret(a)])
        }
        "f" => {
            let a = body.param(0);
            let (x_declaration, x) = body.var(T::Int, a);
            let a = body.param(0);
            let called = body.call("callee", vec![a], T::Int);
            let assignment = body.set(&x, called);
            let x_read = body.get(&x, T::Int);
            let zero = body.int(0);
            let equal = body.builtin(eqeq(), vec![x_read, zero], Some("EXCLEQ"));
            let different = body.builtin(not(), vec![equal], Some("EXCLEQ"));
            let b = body.param(1);
            let c = body.param(1);
            let nan = body.builtin(ieee754equals("Double"), vec![b, c], Some("EQEQ"));
            let not_nan = body.builtin(not(), vec![nan], None);
            let y_read = body.get(&x, T::Int);
            let cast = body.type_operator(KlibIrTypeOperator::ImplicitCast, y_read, T::Int);
            let zero = body.int(0);
            let positive = body.builtin(relation("greater"), vec![cast, zero], Some("GT"));
            let otherwise = body.constant(T::Boolean, KlibIrConstant::Boolean(false));
            let when = body.when_(
                "IF",
                vec![(different, not_nan), (positive, not_nan)],
                otherwise,
                T::Boolean,
            );
            Some(vec![x_declaration, assignment, body.ret(when)])
        }
        _ => None,
    });
    let mut unit = DependencyBodyUnit::default();
    let f = demo
        .lower(&mut unit, "f", &["callee"])
        .unwrap_or_else(|decline| panic!("{decline}"));
    let g = sign("WHEN");
    let g_function = g
        .lower(&mut unit, "f", &[])
        .unwrap_or_else(|decline| panic!("{decline}"));
    for name in ["h", "t"] {
        demo.lower(&mut unit, name, &["callee"])
            .unwrap_or_else(|decline| panic!("{decline}"));
    }
    validated(unit.ir());
    let ir = unit.ir();
    let roots = ir
        .functions
        .iter()
        .map(|function| function.body.expect("a lowered function has a body"))
        .collect::<Vec<_>>();
    assert_eq!(ir.functions.len(), 5, "{f} {g_function}");
    let untyped = (0..ir.exprs.len())
        .map(|index| crate::ir::ExprId::try_from(index).expect("fits"))
        .filter(|expression| !roots.contains(expression))
        // Declarations, loops and jumps are statements.
        .filter(|expression| {
            !matches!(
                ir.expr(*expression),
                crate::ir::IrExpr::Variable { .. }
                    | crate::ir::IrExpr::While { .. }
                    | crate::ir::IrExpr::Break { .. }
                    | crate::ir::IrExpr::Continue { .. }
            )
        })
        .filter(|expression| ir.checked_type(*expression).is_none())
        .map(|expression| format!("{:?}", ir.expr(expression)))
        .collect::<Vec<_>>();
    assert_eq!(untyped, Vec::<String>::new());
}

/// The stdlib's `rangeTo` of the scalar class `owner` over its own type, as a dependency
/// inventory declares it.
fn declared_range_to(
    libraries: &crate::klib_libraries::KlibLibraries,
    owner: &str,
    element: Ty,
) -> crate::libraries::ExternalCallableRealization {
    use crate::symbol_source::SymbolSource;
    let class = libraries
        .classifier(crate::types::type_name(owner))
        .expect("stdlib class");
    let target = class.declared_callables["rangeTo"]
        .functions()
        .iter()
        .find(|function| function.callable.params == [element])
        .and_then(|function| function.callable.external_identity)
        .expect("provider identity");
    libraries
        .external_callable(target)
        .expect("exact declaration")
}

/// `Char.isSurrogate()` is `this in MIN_SURROGATE..MAX_SURROGATE`: a membership in a
/// range literal, which lowers to a comparison with the bounds, constructing no range.
#[test]
fn stdlib_range_literal_membership_is_a_bounds_comparison() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let range_to = declared_range_to(&libraries, "kotlin/Char", Ty::Char);
    let fact = frozen_function(
        &libraries,
        "kotlin/text",
        "isSurrogate",
        Some(Ty::Char),
        &[],
    );
    let facts = KlibCalleeFacts::with_declarations([], [range_to.klib_body_callable().unwrap()]);
    let mut unit = DependencyBodyUnit::default();
    let function = unit
        .lower_function(
            fact.klib_body_callable().expect("signed"),
            libraries.declaration_bodies(),
            &facts,
        )
        .unwrap_or_else(|decline| panic!("{decline}"));
    validated(unit.ir());
    // The source-side oracle here has no stdlib to resolve `Char..Char`, so the rendering is
    // checked against the range membership checked FIR lowering records for that source.
    assert_eq!(
        lowered_body(&unit, function),
        "scope {scope {return@Some(0) Through<kotlin/Char>(v0: kotlin/Char in Char(55296): \
         kotlin/Char, Char(57343): kotlin/Char): Boolean: Nothing}: Nothing}"
    );
}

/// `checkRadix` tests `radix !in MIN_RADIX..MAX_RADIX`: the negated membership lowers, and the
/// body declines only at its exception's constructor.
#[test]
fn stdlib_range_literal_negation_lowers_past_the_membership() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let range_to = declared_range_to(&libraries, "kotlin/Int", Ty::Int);
    let fact = frozen_function(&libraries, "kotlin/text", "checkRadix", None, &[Ty::Int]);
    let callable = fact.klib_body_callable().expect("signed");
    let mut unit = DependencyBodyUnit::default();
    assert_eq!(
        unit.lower_function(
            callable,
            libraries.declaration_bodies(),
            &KlibCalleeFacts::default()
        )
        .expect_err("no range construction is declared")
        .to_string(),
        "the KLIB body of `kotlin.text.checkRadix` (it uses a `!in` of another form than a \
         membership in a range literal)"
    );
    let facts = KlibCalleeFacts::with_declarations([], [range_to.klib_body_callable().unwrap()]);
    assert_eq!(
        unit.lower_function(callable, libraries.declaration_bodies(), &facts)
            .expect_err("the exception constructor declines")
            .to_string(),
        "the KLIB body of `kotlin.text.checkRadix` (it calls `kotlin.IllegalArgumentException.<init>`)"
    );
    assert_unit_is_empty(&unit);
}
