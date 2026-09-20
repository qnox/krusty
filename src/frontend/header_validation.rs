//! Semantic validation owned by declaration headers.
//!
//! These checks run while one bounded Pass-1 parser unit is live. They must not depend on a body
//! visit: declarations such as a class with only primary-constructor parameters have no ordinary
//! Pass-2 body unit at all.

use crate::ast::{Decl, File};
use crate::diag::DiagSink;

pub(super) fn validate(file: &File, diagnostics: &mut DiagSink) {
    for declaration in &file.decl_arena {
        match declaration {
            Decl::Fun(function) => validate_function(function, false, diagnostics),
            Decl::Property(_) => {}
            Decl::Class(class) => {
                let mut constructor_parameters = std::collections::HashSet::new();
                for parameter in &class.props {
                    if !constructor_parameters.insert(parameter.name.as_str()) {
                        diagnostics.error(
                            class.span,
                            format!(
                                "conflicting declaration: constructor parameter '{}' is declared more than once",
                                parameter.name
                            ),
                        );
                    }
                }
                for function in &class.methods {
                    validate_function(function, true, diagnostics);
                }
                if class.is_interface() {
                    for property in &class.body_props {
                        for diagnostic in
                            crate::declaration_validation::interface_property_diagnostics(property)
                        {
                            diagnostics.error(diagnostic.span, diagnostic.message.to_string());
                        }
                    }
                }
                for entry in &class.enum_entries {
                    for function in &entry.methods {
                        validate_function(function, true, diagnostics);
                    }
                }
            }
        }
    }
}

fn validate_function(function: &crate::ast::FunDecl, member: bool, diagnostics: &mut DiagSink) {
    for diagnostic in crate::declaration_validation::infix_declaration_diagnostics(function, member)
    {
        diagnostics.error(diagnostic.span, diagnostic.message.to_string());
    }
}

/// kotlinc's three sentences for an `expect` declaration that carries an implementation. Measured
/// against the reference compiler; none varies with the declaration's kind.
const BODY_MESSAGE: &str = "expected declaration cannot have a body.";
const INITIALIZER_MESSAGE: &str = "expected property cannot have an initializer.";
const DELEGATE_MESSAGE: &str = "expected property cannot be delegated.";

/// Report every `expect` declaration that carries an implementation, and say whether it found one.
///
/// A header declares; it does not implement. The reference compiler reports this whether or not
/// the multiplatform feature is on — the two diagnostics are independent, and a file without the
/// feature gets both — so this check is purely syntactic and runs unconditionally.
///
/// Positions are measured, not guessed: a top-level function is reported at its `expect` keyword, a
/// member at its own declaration, an accessor at the accessor header (`get()` / `set(v)`), and a
/// property INITIALIZER at the initializer expression rather than at the property. A secondary
/// constructor with a body inside an `expect class` is deliberately absent: the reference compiler
/// does not report it, though an `init` block in the same position it does.
pub(super) fn validate_expect_bodies(file: &File, diagnostics: &mut DiagSink) -> bool {
    let before = diagnostics.diags.len();
    for expect in &file.expect_decls {
        match file.decl(expect.declaration) {
            Decl::Fun(function) => {
                // The whole declaration is the header, so the keyword that introduced it is where
                // the reference compiler points. It travels with the declaration; nothing here
                // searches for it, and nothing substitutes another position when it is absent.
                if !matches!(function.body, crate::ast::FunBody::None) {
                    diagnostics.error(expect.keyword, BODY_MESSAGE);
                }
            }
            Decl::Property(property) => {
                validate_expect_property(file, property, expect.keyword, diagnostics);
            }
            Decl::Class(class) => validate_expect_class(file, class, diagnostics),
        }
    }
    diagnostics.diags.len() > before
}

/// Members of an `expect` classifier are headers too, each reported at its own declaration.
fn validate_expect_class(file: &File, class: &crate::ast::ClassDecl, diagnostics: &mut DiagSink) {
    for function in &class.methods {
        if !matches!(function.body, crate::ast::FunBody::None) {
            diagnostics.error(function.signature_span, BODY_MESSAGE);
        }
    }
    for property in &class.body_props {
        validate_expect_property(file, property, property.span, diagnostics);
    }
    for step in &class.init_order {
        if let crate::ast::ClassInit::Block(body) = step {
            // At the `init` keyword, not at the `{` the block expression's own span starts on.
            // The parser records that keyword for every `init` block it parses, so an absent one is
            // a broken parse product rather than a block to skip: reporting nothing here would drop
            // the diagnostic for an implementation the header really carries.
            match file.init_block_keywords.get(body) {
                Some(span) => diagnostics.error(*span, BODY_MESSAGE),
                None => diagnostics.error(
                    class.span,
                    "internal error: an init block in an expect classifier has no `init` keyword",
                ),
            }
        }
    }
    for entry in &class.enum_entries {
        for function in &entry.methods {
            if !matches!(function.body, crate::ast::FunBody::None) {
                diagnostics.error(function.signature_span, BODY_MESSAGE);
            }
        }
    }
}

/// A property header may carry four separable implementations — an initializer, a delegate, and
/// either accessor — and the reference compiler reports each one where it is written.
fn validate_expect_property(
    file: &File,
    property: &crate::ast::PropDecl,
    declaration: crate::diag::Span,
    diagnostics: &mut DiagSink,
) {
    // Each of these positions is a parse product of the very syntax being reported, so an absent
    // one is a broken arena rather than a reason to relocate the diagnostic. Reporting it at the
    // declaration instead would put the message somewhere the reference compiler never puts it,
    // and the complete ordered ledger the regressions compare would silently stop matching.
    let missing = |what: &str| format!("internal error: an expect property's {what} has no span");
    if let Some(init) = property.init {
        // At the initializer EXPRESSION: `expect val x: Int = 3` is reported under the `3`.
        match file.expr_span(init) {
            Some(at) => diagnostics.error(at, INITIALIZER_MESSAGE),
            None => diagnostics.error(declaration, missing("initializer")),
        }
    }
    if let Some(delegate) = property.delegate {
        // A delegate is an implementation too, and gets its own sentence — at the delegate
        // EXPRESSION (`by lazy { 1 }` is reported under `lazy { 1 }`).
        match file.expr_span(delegate) {
            Some(at) => diagnostics.error(at, DELEGATE_MESSAGE),
            None => diagnostics.error(declaration, missing("delegate")),
        }
    }
    if property.getter.is_some() {
        match property.getter_span {
            Some(at) => diagnostics.error(at, BODY_MESSAGE),
            None => diagnostics.error(declaration, missing("getter")),
        }
    }
    if let Some(setter) = &property.setter {
        if setter.body.is_some() {
            diagnostics.error(setter.span, BODY_MESSAGE);
        }
    }
}
