//! Target leaves authenticate the same generic declaration as a serialized caller.

use super::*;
use crate::libraries::{CompilerIntrinsic, ListStorageConstruction};

fn storage_target(operation: CompilerIntrinsic) -> bool {
    matches!(operation, CompilerIntrinsic::ListStorageConstruction(_))
}

#[test]
fn klib_target_storage_leaves_include_checked_selections_and_header_evidence() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let symbols = libraries.symbols(
        SymbolNamespace::Package(type_name("kotlin/collections")),
        "listOf",
    );
    let mut plans = Vec::new();
    for candidate in symbols.callables.functions() {
        let provider = libraries
            .external_callable(candidate.callable.external_identity.unwrap())
            .unwrap();
        let selected = provider.klib_body_callable().unwrap();
        let Some(operation @ CompilerIntrinsic::ListStorageConstruction(plan)) =
            selected.compiler_intrinsic()
        else {
            continue;
        };
        plans.push(plan);
        for facts in [
            KlibCalleeFacts::new([selected]),
            KlibCalleeFacts::with_declarations([], [selected]),
        ] {
            assert_eq!(
                facts
                    .target_intrinsic(
                        selected.identity(),
                        libraries.declaration_bodies(),
                        &storage_target
                    )
                    .unwrap(),
                Some(operation),
                "the target supplies storage for {plan:?}, including checked source selections",
            );
            assert_eq!(
                facts
                    .target_intrinsic(selected.identity(), libraries.declaration_bodies(), &|_| {
                        false
                    })
                    .unwrap(),
                None,
                "a different target still owns the ordinary body obligation",
            );
        }
    }
    assert_eq!(plans.len(), 3);
    for plan in [
        ListStorageConstruction::ReadonlyEmpty,
        ListStorageConstruction::ReadonlySingleton,
        ListStorageConstruction::ReadonlyPacked,
    ] {
        assert_eq!(plans.iter().filter(|&&found| found == plan).count(), 1);
    }
}

fn copy_type(source: &KlibIrArena, target: &mut KlibIrArena, ty: KlibIrTypeId) -> KlibIrTypeId {
    use crate::metadata::klib_ir::tree::KlibIrTypeArgument;
    let mut copied = source.ty(ty).clone();
    match &mut copied {
        KlibIrType::Simple {
            arguments,
            abbreviation,
            ..
        } => {
            for argument in arguments {
                if let KlibIrTypeArgument::Type { ty, .. } = argument {
                    *ty = copy_type(source, target, *ty);
                }
            }
            *abbreviation = None;
        }
        KlibIrType::DefinitelyNotNull(ty) => *ty = copy_type(source, target, *ty),
        _ => panic!("the fixture copies only ordinary declared factory types"),
    }
    target.push_type(copied)
}

fn factory_tree(source: &KlibIrArena, function: &KlibIrFunction) -> KlibIrDeclarationTree {
    let mut arena = KlibIrArena::default();
    let mut function = function.clone();
    assert!(function.dispatch_receiver.is_none() && function.extension_receiver.is_none());
    assert!(function.context_parameters.is_empty());
    for formal in &mut function.type_parameters {
        formal.supertypes = formal
            .supertypes
            .iter()
            .map(|ty| copy_type(source, &mut arena, *ty))
            .collect();
    }
    for parameter in &mut function.regular_parameters {
        assert!(parameter.default_value.is_none());
        parameter.ty = copy_type(source, &mut arena, parameter.ty);
        parameter.vararg_element_type = parameter
            .vararg_element_type
            .map(|ty| copy_type(source, &mut arena, ty));
    }
    function.return_type = copy_type(source, &mut arena, function.return_type);
    // Deliberately unlowerable implementation: authenticating a target leaf must never execute it.
    function.body = Some(KlibIrBody::Synthetic(KlibIrSyntheticBody::EnumValues));
    let function = arena.push_function(function);
    KlibIrDeclarationTree {
        arena,
        declaration: KlibIrMember::Function(function),
    }
}

