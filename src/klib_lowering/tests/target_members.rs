//! A member of a classifier the target represents links at its dispatch receiver; its library
//! body, written against the library's own class layout, is never lowered.

use super::*;

#[test]
fn klib_target_member_links_the_root_class_member_at_its_receiver() {
    use super::demo_package::{assert_unit_is_empty, selected_demo_module, T};
    use crate::klib_libraries::{KlibBodyLibrary, KlibIntrinsicFacts};
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let any = libraries.classifier(type_name("kotlin/Any")).unwrap();
    let candidate = &any.declared_callables["toString"].functions()[0];
    let provider = libraries
        .external_callable(candidate.callable.external_identity.unwrap())
        .unwrap();
    let selected = provider.klib_body_callable().unwrap();
    let represents_any = |classifier| classifier == crate::types::wk::any();
    for facts in [
        KlibCalleeFacts::new([selected]),
        KlibCalleeFacts::with_declarations([], [selected]),
    ] {
        assert!(facts
            .target_member(selected.identity(), &represents_any)
            .unwrap());
        assert!(
            !facts
                .target_member(selected.identity(), &|_| false)
                .unwrap(),
            "a target that lays out no classifier owes the member's body"
        );
    }

    let (arena, function) = libraries
        .declaration_bodies()
        .function(selected.identity())
        .unwrap();
    let KlibDeclarationSignature::Public(signature) = selected.signature() else {
        panic!("function")
    };
    let (records, caller_trees) = selected_demo_module(
        "caller",
        &[("a", &[T::Any], T::String)],
        &["a"],
        |_, body| {
            let receiver = body.param(0);
            let call = body.expr(
                T::String,
                KlibIrExprKind::Call {
                    access: KlibIrMemberAccess {
                        symbol: public_symbol(KlibIrSymbolKind::Function, signature.clone()),
                        arguments: KlibIrArguments::Flat(vec![Some(receiver)]),
                        type_arguments: Vec::new(),
                        origin: None,
                    },
                    super_qualifier: None,
                },
            );
            Some(vec![body.ret(call)])
        },
    );
    let bodies = KlibDeclarationBodies::from_body_libraries(vec![
        KlibBodyLibrary::new(
            "caller".to_owned(),
            [selected.identity().module.clone()],
            caller_trees,
        ),
        KlibBodyLibrary::new(
            selected.identity().module.clone(),
            [],
            KlibIrModuleTrees::from_trees(vec![super::target_calls::target_tree(arena, function)])
                .unwrap(),
        ),
    ])
    .unwrap();
    let root = records["a"].klib_body_callable().unwrap();
    let facts = KlibCalleeFacts::with_declarations([root], [selected]);
    let external = crate::fir::ExternalCallableId::from_raw(73);

    let leaves = [(
        selected.identity().clone(),
        super::super::EmittedDependencyCallable::target_member(external),
    )]
    .into_iter()
    .collect();
    let mut unit = DependencyBodyUnit::default();
    unit.lower_function_with_intrinsics_and_externals(
        root,
        &bodies,
        &facts,
        KlibIntrinsicFacts::empty(),
        &leaves,
    )
    .unwrap();
    validated(unit.ir());
    assert_eq!(
        unit.ir().functions.len(),
        1,
        "the member's library body is not lowered"
    );
    let calls = unit
        .ir()
        .exprs
        .iter()
        .filter_map(|expression| match expression {
            IrExpr::Call {
                callee,
                dispatch_receiver,
                args,
            } => Some((&**callee, *dispatch_receiver, args.len())),
            _ => None,
        })
        .collect::<Vec<_>>();
    let [(
        Callee::External {
            target,
            params,
            ret,
            ..
        },
        Some(_),
        0,
    )] = calls.as_slice()
    else {
        panic!("one call of the exact member at its dispatch receiver: {calls:?}")
    };
    assert_eq!(*target, external);
    assert!(params.is_empty());
    assert_eq!(*ret, Ty::obj("kotlin/String"));

    // The same edge as an ordinary body owes the library's implementation, whose header the
    // emitted function would have to frame at the member's receiver.
    let leaves = [(
        selected.identity().clone(),
        super::super::EmittedDependencyCallable::new(external, None, None),
    )]
    .into_iter()
    .collect();
    let mut unit = DependencyBodyUnit::default();
    let declined = unit
        .lower_function_with_intrinsics_and_externals(
            root,
            &bodies,
            &facts,
            KlibIntrinsicFacts::empty(),
            &leaves,
        )
        .unwrap_err();
    assert_eq!(
        declined.to_string(),
        "the KLIB body of `demo.a` (it reaches the KLIB body of `kotlin.Any.toString` (its \
         serialized declaration disagrees with the selected one: the serialized dispatch receiver \
         disagrees with the selected or emitted singleton))"
    );
    assert_unit_is_empty(&unit);
}

/// `fun a(): Unit = Unit`, whose body reads the `Unit` object.
fn unit_object_reader() -> super::demo_package::Demo {
    use super::demo_package::{demo, T};
    demo(&[("a", &[], T::Unit)], |_, body| {
        let KlibIrType::Simple { classifier, .. } = T::Unit.klib() else {
            panic!("`Unit` is a simple class type")
        };
        let object = body.expr(T::Unit, KlibIrExprKind::GetObject(classifier));
        Some(vec![body.ret(object)])
    })
}

#[test]
fn klib_object_value_of_a_target_classifier_is_the_targets_singleton() {
    use crate::klib_libraries::KlibIntrinsicFacts;
    let demo = unit_object_reader();
    let root = demo.facts["a"].klib_body_callable().unwrap();
    let represents_unit = |classifier| classifier == crate::types::wk::unit();
    let facts = KlibCalleeFacts::new([root]).with_target_classifiers(&represents_unit);
    let mut unit = DependencyBodyUnit::default();
    let function = unit
        .lower_function_with_intrinsics_and_externals(
            root,
            &demo.bodies,
            &facts,
            KlibIntrinsicFacts::empty(),
            &Default::default(),
        )
        .unwrap();
    validated(unit.ir());
    assert_eq!(
        lowered_body(&unit, function),
        super::demo_package::source_body_with_stdlib("fun a(): Unit {\n return Unit\n}\n", "a", 0)
    );
}

#[test]
fn klib_object_value_of_a_library_laid_out_object_declines() {
    use super::demo_package::assert_unit_is_empty;
    use crate::klib_libraries::KlibIntrinsicFacts;
    let demo = unit_object_reader();
    let root = demo.facts["a"].klib_body_callable().unwrap();
    let represents_any = |classifier| classifier == crate::types::wk::any();
    for facts in [
        KlibCalleeFacts::new([root]),
        KlibCalleeFacts::new([root]).with_target_classifiers(&represents_any),
    ] {
        let mut unit = DependencyBodyUnit::default();
        let declined = unit
            .lower_function_with_intrinsics_and_externals(
                root,
                &demo.bodies,
                &facts,
                KlibIntrinsicFacts::empty(),
                &Default::default(),
            )
            .unwrap_err();
        assert_eq!(
            declined.to_string(),
            "the KLIB body of `demo.a` (it uses an object value)"
        );
        assert_unit_is_empty(&unit);
    }
}
