//! Templates of the stdlib's inline functions: a body that only uses forms body lowering models is
//! lowered into the calling file with its provider record's type parameters and expansion modes,
//! and any other body leaves the file as it was.

use super::*;
use crate::libraries::ExternalCallableRealization;
use crate::types::{InlineExpansionMode, InlineParameterModifier};

/// The provider record of the one `kotlin` package function `name`.
fn kotlin_function(libraries: &KlibLibraries, name: &str) -> ExternalCallableRealization {
    let symbols = libraries.symbols(SymbolNamespace::Package(type_name("kotlin")), name);
    let [function] = symbols.callables.functions() else {
        panic!("expected one `kotlin.{name}`");
    };
    let target = function
        .callable
        .external_identity
        .expect("a KLIB function carries its provider identity");
    libraries
        .external_callable(target)
        .expect("the provider realizes its own identity")
}

/// Lower the template of `selected` with the provider's own identities for its calls.
fn template(
    ir: &mut IrFile,
    libraries: &KlibLibraries,
    selected: &ExternalCallableRealization,
) -> Result<KlibInlineTemplate, crate::klib_lowering::KlibBodyDecline> {
    let callees = TemplateCallees {
        declarations: libraries.declaration_bodies(),
        references: libraries.reference_bindings(),
        source: libraries,
    };
    lower_inline_template(ir, selected, libraries.inline_declaration_bodies(), callees)
        .expect("a KLIB function")
}

#[test]
fn default_null_checks_lower_branch_local_returns_in_prepared_inline_blocks() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    for name in ["requireNotNull", "checkNotNull"] {
        let symbols = libraries.symbols(SymbolNamespace::Package(type_name("kotlin")), name);
        let selected = symbols
            .callables
            .functions()
            .iter()
            .filter(|function| function.callable.params.len() == 1)
            .collect::<Vec<_>>();
        let [selected] = selected.as_slice() else {
            panic!("expected one single-argument `kotlin.{name}`");
        };
        let selected = libraries
            .external_callable(
                selected
                    .callable
                    .external_identity
                    .expect("provider identity"),
            )
            .expect("the selected declaration is realized");
        let mut ir = IrFile::default();
        let template = template(&mut ir, &libraries, &selected)
            .unwrap_or_else(|decline| panic!("{name}: {decline}"));
        assert_eq!(
            ir.functions[template.function as usize].ret,
            selected.callable.ret
        );
        validated(&ir);
    }
}

#[test]
fn array_sum_template_keeps_its_selected_receiver_contract() {
    extension_template(
        "kotlin/collections",
        "sumOf",
        Ty::obj("kotlin/IntArray"),
        &[Ty::fun(vec![Ty::Int], Ty::Int)],
    );
}

#[test]
fn map_keeps_the_authenticated_collection_sizing_call_and_receiver_coordinate() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let package = SymbolNamespace::Package(type_name("kotlin/collections"));
    let helper_symbols = libraries.symbols(package, "collectionSizeOrDefault");
    let [helper] = helper_symbols.callables.functions() else {
        panic!("expected one collectionSizeOrDefault declaration");
    };
    let target = helper.callable.external_identity.expect("helper identity");
    let helper = libraries
        .external_callable(target)
        .expect("helper realization");
    assert_eq!(
        helper.callable.compiler_intrinsic,
        Some(crate::libraries::CompilerIntrinsic::CollectionSizeOrDefault)
    );
    let map_symbols = libraries.symbols(package, "map");
    let selected = map_symbols
        .callables
        .functions()
        .iter()
        .filter(|function| {
            function.semantic_receiver().and_then(Ty::obj_internal)
                == Some(type_name("kotlin/collections/Iterable"))
        })
        .collect::<Vec<_>>();
    let [selected] = selected.as_slice() else {
        panic!("expected one Iterable.map");
    };
    let selected = libraries
        .external_callable(selected.callable.external_identity.expect("map identity"))
        .expect("map realization");
    let mut ir = IrFile::default();
    let lowered = template(&mut ir, &libraries, &selected)
        .unwrap_or_else(|decline| panic!("Iterable.map: {decline}"));
    assert_eq!(
        ir.functions[lowered.function as usize].params,
        selected.callable.params
    );
    let calls = ir
        .exprs
        .iter()
        .filter_map(|expression| {
            let IrExpr::Call {
                callee,
                dispatch_receiver,
                args,
            } = expression
            else {
                return None;
            };
            let Callee::External {
                target: called,
                params,
                ret,
                extension_receiver_parameter,
                substitutions,
                ..
            } = callee.as_ref()
            else {
                return None;
            };
            (*called == target).then_some((
                dispatch_receiver.is_some(),
                params.clone(),
                *ret,
                *extension_receiver_parameter,
                args.len(),
                substitutions.clone(),
            ))
        })
        .collect::<Vec<_>>();
    // The prepared stdlib body explicitly serializes TYPE_ARG Any? on this call, rather than
    // map's reconstructed declaration formal. Preserve that instantiation as well as its target.
    let any = Ty::nullable(Ty::obj_name(crate::types::wk::any()));
    assert_eq!(
        calls,
        vec![(
            false,
            vec![Ty::obj_args("kotlin/collections/Iterable", &[any]), Ty::Int],
            Ty::Int,
            Some(0),
            2,
            vec![crate::ir::IrCheckedSubstitution {
                parameter: crate::fir::FirTypeParameterRef::External {
                    callable: target,
                    ordinal: 0
                },
                reified: false,
                value: any,
                reified_runtime: any,
                additional_bounds: Vec::new(),
            }]
        )]
    );
    validated(&ir);
}

