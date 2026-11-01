//! Ordinary serialized bodies construct dependency classes through their exact declarations.

use super::*;
use crate::klib_libraries::KlibIntrinsicFacts;
use crate::libraries::ExternalCallableRealization;
use crate::metadata::klib_ir::tree::{KlibIrTypeArgument, KlibIrVariance};

#[derive(Clone, Copy, Debug)]
enum RawConstructorChange {
    None,
    ForeignModule,
    ClassMask,
    ConstructorMask,
    MemberSlot,
    SymbolKind,
    ConstructorFlag,
    ParameterKind,
    ParameterName,
    DuplicateParameter,
    ParameterCount,
    ParameterType,
    Default,
    Vararg,
    Bound,
    Variance,
    Reified,
    ReturnClass,
    ReturnApplication,
}

/// A declaration-only copy of the provider-selected Pair header: no executable body can be used
/// to recover from a malformed declaration. The fixture rebuilds indices in its own arena.
fn constructor_header_fixture(
    libraries: &KlibLibraries,
    provider: &ExternalCallableRealization,
    change: RawConstructorChange,
) -> KlibDeclarationBodies {
    use RawConstructorChange as C;
    let selected = provider.klib_body_callable().unwrap();
    let contract = selected.class_contract().unwrap();
    let raw = libraries
        .declaration_bodies()
        .class_member_header(selected.identity(), &contract.signature)
        .unwrap()
        .unwrap();
    let mut class = raw.class.clone();
    let mut function = raw.function.clone();
    let mut arena = KlibIrArena::default();
    let any = arena.push_type(super::demo_package::T::NullableAny.klib());
    let int = arena.push_type(super::demo_package::T::Int.klib());
    assert_eq!(class.type_parameters.len(), 2);
    assert_eq!(function.regular_parameters.len(), 2);
    let formals = class
        .type_parameters
        .iter_mut()
        .map(|parameter| {
            parameter.supertypes = vec![any];
            arena.push_type(KlibIrType::Simple {
                classifier: parameter.base.symbol.clone(),
                nullability: KlibIrNullability::NotSpecified,
                arguments: Vec::new(),
                annotations: Vec::new(),
                abbreviation: None,
            })
        })
        .collect::<Vec<_>>();
    for (parameter, formal) in function.regular_parameters.iter_mut().zip(&formals) {
        parameter.ty = *formal;
        parameter.default_value = None;
        parameter.vararg_element_type = None;
    }
    let result = arena.push_type(KlibIrType::Simple {
        classifier: public_symbol(KlibIrSymbolKind::Class, contract.signature.clone()),
        nullability: KlibIrNullability::NotSpecified,
        arguments: formals
            .iter()
            .map(|ty| KlibIrTypeArgument::Type {
                variance: KlibIrVariance::Invariant,
                ty: *ty,
            })
            .collect(),
        annotations: Vec::new(),
        abbreviation: None,
    });
    function.return_type = result;
    function.body = None;
    class.this_receiver = None;
    class.supertypes.clear();
    match change {
        C::ClassMask => {
            class.base.symbol.signature = KlibIrSignature::Public(
                super::constructor_contracts::different_mask(&contract.signature),
            )
        }
        C::ConstructorMask => {
            let KlibDeclarationSignature::Public(signature) = selected.signature() else {
                panic!("constructor signature")
            };
            function.base.symbol.signature =
                KlibIrSignature::Public(super::constructor_contracts::different_mask(signature));
        }
        C::SymbolKind => function.base.symbol.kind = KlibIrSymbolKind::Function,
        C::ConstructorFlag => function.constructor = false,
        C::ParameterKind => {
            function.regular_parameters[0].base.symbol.kind = KlibIrSymbolKind::ReceiverParameter
        }
        C::ParameterName => function.regular_parameters[0].name.push_str("-different"),
        C::DuplicateParameter => {
            function.regular_parameters[1].base.symbol =
                function.regular_parameters[0].base.symbol.clone()
        }
        C::ParameterCount => {
            function.regular_parameters.pop();
        }
        C::ParameterType => function.regular_parameters[0].ty = int,
        C::Default => {
            function.regular_parameters[0].default_value =
                Some(arena.push_expr(None, KlibIrExprKind::Missing))
        }
        C::Vararg => function.regular_parameters[0].vararg_element_type = Some(any),
        C::Bound => class.type_parameters[0].supertypes = vec![int],
        C::Variance => class.type_parameters[0].base.flags ^= 2,
        C::Reified => class.type_parameters[0].base.flags |= 8,
        C::ReturnClass => function.return_type = any,
        C::ReturnApplication => {
            function.return_type = arena.push_type(KlibIrType::Simple {
                classifier: public_symbol(KlibIrSymbolKind::Class, contract.signature.clone()),
                nullability: KlibIrNullability::NotSpecified,
                arguments: formals
                    .iter()
                    .rev()
                    .map(|ty| KlibIrTypeArgument::Type {
                        variance: KlibIrVariance::Invariant,
                        ty: *ty,
                    })
                    .collect(),
                annotations: Vec::new(),
                abbreviation: None,
            });
        }
        C::None | C::ForeignModule | C::MemberSlot => {}
    }
    let function = arena.push_function(function);
    class.members = if matches!(change, C::MemberSlot) {
        Vec::new()
    } else {
        vec![KlibIrMember::Function(function)]
    };
    let class = arena.push_class(class);
    let module = if matches!(change, C::ForeignModule) {
        format!("{}-foreign", contract.module)
    } else {
        contract.module.clone()
    };
    KlibDeclarationBodies::from_libraries(vec![(
        module,
        KlibIrModuleTrees::from_trees(vec![KlibIrDeclarationTree {
            arena,
            declaration: KlibIrMember::Class(class),
        }])
        .unwrap(),
    )])
    .unwrap()
}

