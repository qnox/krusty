//! Official producer contract for a bound extension reference's storage and invocation slots.

use super::*;
use crate::klib_lowering::klib_types::{semantic_type, KlibTypeParameters};

#[test]
fn official_bound_extension_reference_wrappers_bind_the_declared_receiver() {
    let Some(root) = distribution_root() else {
        return;
    };
    let work = ProducerDirectory::new();
    let source = work.0.join("BoundReferences.kt");
    std::fs::write(
        &source,
        include_str!("../../../../tests/fixtures/klib_closure_frames/BoundExtensionReferences.kt"),
    )
    .unwrap();
    let library = work.0.join("bound-references");
    let output = std::process::Command::new(root.join("bin/kotlinc-native"))
        .args(["-p", "library", "-nopack", "-o"])
        .arg(&library)
        .arg(source)
        .output()
        .expect("run exact bound-reference producer");
    assert!(
        output.status.success(),
        "producer: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let libraries = KlibLibraries::open(&[
        KlibLibraryInput::platform_stdlib(
            root.join("klib/common/stdlib"),
            KlibStdlibSemantics::Native,
        ),
        KlibLibraryInput::dependency(library),
    ])
    .unwrap();
    let (_, target) = selected(&libraries, "boundReferenceAppend");
    let target = target.klib_body_callable().unwrap();
    assert_eq!(
        target.kind(),
        crate::libraries::ExternalCallableKind::Extension
    );
    assert_eq!(target.params(), &[Ty::String, Ty::String]);
    assert_eq!(
        target.parameter_identities(),
        &[
            crate::fir::ResolvedParameterIdentity::ExtensionReceiver,
            crate::fir::ResolvedParameterIdentity::Source("value".into()),
        ]
    );
    assert_eq!(target.ret(), Ty::String);
    for (name, prepared) in [
        ("makeBoundReference", false),
        ("inlineMakeBoundReference", false),
        ("inlineMakeBoundReference", true),
    ] {
        let (_, declaration) = selected(&libraries, name);
        let callable = declaration.klib_body_callable().unwrap();
        let bodies = if prepared {
            libraries.inline_declaration_bodies()
        } else {
            libraries.declaration_bodies()
        };
        let (arena, factory) = bodies.function(callable.identity()).unwrap();
        let [source_receiver] = factory.regular_parameters.as_slice() else {
            panic!("one declared factory input")
        };
        let returned = single_return(arena, factory);
        let KlibIrExprKind::RichFunctionReference {
            bound_values,
            reflection_target: Some(reflected),
            invoke,
            flags,
            ..
        } = &arena.expr(returned).kind
        else {
            panic!("one reflected bound reference: {:?}", arena.expr(returned))
        };
        assert_eq!(*flags, 0);
        let [bound] = bound_values.as_slice() else {
            panic!("one explicit bound input")
        };
        assert_eq!(read_symbol(arena, *bound), &source_receiver.base.symbol);
        assert_eq!(arena.expr(*bound).ty, Some(source_receiver.ty));
        assert_eq!(
            &reflected.signature,
            &match target.identity().declaration.clone() {
                KlibDeclarationSignature::Public(signature) => KlibIrSignature::Public(signature),
                _ => panic!("the extension has a public producer identity"),
            }
        );
        let wrapper = arena.function(*invoke);
        assert!(wrapper.type_parameters.is_empty());
        assert!(wrapper.dispatch_receiver.is_none());
        assert!(wrapper.context_parameters.is_empty());
        assert!(wrapper.extension_receiver.is_none());
        let [stored_receiver, argument] = wrapper.regular_parameters.as_slice() else {
            panic!("one storage slot and one invocation slot: {wrapper:?}")
        };
        assert_ne!(stored_receiver.base.symbol, source_receiver.base.symbol);
        assert_eq!(stored_receiver.ty, source_receiver.ty);
        let parameters = KlibTypeParameters::default();
        assert_eq!(
            semantic_type(arena, &parameters, stored_receiver.ty),
            Ok(Ty::String)
        );
        assert_eq!(
            semantic_type(arena, &parameters, argument.ty),
            Ok(Ty::String),
            "invocation input: {:?}",
            arena.ty(argument.ty)
        );
        assert_eq!(
            semantic_type(arena, &parameters, wrapper.return_type),
            Ok(Ty::String)
        );
        let forwarded = single_return(arena, wrapper);
        let KlibIrExprKind::Call {
            access,
            super_qualifier: None,
        } = &arena.expr(forwarded).kind
        else {
            panic!("the reference forwards one exact declaration call")
        };
        assert_eq!(&access.symbol, reflected);
        assert!(access.type_arguments.is_empty());
        match &access.arguments {
            KlibIrArguments::Flat(values) => {
                let [Some(receiver), Some(value)] = values.as_slice() else {
                    panic!("the extension receiver and its one argument")
                };
                assert_eq!(read_symbol(arena, *receiver), &stored_receiver.base.symbol);
                assert_eq!(read_symbol(arena, *value), &argument.base.symbol);
            }
            KlibIrArguments::Split {
                dispatch_receiver: None,
                extension_receiver: Some(receiver),
                values,
            } => {
                let [Some(value)] = values.as_slice() else {
                    panic!("one invocation argument")
                };
                assert_eq!(read_symbol(arena, *receiver), &stored_receiver.base.symbol);
                assert_eq!(read_symbol(arena, *value), &argument.base.symbol);
            }
            other => panic!("an exact bound extension forwarding shape: {other:?}"),
        }
    }
    assert_bound_lowering(&libraries);
}

