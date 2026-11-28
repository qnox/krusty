//! kotlinc's `UNDERSCORE_IS_RESERVED` on the name a declaration introduces.
//!
//! A classifier, type alias, function or property named with underscores only (`_`, `__`) and
//! without back-ticks is rejected at its name. The parser records that syntax fact on the name's
//! position; a local variable's, lambda's or destructuring's bare `_` is an unused binding instead
//! and is not checked here.

use crate::ast::{ClassDecl, Decl, File};
use crate::diag::{DiagSink, Span};

pub(super) fn check(file: &File, diagnostics: &mut DiagSink) {
    let mut report = |name: Span| {
        if file.unescaped_underscore_declarations.contains(&name.lo) {
            diagnostics.error_named(
                name,
                crate::diag::names::UNDERSCORE_IS_RESERVED,
                "names _, __, ___, ... are reserved in Kotlin.",
            );
        }
    };
    for alias in &file.type_alias_decls {
        report(alias.name_span);
    }
    for declaration in &file.decl_arena {
        match declaration {
            Decl::Fun(function) => report(function.name_span),
            Decl::Property(property) => report(property.name_span),
            Decl::Class(class) => class_members(class, &mut report),
        }
    }
}

fn class_members(class: &ClassDecl, report: &mut impl FnMut(Span)) {
    report(class.name_span);
    for alias in &class.type_aliases {
        report(alias.name_span);
    }
    for function in class
        .methods
        .iter()
        .chain(class.enum_entries.iter().flat_map(|entry| &entry.methods))
    {
        report(function.name_span);
    }
    for property in class
        .body_props
        .iter()
        .chain(class.enum_entries.iter().flat_map(|entry| &entry.props))
    {
        report(property.name_span);
    }
}