fn pair_constructor(libraries: &KlibLibraries) -> ExternalCallableRealization {
    let symbols = libraries.symbols(SymbolNamespace::Package(type_name("kotlin")), "Pair");
    let class = symbols.classifier.as_ref().unwrap();
    let [constructor] = class.constructors.as_slice() else {
        panic!("Pair constructor")
    };
    libraries
        .external_callable(constructor.external_identity.unwrap())
        .unwrap()
}

#[test]
fn klib_constructor_rejects_malformed_raw_declaration_contracts() {
    use RawConstructorChange as C;
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let provider = pair_constructor(&libraries);
    let selected = provider.klib_body_callable().unwrap();
    super::super::constructor_calls::authenticate(
        selected,
        &constructor_header_fixture(&libraries, &provider, C::None),
    )
    .unwrap_or_else(|reason| panic!("the exact declaration-only header: {reason:?}"));
    for change in [
        C::ForeignModule,
        C::ClassMask,
        C::ConstructorMask,
        C::MemberSlot,
        C::SymbolKind,
        C::ConstructorFlag,
        C::ParameterKind,
        C::ParameterName,
        C::DuplicateParameter,
        C::ParameterCount,
        C::ParameterType,
        C::Default,
        C::Vararg,
        C::Bound,
        C::Variance,
        C::Reified,
        C::ReturnClass,
        C::ReturnApplication,
    ] {
        let result = super::super::constructor_calls::authenticate(
            selected,
            &constructor_header_fixture(&libraries, &provider, change),
        );
        assert!(
            result.is_err(),
            "malformed constructor header must not authenticate: {change:?}"
        );
    }
    for changed in [
        {
            let mut changed = provider.clone();
            changed.klib_body_parameter_defaults = None;
            changed
        },
        {
            let mut changed = provider.clone();
            changed.klib_body_parameter_defaults =
                Some(crate::libraries::ParamFlags::repeat(false, 1));
            changed
        },
        {
            let mut changed = provider.clone();
            changed.klib_body_vararg_index = Some(0);
            changed
        },
        {
            let mut changed = provider.clone();
            changed.callable.visibility = Visibility::Private;
            changed
        },
        {
            let mut changed = provider.clone();
            changed.callable.ret = Ty::obj("kotlin/Pair");
            changed
        },
    ] {
        assert!(
            super::super::constructor_calls::authenticate(
                changed.klib_body_callable().unwrap(),
                libraries.declaration_bodies()
            )
            .is_err(),
            "malformed selected constructor contract"
        );
    }
}