#[test]
fn collection_to_typed_array_uses_the_selected_array_allocation() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let symbols = libraries.symbols(
        SymbolNamespace::Package(type_name("kotlin/collections")),
        "toTypedArray",
    );
    let selected = symbols
        .callables
        .functions()
        .iter()
        .filter(|function| {
            function.semantic_receiver().is_some_and(|receiver| {
                receiver.obj_internal() == Some(type_name("kotlin/collections/Collection"))
            })
        })
        .collect::<Vec<_>>();
    let [selected] = selected.as_slice() else {
        panic!("expected one Collection.toTypedArray declaration");
    };
    let selected = libraries
        .external_callable(
            selected
                .callable
                .external_identity
                .expect("provider identity"),
        )
        .expect("the selected declaration is realized");
    let mut ir = IrFile::default();
    template(&mut ir, &libraries, &selected)
        .unwrap_or_else(|decline| panic!("Collection.toTypedArray: {decline}"));
    let array = libraries
        .classifier(crate::types::wk::array())
        .expect("Array declaration");
    let constructors = array
        .constructors
        .iter()
        .filter(|constructor| constructor.params == [Ty::Int])
        .collect::<Vec<_>>();
    let [constructor] = constructors.as_slice() else {
        panic!("expected one Array allocation constructor");
    };
    let realized = libraries
        .external_callable(constructor.external_identity.expect("constructor identity"))
        .expect("the selected constructor is realized");
    assert_eq!(
        realized.callable.semantic_role,
        Some(crate::types::SemanticCallRole::KotlinArrayAllocation)
    );
    let allocations = ir
        .exprs
        .iter()
        .filter_map(|expression| match expression {
            IrExpr::NewArray { array_type, .. } => Some(*array_type),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        allocations,
        vec![Ty::array(Ty::nullable(Ty::obj_name(
            crate::types::wk::any()
        )))]
    );
    validated(&ir);
}

#[test]
fn unsigned_array_iteration_template_keeps_its_selected_receiver_contract() {
    extension_template(
        "kotlin/collections",
        "forEach",
        Ty::obj("kotlin/UByteArray"),
        &[Ty::fun(vec![Ty::UByte], Ty::Unit)],
    );
}

#[test]
fn text_trim_template_keeps_its_selected_receiver_contract() {
    extension_template("kotlin/text", "trim", Ty::String, &[]);
}

fn extension_template(package: &str, name: &str, receiver: Ty, parameters: &[Ty]) {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let symbols = libraries.symbols(SymbolNamespace::Package(type_name(package)), name);
    let selected = symbols
        .callables
        .functions()
        .iter()
        .filter(|function| {
            function.semantic_receiver() == Some(receiver)
                && function.semantic_params().as_ref() == parameters
        })
        .collect::<Vec<_>>();
    let [selected] = selected.as_slice() else {
        panic!("expected one `{package}.{name}` on {receiver:?}");
    };
    let selected = libraries
        .external_callable(
            selected
                .callable
                .external_identity
                .expect("provider identity"),
        )
        .expect("the selected declaration is realized");
    let mut ir = IrFile::default();
    template(&mut ir, &libraries, &selected)
        .unwrap_or_else(|decline| panic!("{name} on {receiver:?}: {decline}"));
    validated(&ir);
}