fn assert_bound_lowering(libraries: &KlibLibraries) {
    let declarations = reference_declarations(
        libraries,
        &[
            "makeBoundReference",
            "inlineMakeBoundReference",
            "makeOtherBoundReference",
            "makeComputedBoundReference",
            "inlineMakeComputedBoundReference",
            "boundReferenceThenLocalClass",
        ],
    );
    let externals = reference_externals(libraries, &declarations);
    let target = selected(libraries, "boundReferenceAppend").0;
    let other = selected(libraries, "otherBoundReferenceAppend").0;
    for (name, expected, inline) in [
        ("makeBoundReference", target, false),
        ("inlineMakeBoundReference", target, true),
        ("makeOtherBoundReference", other, false),
        ("makeComputedBoundReference", target, false),
        ("inlineMakeComputedBoundReference", target, true),
    ] {
        let (_, declaration) = selected(libraries, name);
        let callable = declaration.klib_body_callable().unwrap();
        let facts = bound_facts(callable, &declarations);
        let mut unit = DependencyBodyUnit::default();
        unit.lower_function_with_intrinsics_and_externals(
            callable,
            libraries.declaration_bodies(),
            &facts,
            libraries.intrinsic_facts(),
            &externals,
        )
        .unwrap();
        assert_bound_reference(unit.ir(), expected);
        if inline {
            let mut prepared = IrFile::default();
            lower_inline_template(
                &mut prepared,
                &declaration,
                libraries.inline_declaration_bodies(),
                TemplateCallees {
                    declarations: libraries.declaration_bodies(),
                    references: libraries.reference_bindings(),
                    source: libraries,
                },
            )
            .unwrap()
            .unwrap();
            assert_bound_reference(&prepared, expected);
        }
    }
    let (_, declaration) = selected(libraries, "boundReferenceThenLocalClass");
    let callable = declaration.klib_body_callable().unwrap();
    let mut unit = DependencyBodyUnit::default();
    let decline = unit
        .lower_function_with_intrinsics_and_externals(
            callable,
            libraries.declaration_bodies(),
            &bound_facts(callable, &declarations),
            libraries.intrinsic_facts(),
            &externals,
        )
        .unwrap_err();
    assert_eq!(
        decline.reason(),
        &KlibBodyDeclineReason::UnsupportedOperation("a local class")
    );
    crate::klib_lowering::tests::demo_package::assert_unit_is_empty(&unit);
    assert!(unit.ir().callable_reference_adapter_results.is_empty());
    let (_, next) = selected(libraries, "makeBoundReference");
    let callable = next.klib_body_callable().unwrap();
    unit.lower_function_with_intrinsics_and_externals(
        callable,
        libraries.declaration_bodies(),
        &bound_facts(callable, &declarations),
        libraries.intrinsic_facts(),
        &externals,
    )
    .unwrap();
    assert_bound_reference(unit.ir(), target);
}