#[test]
fn klib_constructor_requires_exact_provider_evidence_and_a_constructor_handle() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let symbols = libraries.symbols(SymbolNamespace::Package(type_name("kotlin")), "to");
    let [to] = symbols.callables.functions() else {
        panic!("the generic to declaration")
    };
    let to = libraries
        .external_callable(to.callable.external_identity.unwrap())
        .unwrap();
    let selected = to.klib_body_callable().unwrap();
    let provider = pair_constructor(&libraries);
    let target = provider.callable.external_identity.unwrap();
    let mut wrong_kind = provider.clone();
    wrong_kind.kind = ExternalCallableKind::TopLevel;
    let wrong_kind = wrong_kind.klib_body_callable().unwrap();
    let constructor = provider.klib_body_callable().unwrap();
    let (_, raw) = libraries
        .declaration_bodies()
        .function(constructor.identity())
        .expect("the constructor declaration really exists");
    assert_eq!(raw.base.symbol.kind, KlibIrSymbolKind::Constructor);
    assert!(raw.constructor);
    let KlibDeclarationSignature::Public(signature) = constructor.signature() else {
        panic!("constructor signature")
    };
    let constructor_external = crate::fx_hash::HashMap::from_iter([(
        constructor.identity().clone(),
        super::super::EmittedDependencyCallable::constructor(target),
    )]);
    let body_external = crate::fx_hash::HashMap::from_iter([(
        constructor.identity().clone(),
        super::super::EmittedDependencyCallable::new(target, None, None),
    )]);
    let handle_error = KlibBodyDeclineReason::SignatureMismatch(
        "an authenticated constructor has no exact constructor external handle".to_owned(),
    );
    for (facts, externals, reason) in [
        (
            KlibCalleeFacts::new([selected]),
            constructor_external,
            KlibBodyDeclineReason::UnsupportedCallee(Box::new(signature.clone())),
        ),
        (
            KlibCalleeFacts::new([selected, constructor]),
            crate::fx_hash::HashMap::default(),
            handle_error.clone(),
        ),
        (
            KlibCalleeFacts::new([selected, constructor]),
            body_external,
            handle_error,
        ),
        (
            KlibCalleeFacts::new([selected, wrong_kind]),
            crate::fx_hash::HashMap::default(),
            KlibBodyDeclineReason::SignatureMismatch(
                "a constructor call selected a non-constructor provider declaration".to_owned(),
            ),
        ),
    ] {
        let mut unit = DependencyBodyUnit::default();
        let decline = unit
            .lower_function_with_intrinsics_and_externals(
                selected,
                libraries.declaration_bodies(),
                &facts,
                KlibIntrinsicFacts::empty(),
                &externals,
            )
            .expect_err("construction needs both independent proofs");
        assert_eq!(decline.reason(), &reason);
        assert_eq!(
            decline.is_internal_inconsistency(),
            matches!(reason, KlibBodyDeclineReason::SignatureMismatch(_))
        );
        super::demo_package::assert_unit_is_empty(&unit);
    }
}

