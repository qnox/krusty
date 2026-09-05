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