fn bound_facts<'a>(
    callable: crate::libraries::KlibBodyCallable<'a>,
    declarations: &'a [(ExternalCallableId, ExternalCallableRealization)],
) -> KlibCalleeFacts<'a> {
    KlibCalleeFacts::with_reference_declarations(
        [callable],
        declarations.iter().filter_map(|(target, declaration)| {
            declaration
                .klib_body_callable()
                .map(|callable| (*target, callable))
        }),
    )
}

fn assert_bound_reference(ir: &IrFile, expected: ExternalCallableId) {
    validated(ir);
    let references = ir
        .exprs
        .iter()
        .filter_map(|expression| {
            if let IrExpr::CallableReference(reference) = expression {
                Some(reference)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    let [reference] = references.as_slice() else {
        panic!("one exact bound reference")
    };
    assert!(matches!(reference.target,
        crate::ir::IrCallableReferenceTarget::External { declaration, receiver: Some(Ty::String) }
            if declaration == expected));
    assert!(reference.captures.is_empty());
    assert_eq!(
        ir.checked_type(reference.bound_receiver.unwrap()),
        Some(Ty::String)
    );
    assert_eq!(
        reference.function_type,
        Ty::fun(vec![Ty::String], Ty::String)
    );
    assert_eq!(reference.declaration_parameters.as_ref(), &[Ty::String]);
    assert_eq!(reference.declaration_result, Ty::String);
    assert!(!reference.declaration_suspend);
    assert!(reference.adaptation.is_none());
    assert_eq!(reference.reflection_owner, None);
    let adapter = &ir.functions[reference.adapter as usize];
    assert_eq!(adapter.params, vec![Ty::String, Ty::String]);
    assert_eq!(adapter.ret, Ty::String);
    assert_eq!(ir.lambda_own_params_from[&reference.adapter], 1);
    assert_eq!(
        ir.fn_params[&reference.adapter].identities,
        vec![
            crate::ir::IrParameterIdentity::generated(
                crate::ir::IrGeneratedParameterRole::ReferenceBoundReceiver,
                None
            ),
            crate::ir::IrParameterIdentity::generated(
                crate::ir::IrGeneratedParameterRole::ReferenceInvokeValue { ordinal: 0 },
                None
            ),
        ]
    );
    assert!(ir.fn_params[&reference.adapter]
        .captured_receivers
        .is_empty());
    let body = adapter.body.unwrap();
    assert_eq!(
        ir.checked_type(ir.callable_reference_adapter_results[&body]),
        Some(Ty::String)
    );
    assert!(ir.callable_scopes.contains(&body));
}

fn single_return(arena: &KlibIrArena, function: &KlibIrFunction) -> KlibIrExprId {
    let Some(KlibIrBody::Block(statements)) = &function.body else {
        panic!("a block-bodied producer declaration")
    };
    let [KlibIrStatement::Expression(returned)] = statements.as_slice() else {
        panic!("a single producer return: {statements:?}")
    };
    let KlibIrExprKind::Return { target, value } = &arena.expr(*returned).kind else {
        panic!("a producer return expression")
    };
    assert_eq!(target, &function.base.symbol);
    *value
}

fn read_symbol(arena: &KlibIrArena, expression: KlibIrExprId) -> &KlibIrSymbol {
    let KlibIrExprKind::GetValue { symbol, .. } = &arena.expr(expression).kind else {
        panic!("one exact parameter read: {:?}", arena.expr(expression))
    };
    symbol
}