#[test]
fn klib_generic_constructor_ordinary_body_retains_class_formals() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let namespace = SymbolNamespace::Package(type_name("kotlin"));
    let symbols = libraries.symbols(namespace, "to");
    let [to] = symbols.callables.functions() else {
        panic!("the stdlib's one generic to declaration");
    };
    let to_target = to.callable.external_identity.unwrap();
    let to = libraries.external_callable(to_target).unwrap();
    assert_eq!(to.klib_body_type_parameters.len(), 2);
    let pair = libraries.symbols(namespace, "Pair");
    let pair = pair.classifier.as_ref().unwrap();
    let [constructor] = pair.constructors.as_slice() else {
        panic!("Pair's one primary constructor");
    };
    let constructor_target = constructor.external_identity.unwrap();
    let constructor = libraries.external_callable(constructor_target).unwrap();
    let mut selections = IrFile::default();
    selections.add_expr(IrExpr::Call {
        callee: Box::new(Callee::External {
            target: to_target,
            default_provider: None,
            params: to.callable.params.clone(),
            ret: to.callable.ret,
            substitutions: Vec::new(),
            defaults: Vec::new(),
            extension_receiver_parameter: Some(0),
        }),
        dispatch_receiver: None,
        args: Vec::new(),
    });
    selections.add_expr(IrExpr::New {
        internal: type_name("kotlin/Pair"),
        args: Vec::new(),
        ctor_params: Some(constructor.callable.params.clone()),
        ctor_desc: None,
        external_target: Some(constructor_target),
        defaults: Box::new([]),
        default_prefix_count: 0,
    });
    let frozen = CheckedBackendCallables::freeze(&selections, &libraries).unwrap();
    let selected = frozen
        .callable(to_target)
        .unwrap()
        .klib_body_callable()
        .unwrap();
    let constructor = frozen
        .callable(constructor_target)
        .unwrap()
        .klib_body_callable()
        .unwrap();
    let facts = KlibCalleeFacts::new([selected, constructor]);
    let externals = crate::fx_hash::HashMap::from_iter([(
        constructor.identity().clone(),
        super::super::EmittedDependencyCallable::constructor(constructor_target),
    )]);
    let mut unit = DependencyBodyUnit::default();
    let function = unit
        .lower_function_with_intrinsics_and_externals(
            selected,
            libraries.declaration_bodies(),
            &facts,
            KlibIntrinsicFacts::empty(),
            &externals,
        )
        .unwrap_or_else(|decline| panic!("ordinary generic construction: {decline}"));
    validated(unit.ir());
    assert_eq!(
        unit.ir().functions[function as usize].params,
        to.callable.params
    );
    assert_eq!(unit.ir().functions[function as usize].ret, to.callable.ret);
    let constructions = unit
        .ir()
        .exprs
        .iter()
        .enumerate()
        .filter_map(|(index, expression)| match expression {
            IrExpr::New {
                internal,
                args,
                ctor_params,
                ctor_desc,
                external_target,
                defaults,
                default_prefix_count,
            } => Some((
                index as ExprId,
                *internal,
                args,
                ctor_params,
                ctor_desc,
                *external_target,
                defaults,
                *default_prefix_count,
            )),
            _ => None,
        })
        .collect::<Vec<_>>();
    let [(expression, classifier, arguments, parameters, descriptor, target, defaults, prefix)] =
        constructions.as_slice()
    else {
        panic!("the body constructs exactly one Pair");
    };
    assert_eq!(*classifier, type_name("kotlin/Pair"));
    assert_eq!(*parameters, &Some(to.callable.params.clone()));
    assert_eq!(*descriptor, &None);
    assert_eq!(*target, Some(constructor_target));
    assert!(defaults.is_empty());
    assert_eq!(*prefix, 0);
    assert_eq!(unit.ir().logical_type(*expression), Some(to.callable.ret));
    assert_eq!(
        arguments
            .iter()
            .map(|id| match unit.ir().expr(*id) {
                IrExpr::GetValue(slot) => *slot,
                other => panic!("a constructor reads its supplied parameter once: {other:?}"),
            })
            .collect::<Vec<_>>(),
        [0, 1]
    );
}