#[test]
fn a_scope_function_body_is_a_generic_template_that_splices_its_lambda() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    for (name, spellings, rendered) in [
        ("let", &["T", "R"][..], "scope {scope {return@Some(0) invoke(v1!: (T) -> R, v0: T): R: Nothing}: Nothing}"),
        ("also", &["T"][..], "scope {scope {invoke(v1!: (T) -> Unit, v0: T): Unit; return@Some(0) v0: T: Nothing}: Nothing}"),
        (
            "takeIf",
            &["T"][..],
            "scope {scope {return@Some(0) when[invoke(v1!: (T) -> Boolean, v0: T): Boolean -> \
             ImplicitCoercion<T?>(v0: T), else -> Null: Null]: T?: Nothing}: Nothing}",
        ),
    ] {
        let selected = kotlin_function(&libraries, name);
        let mut ir = IrFile::default();
        let template = template(&mut ir, &libraries, &selected).unwrap_or_else(|decline| panic!("{name}: {decline}"));
        let generic = selected.callable.generic_sig.as_deref().expect("generic");
        assert_eq!(template.type_parameters, generic.formals, "{name}");
        let function = &ir.functions[template.function as usize];
        assert_eq!(function.params, selected.callable.params, "{name}");
        assert_eq!(function.ret, selected.callable.ret, "{name}");
        assert!(ir.inline_fns.contains(&template.function), "{name}");
        assert!(ir.inline_only_fns.contains(&template.function), "{name}");
        assert_eq!(
            ir.inline_expansion_modes(template.function),
            Some(&[InlineExpansionMode::Receiver, InlineExpansionMode::Splice][..]),
            "{name}"
        );
        assert_eq!(
            ir.fn_params[&template.function].inline_modifiers,
            [InlineParameterModifier::None, InlineParameterModifier::None],
            "{name}"
        );
        let mut body = normalized_labels(&render(&ir, function.body.expect("a template has a body")));
        for (formal, spelling) in generic.formals.iter().zip(spellings) {
            body = body.replace(&format!("<{formal:?}>"), spelling);
        }
        assert_eq!(body, rendered, "{name}");
        validated(&ir);
    }
}

#[test]
fn repeat_uses_the_canonical_declaration_of_its_inherited_iterator() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let selected = kotlin_function(&libraries, "repeat");
    let mut ir = IrFile::default();
    // The original `repeat` body now lowers: its exact `IntRange.iterator` reference is bound at
    // provider publication to the real `IntProgression.iterator`, not a fabricated range member.
    let template = template(&mut ir, &libraries, &selected).expect("the real repeat template");
    let function = &ir.functions[template.function as usize];
    assert_eq!(function.params, selected.callable.params);
    assert_eq!(function.ret, selected.callable.ret);
    assert!(ir.inline_only_fns.contains(&template.function));
    assert!(ir.exprs.iter().any(|expression| {
        let IrExpr::Call { callee, .. } = expression else {
            return false;
        };
        let Callee::External { target, .. } = callee.as_ref() else {
            return false;
        };
        let callable = libraries
            .external_callable(*target)
            .expect("a provider identity");
        matches!(callable.declaration_signature,
            Some(KlibDeclarationSignature::Public(signature))
                if signature.matches_exact(&["kotlin", "ranges"],
                    &["IntProgression", "iterator"], 8062689420742628886, 0))
    }));
    validated(&ir);
}

#[test]
fn a_full_signature_mismatch_does_not_reuse_a_same_named_template() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let mut selected = kotlin_function(&libraries, "repeat");
    let forged = super::demo_package::operator(&["kotlin"], None, "repeat", &[]).0;
    let Some(KlibDeclarationSignature::Public(original)) = &selected.declaration_signature else {
        panic!("repeat has a public identity");
    };
    assert_eq!(forged.package(), original.package());
    assert_eq!(forged.declaration(), original.declaration());
    assert_ne!(forged.member_id(), original.member_id());
    let forged = KlibDeclarationSignature::Public(forged);
    selected.declaration_signature = Some(forged.clone());
    selected.dependency_callable.as_mut().unwrap().declaration = forged;
    let mut ir = IrFile::default();
    let decline = template(&mut ir, &libraries, &selected)
        .expect_err("an equal qualified spelling cannot authorize another full identity");
    assert_eq!(decline.reason(), &KlibBodyDeclineReason::Unjoined);
    assert!(ir.exprs.is_empty());
    assert!(ir.functions.is_empty());
    assert!(ir.fn_params.is_empty());
    assert!(ir.inline_only_fns.is_empty());
}

