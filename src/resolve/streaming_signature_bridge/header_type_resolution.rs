//! Semantic resolution of declaration types for the transitional publisher.
//!
//! Parser and compact callers share this algorithm through `SignatureTypeSyntax`. Compact Pass 1
//! therefore consumes `HeaderTypeId` directly without either rebuilding parser `TypeRef` trees or
//! introducing a second type resolver during the migration.

use crate::ast::TypeRef;
use crate::diag::DiagSink;
use crate::fir::{HeaderTypeId, StreamedHeaderModule};
use crate::types::{Ty, TypeName};

use super::super::{
    apply_alias_expansion, classifier_over_default, default_classifier_internal, ClassNames,
    TParams,
};
use super::signature_type_syntax::SignatureTypeSyntax;

fn classifier_for_syntax(
    name: &str,
    has_arguments: bool,
    resolved: Option<TypeName>,
) -> Option<TypeName> {
    if has_arguments {
        resolved.or_else(|| default_classifier_internal(name))
    } else {
        classifier_over_default(name, resolved)
    }
}

fn projected_argument(
    argument: SignatureTypeSyntax<'_>,
    classes: &ClassNames,
    tparams: &TParams,
    diags: &mut DiagSink,
) -> Ty {
    let star = argument
        .star_projection()
        .expect("a declaration type argument must retain its syntax");
    let resolved = if star {
        Ty::Error
    } else {
        resolve_signature_type_with(argument, classes, tparams, diags)
    };
    argument
        .projected(resolved, Ty::nullable(Ty::obj("kotlin/Any")))
        .expect("a declaration type argument must retain its projection")
}

pub(in crate::resolve) fn resolve_signature_type_with(
    reference: SignatureTypeSyntax<'_>,
    classes: &ClassNames,
    tparams: &TParams,
    diags: &mut DiagSink,
) -> Ty {
    let Some(name) = reference.spelling() else {
        return Ty::Error;
    };
    let Some(span) = reference.span() else {
        return Ty::Error;
    };
    if reference.definitely_non_null() == Some(true) && !tparams.contains(&name) {
        diags.error(
            span,
            "a definitely non-null type must use a type parameter on the left of '& Any'",
        );
        return Ty::Error;
    }
    let (resolved_classifier, failed_segment) = match classes.classifier_binding(&name) {
        Ok(classifier) => (Some(classifier), None),
        Err(segment) => (None, Some(segment.to_owned())),
    };
    let arguments = reference.arguments().unwrap_or_default();
    let scoped = if tparams.contains(&name) {
        Some(tparams.bound(&name))
    } else {
        classifier_for_syntax(&name, !arguments.is_empty(), resolved_classifier).map(|internal| {
            let arguments = arguments
                .iter()
                .copied()
                .map(|argument| projected_argument(argument, classes, tparams, diags))
                .collect::<Vec<_>>();
            apply_alias_expansion(
                classes,
                &name,
                internal,
                &arguments,
                reference.is_import().unwrap_or(false),
                span,
                diags,
            )
        })
    };
    let function = reference.function_shape().flatten().map(|function| {
        let parameters = function
            .parameters
            .iter()
            .copied()
            .map(|parameter| {
                if parameter.star_projection() == Some(true) {
                    Ty::nullable(Ty::obj("kotlin/Any"))
                } else {
                    resolve_signature_type_with(parameter, classes, tparams, diags)
                }
            })
            .collect::<Vec<_>>();
        let result = function.result.map(|result| {
            if result.star_projection() == Some(true) {
                Ty::nullable(Ty::obj("kotlin/Any"))
            } else {
                resolve_signature_type_with(result, classes, tparams, diags)
            }
        });
        Ty::fun_with_shape(
            parameters,
            result.unwrap_or(Ty::Unit),
            function.context_count,
            function.has_receiver,
            function.suspend,
        )
    });
    let base = if let Some(function) = function {
        function
    } else if let Some(scoped) = scoped {
        scoped
    } else if let Some(builtin) = Ty::from_name(&name) {
        builtin
    } else if let Some(element) = Ty::primitive_array_element(&name) {
        Ty::array(element)
    } else if let Some(internal) = resolved_classifier.or_else(|| classes.get(&name)) {
        if let Some(primitive) = internal.strip_prefix("__ty/") {
            Ty::from_name(&primitive).unwrap_or(Ty::Error)
        } else {
            let arguments = arguments
                .iter()
                .copied()
                .map(|argument| projected_argument(argument, classes, tparams, diags))
                .collect::<Vec<_>>();
            apply_alias_expansion(
                classes,
                &name,
                internal,
                &arguments,
                reference.is_import().unwrap_or(false),
                span,
                diags,
            )
        }
    } else {
        diags.error(
            span,
            format!(
                "unresolved reference '{}'.",
                failed_segment.as_deref().unwrap_or(&name)
            ),
        );
        Ty::Error
    };
    let base = if reference.definitely_non_null() == Some(true) {
        match base {
            Ty::TyParam(name, bound) => Ty::ty_param(name, bound.non_null()),
            other => other,
        }
    } else {
        base
    };
    if reference.nullable() == Some(true) && base != Ty::Error {
        Ty::nullable(base)
    } else {
        base
    }
}

