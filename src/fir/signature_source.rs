//! Locate the parser declaration that owns a stable signature stub.
//!
//! Extraction records constraints against declaration identities. The expression those constraints
//! read still lives on the parser AST, found by the stub's source range.

use crate::ast::{ExprId, File};

use super::DeclarationStub;

pub(super) fn source_function(
    file: &File,
    range: crate::diag::Span,
) -> Option<&crate::ast::FunDecl> {
    for declaration in &file.decl_arena {
        match declaration {
            crate::ast::Decl::Fun(function) if function.span == range => return Some(function),
            crate::ast::Decl::Class(class) => {
                if let Some(function) = class
                    .methods
                    .iter()
                    .chain(
                        class
                            .enum_entries
                            .iter()
                            .flat_map(|entry| entry.methods.iter()),
                    )
                    .find(|function| function.span == range)
                {
                    return Some(function);
                }
            }
            crate::ast::Decl::Fun(_) | crate::ast::Decl::Property(_) => {}
        }
    }
    None
}

pub(super) fn source_property(
    file: &File,
    range: crate::diag::Span,
) -> Option<&crate::ast::PropDecl> {
    for declaration in &file.decl_arena {
        match declaration {
            crate::ast::Decl::Property(property) if property.span == range => {
                return Some(property);
            }
            crate::ast::Decl::Class(class) => {
                if let Some(property) = class
                    .body_props
                    .iter()
                    .chain(
                        class
                            .enum_entries
                            .iter()
                            .flat_map(|entry| entry.props.iter()),
                    )
                    .find(|property| property.span == range)
                {
                    return Some(property);
                }
            }
            crate::ast::Decl::Fun(_) | crate::ast::Decl::Property(_) => {}
        }
    }
    None
}

pub(super) fn source_signature_expression(file: &File, stub: &DeclarationStub) -> Option<ExprId> {
    match stub.kind {
        super::DeclarationKind::Function => match source_function(file, stub.range)?.body {
            crate::ast::FunBody::Expr(expression) => Some(expression),
            crate::ast::FunBody::Block(_) | crate::ast::FunBody::None => None,
        },
        super::DeclarationKind::Property => {
            let property = source_property(file, stub.range)?;
            property
                .delegate
                .or(property.init)
                .or(match property.getter.as_ref() {
                    Some(crate::ast::FunBody::Expr(expression)) => Some(*expression),
                    Some(crate::ast::FunBody::Block(_) | crate::ast::FunBody::None) | None => None,
                })
        }
        super::DeclarationKind::Classifier
        | super::DeclarationKind::EnumEntry
        | super::DeclarationKind::TypeAlias
        | super::DeclarationKind::Constructor
        | super::DeclarationKind::Accessor
        | super::DeclarationKind::Initializer
        | super::DeclarationKind::Script => None,
    }
}