#[test]
fn a_member_copy_of_another_class_than_its_selected_owner_declines() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let class = libraries
        .symbols(SymbolNamespace::Package(type_name("kotlin")), "UInt")
        .classifier
        .clone()
        .expect("`kotlin.UInt` is declared");
    let (functions, _) = class
        .declared_callables
        .get("toInt")
        .expect("`UInt` declares `toInt`")
        .clone()
        .into_parts();
    let [function] = functions.overloads.as_slice() else {
        panic!("expected one `kotlin.UInt.toInt`");
    };
    let target = function
        .callable
        .external_identity
        .expect("a KLIB member carries its provider identity");
    let mut selected = libraries
        .external_callable(target)
        .expect("the provider realizes its own identity");
    assert_eq!(
        selected.kind,
        crate::libraries::ExternalCallableKind::Member
    );
    assert_eq!(
        selected.declaration_owner,
        Some(crate::types::SemanticCallableOwner::Classifier(type_name(
            "kotlin/UInt"
        )))
    );
    selected.declaration_owner = Some(crate::types::SemanticCallableOwner::Classifier(type_name(
        "kotlin/Int",
    )));
    let mut ir = IrFile::default();
    let decline = template(&mut ir, &libraries, &selected)
        .expect_err("the serialized copy is a member of another class");
    assert_eq!(
        decline.reason(),
        &KlibBodyDeclineReason::SignatureMismatch(format!(
            "a prepared inline copy is a member of {:?}, selected as one of {:?}",
            type_name("kotlin/UInt"),
            Some(crate::types::SemanticCallableOwner::Classifier(type_name(
                "kotlin/Int"
            )))
        ))
    );
    assert!(ir.functions.is_empty());
}

#[test]
fn a_context_function_value_is_invoked_with_its_context_arguments() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let symbols = libraries.symbols(SymbolNamespace::Package(type_name("kotlin")), "context");
    assert!(!symbols.callables.functions().is_empty());
    for function in symbols.callables.functions() {
        let target = function
            .callable
            .external_identity
            .expect("a KLIB function carries its provider identity");
        let selected = libraries
            .external_callable(target)
            .expect("the provider realizes its own identity");
        let mut ir = IrFile::default();
        let template = template(&mut ir, &libraries, &selected).unwrap_or_else(|decline| {
            panic!("`context`/{}: {decline}", selected.callable.params.len())
        });
        let body = ir.functions[template.function as usize]
            .body
            .expect("a template has a body");
        let invocations = ir
            .exprs
            .iter()
            .filter(|expression| matches!(expression, IrExpr::InvokeFunction { .. }))
            .count();
        assert_eq!(invocations, 1, "{}", render(&ir, body));
    }
}

#[test]
fn a_dependency_constructor_call_constructs_by_the_provider_identity() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let selected = kotlin_function(&libraries, "error");
    let mut ir = IrFile::default();
    let template = template(&mut ir, &libraries, &selected)
        .unwrap_or_else(|decline| panic!("`error`: {decline}"));
    let function = &ir.functions[template.function as usize];
    let construction = ir
        .exprs
        .iter()
        .find_map(|expression| match expression {
            IrExpr::New {
                internal,
                args,
                ctor_params,
                external_target: Some(target),
                defaults,
                ..
            } => Some((
                *internal,
                args.len(),
                ctor_params.clone(),
                *target,
                defaults.clone(),
            )),
            _ => None,
        })
        .expect("`error` constructs its exception");
    let (internal, supplied, params, target, defaults) = construction;
    assert_eq!(internal, type_name("kotlin/IllegalStateException"));
    assert_eq!(supplied, 1);
    assert_eq!(params, Some(vec![Ty::nullable(Ty::String)]));
    assert!(defaults.is_empty());
    let constructor = libraries
        .external_callable(target)
        .expect("the provider realizes the constructor identity");
    assert_eq!(
        constructor.kind,
        crate::libraries::ExternalCallableKind::Constructor
    );
    assert_eq!(
        constructor.declaration_owner,
        Some(crate::types::SemanticCallableOwner::Classifier(type_name(
            "kotlin/IllegalStateException"
        )))
    );
    assert!(function.body.is_some());
    validated(&ir);
}

