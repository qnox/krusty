//! Explicit API mode (`-Xexplicit-api=strict|warning`), a port of kotlinc's
//! `FirExplicitApiDeclarationChecker`.
//!
//! A declaration that is part of the public API must write its visibility, and one whose type would
//! be inferred must write that too. Public API means effectively public: the declaration and every
//! classifier containing it are `public` or `protected`, and none of them is local. The checks read
//! the modifier list as written, which the parser keeps in [`File::declaration_prefixes`].

use crate::ast::{ClassDecl, Decl, DeclId, File, FunBody, FunDecl, PropDecl};
use crate::diag::{DiagSink, Span};
use crate::features::LangFeatures;
use crate::types::Visibility;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Strict,
    Warning,
}

const VISIBILITY_MESSAGE: &str = "visibility must be specified in explicit API mode.";
const RETURN_TYPE_MESSAGE: &str = "return type must be specified in explicit API mode.";

/// Report every explicit API violation in `file`, in source order.
pub(super) fn check(file: &File, features: &LangFeatures, diagnostics: &mut DiagSink) {
    let mode = if features.has("ExplicitApiStrict") {
        Mode::Strict
    } else if features.has("ExplicitApiWarning") {
        Mode::Warning
    } else {
        return;
    };
    let mut findings = Findings {
        file,
        reports: Vec::new(),
    };
    for &id in &file.decls {
        match file.decl(id) {
            Decl::Fun(function) if !file.is_local_declaration(id) => findings.function(function),
            Decl::Property(property) if !file.is_local_declaration(id) => {
                findings.property(property, false)
            }
            Decl::Class(class) if is_public_api_classifier(file, id) => findings.classifier(class),
            _ => {}
        }
    }
    for alias in &file.type_alias_decls {
        findings.type_alias(alias.span, alias.name_span);
    }
    findings.reports.sort_by_key(|(span, _)| span.lo);
    for (span, message) in findings.reports {
        match mode {
            Mode::Strict => diagnostics.error(span, message),
            Mode::Warning => diagnostics.warning(span, message),
        }
    }
}

/// Whether classifier `id` is effectively public: neither it nor any classifier containing it is
/// local or anonymous, and each is `public` or `protected`. Containment is the recorded owner edge.
fn is_public_api_classifier(file: &File, id: DeclId) -> bool {
    let Decl::Class(class) = file.decl(id) else {
        return false;
    };
    if file.is_local_declaration(id)
        || file.local_class_enclosing_declarations.contains_key(&id)
        || file.enum_entry_nested_classifier_owners.contains_key(&id)
        || file.is_anonymous_object_class(id)
        || !is_public_api(class.visibility)
    {
        return false;
    }
    file.hoisted_classifier_owner(id)
        .is_none_or(|owner| is_public_api_classifier(file, owner))
}

fn is_public_api(visibility: Visibility) -> bool {
    matches!(visibility, Visibility::Public | Visibility::Protected)
}

struct Findings<'f> {
    file: &'f File,
    reports: Vec<(Span, &'static str)>,
}

impl Findings<'_> {
    /// Report a missing visibility for a public-API declaration whose modifier list is found at
    /// `anchor`, the offset of a token that keys it, ending the report at `end`.
    fn visibility(&mut self, anchor: Span, end: Span) {
        let prefix = self.file.declaration_prefixes.at(anchor.lo);
        if prefix.is_some_and(|prefix| prefix.visibility.is_some()) {
            return;
        }
        let start = prefix.map_or(anchor.lo, |prefix| prefix.start.lo);
        self.reports
            .push((Span::new(start, end.hi), VISIBILITY_MESSAGE));
    }

    fn classifier(&mut self, class: &ClassDecl) {
        self.visibility(class.name_span, class.name_span);
        // A `data` or `annotation` class's properties need no visibility.
        let property_carrier = class.is_data || class.is_annotation();
        for parameter in class.props.iter().filter(|parameter| parameter.is_property) {
            if is_public_api(parameter.visibility) && !parameter.is_override && !property_carrier {
                self.visibility(parameter.declaration_span, parameter.span);
            }
        }
        for function in &class.methods {
            self.function(function);
        }
        for property in &class.body_props {
            self.property(property, property_carrier);
        }
        for constructor in &class.secondary_ctors {
            if is_public_api(constructor.visibility) {
                self.visibility(constructor.span, constructor.span);
            }
        }
        for alias in &class.type_aliases {
            self.type_alias(alias.span, alias.name_span);
        }
    }

    fn function(&mut self, function: &FunDecl) {
        if !is_public_api(function.visibility) {
            return;
        }
        if !function.is_override() {
            self.visibility(function.span, function.name_span);
        }
        if function.ret.is_none() && matches!(function.body, FunBody::Expr(_)) {
            self.reports.push((function.name_span, RETURN_TYPE_MESSAGE));
        }
    }

    fn property(&mut self, property: &PropDecl, in_property_carrier: bool) {
        if !is_public_api(property.visibility) {
            return;
        }
        if !property.is_override && !in_property_carrier {
            self.visibility(property.span, property.name_span);
        }
        if property.ty.is_none() {
            self.reports.push((property.name_span, RETURN_TYPE_MESSAGE));
        }
    }

    /// A type alias carries no visibility on the AST; its written modifier list decides both
    /// whether it is public API and whether it said so.
    fn type_alias(&mut self, span: Span, name_span: Span) {
        let visibility = self
            .file
            .declaration_prefixes
            .at(span.lo)
            .and_then(|prefix| prefix.visibility)
            .unwrap_or_default();
        if is_public_api(visibility) {
            self.visibility(span, name_span);
        }
    }
}
