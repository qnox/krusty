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
        source: libraries,
    };
    lower_inline_template(ir, selected, libraries.inline_declaration_bodies(), callees)
        .expect("a KLIB function")
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
fn a_declined_body_leaves_the_file_as_it_was() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let selected = kotlin_function(&libraries, "repeat");
    let mut ir = IrFile::default();
    let decline = template(&mut ir, &libraries, &selected)
        .expect_err("`repeat` uses a form body lowering does not model");
    assert_eq!(
        decline.reason(),
        &KlibBodyDeclineReason::UnsupportedOperation("a block of a compiler-introduced form")
    );
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
            IrExpr::Checked(crate::ir::IrCheckedOperation::ExternalPropertyRead {
                target,
                dispatch: crate::ir::IrPropertyDispatch::Ordinary,
                receiver: Some(_),
                arguments,
                parameters,
                result,
                source_receiver,
            }) => Some((
                *target,
                arguments.len(),
                parameters.clone(),
                *result,
                *source_receiver,
            )),
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
                callee:
                    Callee::External {
                        target,
                        params,
                        ret,
                        ..
                    },
                dispatch_receiver: Some(_),
                args,
            } => Some((*target, params.clone(), *ret, args.len())),
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