#[test]
fn a_dependency_property_read_reads_the_provider_property() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let symbols = libraries.symbols(
        SymbolNamespace::Package(type_name("kotlin/text")),
        "isNotEmpty",
    );
    let char_sequence = Ty::obj("kotlin/CharSequence");
    let [function] = symbols
        .callables
        .functions()
        .iter()
        .filter(|function| function.receiver.map(Ty::non_null) == Some(char_sequence))
        .collect::<Vec<_>>()[..]
    else {
        panic!("expected one `kotlin.text.isNotEmpty` of a `CharSequence`");
    };
    let selected = libraries
        .external_callable(
            function
                .callable
                .external_identity
                .expect("a KLIB identity"),
        )
        .expect("the provider realizes its own identity");
    let mut ir = IrFile::default();
    let template = template(&mut ir, &libraries, &selected)
        .unwrap_or_else(|decline| panic!("`isNotEmpty`: {decline}"));
    let reads = ir
        .exprs
        .iter()
        .filter_map(|expression| match expression {
            IrExpr::Checked(operation) => match operation.as_ref() {
                crate::ir::IrCheckedOperation::ExternalPropertyRead {
                    target,
                    dispatch: crate::ir::IrPropertyDispatch::Ordinary,
                    receiver: Some(_),
                    arguments,
                    parameters,
                    result,
                    source_receiver,
                } => Some((
                    *target,
                    arguments.len(),
                    parameters.clone(),
                    *result,
                    *source_receiver,
                )),
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>();
    let [(target, arguments, parameters, result, source_receiver)] = &reads[..] else {
        panic!("`isNotEmpty` reads one property, read {reads:?}");
    };
    assert_eq!(
        (*arguments, parameters.as_slice(), *result),
        (0, &[][..], Ty::Int)
    );
    assert_eq!(*source_receiver, Some(char_sequence));
    let property = crate::symbol_source::SymbolSource::external_property(&libraries, *target)
        .expect("the provider realizes the property identity");
    assert_eq!(property.name, "length");
    assert!(property.setter.is_none());
    assert!(ir.functions[template.function as usize].body.is_some());
    validated(&ir);
}

#[test]
fn a_member_of_a_generic_class_is_called_at_its_receiver_type_arguments() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let symbols = libraries.symbols(
        SymbolNamespace::Package(type_name("kotlin/collections")),
        "isNotEmpty",
    );
    let collection = type_name("kotlin/collections/Collection");
    let [function] = symbols
        .callables
        .functions()
        .iter()
        .filter(|function| {
            function
                .receiver
                .and_then(|receiver| receiver.non_null().kotlin_class_internal())
                == Some(collection)
        })
        .collect::<Vec<_>>()[..]
    else {
        panic!("expected one `kotlin.collections.isNotEmpty` of a `Collection`");
    };
    let selected = libraries
        .external_callable(
            function
                .callable
                .external_identity
                .expect("a KLIB identity"),
        )
        .expect("the provider realizes its own identity");
    let mut ir = IrFile::default();
    let template = template(&mut ir, &libraries, &selected)
        .unwrap_or_else(|decline| panic!("`isNotEmpty`: {decline}"));
    assert_eq!(template.type_parameters.len(), 1);
    let calls = ir
        .exprs
        .iter()
        .filter_map(|expression| match expression {
            IrExpr::Call {
                callee,
                dispatch_receiver: Some(_),
                args,
            } => match callee.as_ref() {
                Callee::External {
                    target,
                    params,
                    ret,
                    ..
                } => Some((*target, params.clone(), *ret, args.len())),
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>();
    let [(target, params, ret, arguments)] = &calls[..] else {
        panic!("`isNotEmpty` calls one member, called {calls:?}");
    };
    assert_eq!(
        (params.as_slice(), *ret, *arguments),
        (&[][..], Ty::Boolean, 0)
    );
    let member = libraries
        .external_callable(*target)
        .expect("the provider realizes the member identity");
    assert_eq!(member.callable.name, "isEmpty");
    assert_eq!(
        member.declaration_owner,
        Some(crate::types::SemanticCallableOwner::Classifier(collection))
    );
    validated(&ir);
}