fn applied_factory_type(
    source: &KlibIrArena,
    ty: KlibIrTypeId,
    target: &mut KlibIrArena,
    argument: KlibIrTypeId,
) -> KlibIrTypeId {
    let mut ty = source.ty(ty).clone();
    let KlibIrType::Simple {
        arguments,
        abbreviation,
        ..
    } = &mut ty
    else {
        panic!("applied factory class")
    };
    assert_eq!(arguments.len(), 1);
    let crate::metadata::klib_ir::tree::KlibIrTypeArgument::Type { ty: formal, .. } =
        &mut arguments[0]
    else {
        panic!("declared formal use")
    };
    *formal = argument;
    *abbreviation = None;
    target.push_type(ty)
}

#[test]
fn klib_target_storage_external_leaves_keep_generic_calls_without_emitting_factory_bodies() {
    use super::demo_package::{selected_demo_module, T};
    use crate::klib_libraries::{KlibBodyLibrary, KlibIntrinsicFacts};
    use crate::metadata::klib_ir::tree::KlibIrVarargElement;
    let Some(distribution) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&distribution);
    let symbols = libraries.symbols(
        SymbolNamespace::Package(type_name("kotlin/collections")),
        "listOf",
    );
    for candidate in symbols.callables.functions() {
        let provider = libraries
            .external_callable(candidate.callable.external_identity.unwrap())
            .unwrap();
        let selected = provider.klib_body_callable().unwrap();
        let Some(operation @ CompilerIntrinsic::ListStorageConstruction(plan)) =
            selected.compiler_intrinsic()
        else {
            continue;
        };
        let (arena, factory) = libraries
            .declaration_bodies()
            .function(selected.identity())
            .unwrap();
        let KlibDeclarationSignature::Public(signature) = selected.signature() else {
            panic!("function")
        };
        for (element, parameters) in [(T::Int, &[T::Int][..]), (T::String, &[T::String][..])] {
            let (records, caller_trees) = selected_demo_module(
                "caller",
                &[("a", parameters, element)],
                &["a"],
                |_, body| {
                    let value = body.param(0);
                    let argument = body.ty(element);
                    let result =
                        applied_factory_type(arena, factory.return_type, &mut body.arena, argument);
                    let arguments = match plan {
                        ListStorageConstruction::ReadonlyEmpty => Vec::new(),
                        ListStorageConstruction::ReadonlySingleton => vec![Some(value)],
                        ListStorageConstruction::ReadonlyPacked => {
                            let packed = applied_factory_type(
                                arena,
                                factory.regular_parameters[0].ty,
                                &mut body.arena,
                                argument,
                            );
                            let packed = body.arena.push_expr(
                                Some(packed),
                                KlibIrExprKind::Vararg {
                                    element_type: argument,
                                    elements: vec![KlibIrVarargElement::Expression(value)],
                                },
                            );
                            vec![Some(packed)]
                        }
                        _ => panic!("readonly factory"),
                    };
                    let call = body.arena.push_expr(
                        Some(result),
                        KlibIrExprKind::Call {
                            access: KlibIrMemberAccess {
                                symbol: public_symbol(
                                    KlibIrSymbolKind::Function,
                                    signature.clone(),
                                ),
                                arguments: KlibIrArguments::Flat(arguments),
                                type_arguments: vec![Some(argument)],
                                origin: None,
                            },
                            super_qualifier: None,
                        },
                    );
                    Some(vec![KlibIrStatement::Expression(call), body.ret(value)])
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
                    KlibIrModuleTrees::from_trees(vec![factory_tree(arena, factory)]).unwrap(),
                ),
            ])
            .unwrap();
            let root = records["a"].klib_body_callable().unwrap();
            let facts = KlibCalleeFacts::with_declarations([root], [selected]);
            assert_eq!(
                facts
                    .target_intrinsic(selected.identity(), &bodies, &storage_target)
                    .unwrap(),
                Some(operation)
            );
            let external = crate::fir::ExternalCallableId::from_raw(71);
            let leaves = [(
                selected.identity().clone(),
                super::super::EmittedDependencyCallable::target_realization(external, operation),
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
            .unwrap_or_else(|reason| panic!("{plan:?}<{element:?}>: {reason}"));
            validated(unit.ir());
            assert_eq!(unit.ir().functions.len(), 1, "only the caller is emitted");
            let calls = unit
                .ir()
                .exprs
                .iter()
                .filter_map(|expression| match expression {
                    IrExpr::Call { callee, .. } => Some(&**callee),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(calls.len(), 1);
            let Callee::External {
                target,
                substitutions,
                params,
                ret,
                ..
            } = calls[0]
            else {
                panic!("a retained provider call")
            };
            assert_eq!(*target, external);
            assert_eq!(substitutions.len(), 1);
            assert_eq!(substitutions[0].value, element.semantic());
            let generic = selected.generic_signature().unwrap();
            let bindings = [(generic.formals[0].clone(), element.semantic())]
                .into_iter()
                .collect();
            assert_eq!(*ret, generic.ret.substitute_erased(&bindings));
            assert_eq!(
                *params,
                generic
                    .params
                    .iter()
                    .map(|ty| ty.substitute_erased(&bindings))
                    .collect::<Vec<_>>()
            );
            let mut ordinary = DependencyBodyUnit::default();
            assert!(
                ordinary.lower_function(root, &bodies, &facts).is_err(),
                "without an external leaf the unlowerable body is not silently accepted"
            );
            assert_eq!(ordinary.ir().functions.len(), 0, "transactional decline");
        }
    }
}

#[test]
fn klib_target_storage_leaves_reject_conflicting_missing_and_malformed_contracts() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let symbols = libraries.symbols(
        SymbolNamespace::Package(type_name("kotlin/collections")),
        "listOf",
    );
    let candidate = symbols
        .callables
        .functions()
        .iter()
        .find(|candidate| {
            candidate.callable.params.len() == 1 && candidate.callable.vararg_index.is_none()
        })
        .unwrap();
    let provider = libraries
        .external_callable(candidate.callable.external_identity.unwrap())
        .unwrap();
    let selected = provider.klib_body_callable().unwrap();
    let bodies = libraries.declaration_bodies();
    let mut malformed = provider.clone();
    malformed.callable.ret = Ty::Boolean;
    let malformed = malformed.klib_body_callable().unwrap();
    let facts = KlibCalleeFacts::with_declarations([], [selected, malformed]);
    assert_eq!(
        facts
            .target_intrinsic(selected.identity(), bodies, &storage_target)
            .err(),
        Some(KlibBodyDeclineReason::SignatureMismatch(
            "conflicting provider contracts for a target declaration".to_owned()
        ))
    );
    let facts = KlibCalleeFacts::new([malformed]);
    assert_eq!(
        facts
            .target_intrinsic(selected.identity(), bodies, &storage_target)
            .err(),
        Some(KlibBodyDeclineReason::SignatureMismatch(
            "the selected generic signature disagrees with its callable parameters or result"
                .to_owned()
        ))
    );
    let mut foreign = provider.clone();
    foreign.dependency_callable.as_mut().unwrap().module = "unrelated-module".to_owned();
    let foreign = foreign.klib_body_callable().unwrap();
    assert_eq!(
        KlibCalleeFacts::new([foreign])
            .target_intrinsic(foreign.identity(), bodies, &storage_target)
            .err(),
        Some(KlibBodyDeclineReason::Unjoined)
    );
    let mut ordinary = provider.clone();
    ordinary.callable.compiler_intrinsic = None;
    let ordinary = ordinary.klib_body_callable().unwrap();
    assert_eq!(
        KlibCalleeFacts::new([ordinary])
            .target_intrinsic(ordinary.identity(), bodies, &storage_target)
            .unwrap(),
        None
    );
    assert_eq!(
        KlibCalleeFacts::default()
            .target_intrinsic(selected.identity(), bodies, &storage_target)
            .unwrap(),
        None
    );
}
