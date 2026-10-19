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
    let provider = |identity: &crate::libraries::KlibDependencyCallable| {
        libraries.external_callable_by_dependency(identity)
    };
    let callees = TemplateCallees {
        declarations: libraries.declaration_bodies(),
        provider: &provider,
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