pub(in crate::resolve) fn resolve_header_type_with(
    headers: &StreamedHeaderModule,
    syntax: HeaderTypeId,
    classes: &ClassNames,
    tparams: &TParams,
    diags: &mut DiagSink,
) -> Ty {
    resolve_signature_type_with(
        SignatureTypeSyntax::compact(&headers.syntax, &headers.lookup_names, syntax),
        classes,
        tparams,
        diags,
    )
}

pub(in crate::resolve) fn resolve_parser_type_with(
    reference: &TypeRef,
    classes: &ClassNames,
    tparams: &TParams,
    diags: &mut DiagSink,
) -> Ty {
    resolve_signature_type_with(
        SignatureTypeSyntax::parser(reference),
        classes,
        tparams,
        diags,
    )
}

pub(in crate::resolve) fn resolve_header_function_parameter_types(
    headers: &StreamedHeaderModule,
    syntax: HeaderTypeId,
    classes: &ClassNames,
    tparams: &TParams,
    diags: &mut DiagSink,
) -> Vec<Ty> {
    let syntax = SignatureTypeSyntax::compact(&headers.syntax, &headers.lookup_names, syntax);
    syntax
        .function_shape()
        .flatten()
        .map(|function| {
            function
                .parameters
                .into_iter()
                .map(|parameter| resolve_signature_type_with(parameter, classes, tparams, diags))
                .collect()
        })
        .unwrap_or_default()
}

pub(in crate::resolve) fn header_type_has_function_receiver(
    headers: &StreamedHeaderModule,
    syntax: HeaderTypeId,
) -> bool {
    SignatureTypeSyntax::compact(&headers.syntax, &headers.lookup_names, syntax)
        .function_shape()
        .flatten()
        .is_some_and(|function| function.has_receiver)
}

pub(in crate::resolve) fn header_type_formal_occurrences(
    headers: &StreamedHeaderModule,
    syntax: HeaderTypeId,
    name: &str,
    projected: bool,
) -> Option<(bool, bool)> {
    SignatureTypeSyntax::compact(&headers.syntax, &headers.lookup_names, syntax)
        .formal_occurrences(name, projected)
}

pub(in crate::resolve) fn header_type_bare_parameter_spelling(
    headers: &StreamedHeaderModule,
    syntax: HeaderTypeId,
) -> Option<String> {
    SignatureTypeSyntax::compact(&headers.syntax, &headers.lookup_names, syntax)
        .bare_parameter_spelling()
}

pub(in crate::resolve) fn header_type_parameter_spelling_allow_nullable(
    headers: &StreamedHeaderModule,
    syntax: HeaderTypeId,
) -> Option<String> {
    let syntax = SignatureTypeSyntax::compact(&headers.syntax, &headers.lookup_names, syntax);
    if syntax.definitely_non_null()? || syntax.function_shape()?.is_some() {
        return None;
    }
    let spelling = syntax.spelling()?;
    (syntax.arguments()?.is_empty() && !spelling.contains(['.', '/', '$']))
        .then(|| spelling.into_owned())
}

pub(in crate::resolve) fn header_type_bare_classifier_shape(
    headers: &StreamedHeaderModule,
    syntax: HeaderTypeId,
) -> Option<(String, bool, bool)> {
    let syntax = SignatureTypeSyntax::compact(&headers.syntax, &headers.lookup_names, syntax);
    if syntax.function_shape()?.is_some() || !syntax.arguments()?.is_empty() {
        return None;
    }
    let spelling = syntax.spelling()?;
    (!spelling.contains(['.', '/', '$'])).then(|| {
        (
            spelling.into_owned(),
            syntax.nullable().unwrap_or(false),
            syntax.definitely_non_null().unwrap_or(false),
        )
    })
}
