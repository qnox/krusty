//! Semantic validation owned by declaration headers.
//!
//! These checks run while one bounded Pass-1 parser unit is live. They must not depend on a body
//! visit: declarations such as a class with only primary-constructor parameters have no ordinary
//! Pass-2 body unit at all.

use crate::ast::{Decl, File};
use crate::diag::DiagSink;

pub(super) fn validate(file: &File, diagnostics: &mut DiagSink) {
    for declaration in &file.decl_arena {
        let Decl::Class(class) = declaration else {
            continue;
        };
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
    }
}
