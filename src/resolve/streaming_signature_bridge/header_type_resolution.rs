//! Semantic resolution of compact declaration types for the transitional publisher.
//!
//! Every Pass-1 declaration consumer enters through this resolver with `HeaderTypeId`; recursive
//! source-alias substitution stays inside the same algorithm and never rebuilds parser type trees.

use crate::diag::DiagSink;
use crate::fir::{HeaderTypeId, StreamedHeaderModule};
use crate::types::{Ty, TypeName};

use super::super::{
    apply_alias_expansion, classifier_over_default, default_classifier_internal,
    type_parameter_bounds, ClassNames, TParams,
};
use super::signature_type_syntax::SignatureTypeSyntax;
use super::StreamedTypeAliasHeader;

struct CompactAliasResolution<'a> {
    headers: &'a StreamedHeaderModule,
    visible: &'a [StreamedTypeAliasHeader],
}

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
    aliases: Option<&CompactAliasResolution<'_>>,
    alias_depth: u32,
) -> Ty {
    let star = argument
        .star_projection()
        .expect("a declaration type argument must retain its syntax");
    let resolved = if star {
        Ty::Error
    } else {
        resolve_signature_type_with_aliases(argument, classes, tparams, diags, aliases, alias_depth)
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
    resolve_signature_type_with_aliases(reference, classes, tparams, diags, None, 0)
}

fn resolve_signature_type_with_aliases(
    reference: SignatureTypeSyntax<'_>,
    classes: &ClassNames,
    tparams: &TParams,
    diags: &mut DiagSink,
    aliases: Option<&CompactAliasResolution<'_>>,
    alias_depth: u32,
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
    let arguments = reference.arguments().unwrap_or_default();
    let selected_alias = (alias_depth <= 8
        && !tparams.contains(&name)
        && reference.function_shape().flatten().is_none())
    .then_some(aliases)
    .flatten()
    .and_then(|aliases| {
        aliases
            .visible
            .iter()
            .rev()
            .find(|alias| alias.name == name && alias.type_parameters.len() == arguments.len())
            .map(|alias| (aliases, alias))
    });
    if let Some((aliases, alias)) = selected_alias {
        let mut alias_tparams = tparams.clone();
        for (formal, argument) in alias.type_parameters.iter().zip(arguments.iter().copied()) {
            let argument = projected_argument(
                argument,
                classes,
                tparams,
                diags,
                Some(aliases),
                alias_depth + 1,
            );
            alias_tparams.insert_binding(formal, argument, Vec::new());
        }
        let target = SignatureTypeSyntax::compact(
            &aliases.headers.syntax,
            &aliases.headers.lookup_names,
            alias.target,
        );
        let expanded = resolve_signature_type_with_aliases(
            target,
            classes,
            &alias_tparams,
            diags,
            Some(aliases),
            alias_depth + 1,
        );
        let base = expanded.projection_inner().unwrap_or(expanded);
        return apply_reference_flags(reference, base);
    }
    let (resolved_classifier, failed_segment) = match classes.classifier_binding(&name) {
        Ok(classifier) => (Some(classifier), None),
        Err(segment) => (None, Some(segment.to_owned())),
    };
    let scoped = if tparams.contains(&name) {
        Some(tparams.bound(&name))
    } else {
        classifier_for_syntax(&name, !arguments.is_empty(), resolved_classifier).map(|internal| {
            let arguments = arguments
                .iter()
                .copied()
                .map(|argument| {
                    projected_argument(argument, classes, tparams, diags, aliases, alias_depth)
                })
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
                    resolve_signature_type_with_aliases(
                        parameter,
                        classes,
                        tparams,
                        diags,
                        aliases,
                        alias_depth,
                    )
                }
            })
            .collect::<Vec<_>>();
        let result = function.result.map(|result| {
            if result.star_projection() == Some(true) {
                Ty::nullable(Ty::obj("kotlin/Any"))
            } else {
                resolve_signature_type_with_aliases(
                    result,
                    classes,
                    tparams,
                    diags,
                    aliases,
                    alias_depth,
                )
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
                .map(|argument| {
                    projected_argument(argument, classes, tparams, diags, aliases, alias_depth)
                })
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
    apply_reference_flags(reference, base)
}

fn apply_reference_flags(reference: SignatureTypeSyntax<'_>, base: Ty) -> Ty {
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

pub(in crate::resolve) fn resolve_header_type_alias_target_with(
    headers: &StreamedHeaderModule,
    syntax: HeaderTypeId,
    visible_aliases: &[StreamedTypeAliasHeader],
    classes: &ClassNames,
    tparams: &TParams,
    diags: &mut DiagSink,
) -> Ty {
    resolve_signature_type_with_aliases(
        SignatureTypeSyntax::compact(&headers.syntax, &headers.lookup_names, syntax),
        classes,
        tparams,
        diags,
        Some(&CompactAliasResolution {
            headers,
            visible: visible_aliases,
        }),
        0,
    )
}

pub(in crate::resolve) fn resolve_header_type_alias_argument_with(
    headers: &StreamedHeaderModule,
    syntax: HeaderTypeId,
    visible_aliases: &[StreamedTypeAliasHeader],
    classes: &ClassNames,
    tparams: &TParams,
    diags: &mut DiagSink,
) -> Ty {
    projected_argument(
        SignatureTypeSyntax::compact(&headers.syntax, &headers.lookup_names, syntax),
        classes,
        tparams,
        diags,
        Some(&CompactAliasResolution {
            headers,
            visible: visible_aliases,
        }),
        0,
    )
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

pub(in crate::resolve) fn header_type_spelling(
    headers: &StreamedHeaderModule,
    syntax: HeaderTypeId,
) -> Option<String> {
    SignatureTypeSyntax::compact(&headers.syntax, &headers.lookup_names, syntax)
        .spelling()
        .map(|spelling| spelling.into_owned())
}

pub(in crate::resolve) fn header_type_span(
    headers: &StreamedHeaderModule,
    syntax: HeaderTypeId,
) -> Option<crate::diag::Span> {
    SignatureTypeSyntax::compact(&headers.syntax, &headers.lookup_names, syntax).span()
}

pub(in crate::resolve) fn header_type_is_function(
    headers: &StreamedHeaderModule,
    syntax: HeaderTypeId,
) -> bool {
    SignatureTypeSyntax::compact(&headers.syntax, &headers.lookup_names, syntax)
        .function_shape()
        .flatten()
        .is_some()
}

pub(in crate::resolve) fn resolve_header_type_arguments_with(
    headers: &StreamedHeaderModule,
    syntax: HeaderTypeId,
    classes: &ClassNames,
    tparams: &TParams,
    diags: &mut DiagSink,
) -> Vec<Ty> {
    SignatureTypeSyntax::compact(&headers.syntax, &headers.lookup_names, syntax)
        .arguments()
        .unwrap_or_default()
        .into_iter()
        .map(|argument| resolve_signature_type_with(argument, classes, tparams, diags))
        .collect()
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

fn compact_bound_erasure(
    headers: &StreamedHeaderModule,
    syntax: HeaderTypeId,
    resolve: &dyn Fn(&str) -> Option<TypeName>,
) -> Ty {
    let any = Ty::obj("kotlin/Any");
    let syntax = SignatureTypeSyntax::compact(&headers.syntax, &headers.lookup_names, syntax);
    if syntax.nullable() != Some(false) {
        return any;
    }
    let Some(spelling) = syntax.spelling() else {
        return any;
    };
    if let Some(builtin) = Ty::from_name(&spelling) {
        return if builtin.is_specializable_bound() || builtin.is_reference() {
            builtin
        } else {
            any
        };
    }
    resolve(&spelling).map(Ty::obj_name).unwrap_or(any)
}

fn compact_non_nullable_bound_spelling(
    headers: &StreamedHeaderModule,
    syntax: HeaderTypeId,
) -> Option<String> {
    let syntax = SignatureTypeSyntax::compact(&headers.syntax, &headers.lookup_names, syntax);
    (syntax.nullable() == Some(false))
        .then(|| syntax.spelling().map(|spelling| spelling.into_owned()))
        .flatten()
}

pub(in crate::resolve) fn compact_tparams_from_decl_with(
    headers: &StreamedHeaderModule,
    names: &[String],
    bounds: &[(String, HeaderTypeId)],
    resolve: &dyn Fn(&str) -> Option<TypeName>,
) -> TParams {
    type_parameter_bounds::erased_from_syntax(
        names,
        bounds,
        &|bound| compact_non_nullable_bound_spelling(headers, *bound),
        &|bound| compact_bound_erasure(headers, *bound, resolve),
    )
}

pub(in crate::resolve) fn compact_tparams_from_decl_with_primary_class(
    headers: &StreamedHeaderModule,
    names: &[String],
    bounds: &[(String, HeaderTypeId)],
    resolve: &dyn Fn(&str) -> Option<TypeName>,
    is_interface: &dyn Fn(TypeName) -> bool,
) -> TParams {
    type_parameter_bounds::erased_from_syntax_with_primary_class(
        names,
        bounds,
        &|bound| compact_non_nullable_bound_spelling(headers, *bound),
        &|bound| compact_bound_erasure(headers, *bound, resolve),
        &|bound| {
            let syntax =
                SignatureTypeSyntax::compact(&headers.syntax, &headers.lookup_names, *bound);
            syntax.nullable() == Some(false)
                && syntax
                    .spelling()
                    .and_then(|spelling| resolve(&spelling))
                    .is_some_and(|classifier| !is_interface(classifier))
        },
    )
}

pub(in crate::resolve) fn compact_tparams_extended_with(
    enclosing: &TParams,
    headers: &StreamedHeaderModule,
    names: &[String],
    bounds: &[(String, HeaderTypeId)],
    resolve: &dyn Fn(&str) -> Option<TypeName>,
) -> TParams {
    type_parameter_bounds::erased_extended_from_syntax(
        enclosing,
        names,
        bounds,
        &|bound| compact_non_nullable_bound_spelling(headers, *bound),
        &|bound| header_type_bare_parameter_spelling(headers, *bound),
        &|bound| compact_bound_erasure(headers, *bound, resolve),
    )
}

pub(in crate::resolve) fn compact_tparams_class_erased_extended_with(
    enclosing: &TParams,
    headers: &StreamedHeaderModule,
    names: &[String],
    bounds: &[(String, HeaderTypeId)],
    resolve: &dyn Fn(&str) -> Option<TypeName>,
) -> TParams {
    type_parameter_bounds::class_erased_extended_from_syntax(
        enclosing,
        names,
        bounds,
        &|bound| compact_non_nullable_bound_spelling(headers, *bound),
        &|bound| header_type_bare_parameter_spelling(headers, *bound),
        &|bound| compact_bound_erasure(headers, *bound, resolve),
    )
}

fn compact_symbolic_tparams(
    enclosing: &TParams,
    headers: &StreamedHeaderModule,
    names: &[String],
    bounds: &[(String, HeaderTypeId)],
    resolve: &dyn Fn(&str) -> Option<TypeName>,
) -> TParams {
    TParams::symbolic_from_syntax_enclosing(
        names,
        bounds,
        &|bound| header_type_bare_parameter_spelling(headers, *bound),
        &|bound, parameter| {
            SignatureTypeSyntax::compact(&headers.syntax, &headers.lookup_names, *bound)
                .tparam_bound_semantic_with(resolve, parameter)
                .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")))
        },
        &|name| enclosing.contains(name).then(|| enclosing.bound(name)),
    )
}

pub(in crate::resolve) fn compact_tparams_symbolic_from_decl_with(
    headers: &StreamedHeaderModule,
    names: &[String],
    bounds: &[(String, HeaderTypeId)],
    resolve: &dyn Fn(&str) -> Option<TypeName>,
) -> TParams {
    compact_symbolic_tparams(&TParams::default(), headers, names, bounds, resolve)
}

pub(in crate::resolve) fn compact_tparams_symbolic_extended_with(
    enclosing: &TParams,
    headers: &StreamedHeaderModule,
    names: &[String],
    bounds: &[(String, HeaderTypeId)],
    resolve: &dyn Fn(&str) -> Option<TypeName>,
) -> TParams {
    let declared = compact_symbolic_tparams(enclosing, headers, names, bounds, resolve);
    let mut out = enclosing.clone();
    for name in names {
        out.insert_binding(name, declared.bound(name), declared.extra_bounds_of(name));
    }
    out
}

pub(in crate::resolve) fn compact_declared_tparam_semantic_bound(
    headers: &StreamedHeaderModule,
    name: &str,
    names: &[String],
    bounds: &[(String, HeaderTypeId)],
    resolve: &dyn Fn(&str) -> Option<TypeName>,
) -> Option<Ty> {
    let mut current = name.to_string();
    let mut seen = std::collections::HashSet::new();
    loop {
        let bound = bounds
            .iter()
            .find_map(|(parameter, bound)| (parameter == &current).then_some(*bound))?;
        if let Some(parameter) = header_type_bare_parameter_spelling(headers, bound)
            .filter(|parameter| names.iter().any(|candidate| candidate == parameter))
        {
            if !seen.insert(current) {
                return Some(Ty::obj("kotlin/Any"));
            }
            current = parameter;
            continue;
        }
        return SignatureTypeSyntax::compact(&headers.syntax, &headers.lookup_names, bound)
            .tparam_bound_semantic_with(resolve, &mut |_| None);
    }
}
