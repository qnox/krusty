//! Context-parameter receivers of a declaration's signature scope.
//!
//! A declaration's context parameters are implicit receivers of its own body and signature, but
//! their written types are not resolved in their own scope: `context(c: Outer.Inner)` cannot use
//! `c` to resolve `Outer.Inner`. Resolving a context parameter type can ask for the declaration's
//! implicit receivers (an inner type takes its outer arguments from them), so while one
//! declaration's context parameter types are being resolved, that declaration contributes no
//! context receivers to the nested request.

use std::cell::RefCell;

use super::*;

/// The declarations whose context parameter types are being resolved, innermost last.
#[derive(Default)]
pub(super) struct ContextHeadersInResolution(RefCell<Vec<crate::fir::DeclarationId>>);

impl ProductionSignatureSemantics<'_> {
    /// Resolve `declaration`'s context parameter types with `resolve`, or nothing when this
    /// request is nested inside resolving those same types.
    fn resolving_context_headers(
        &self,
        declaration: crate::fir::DeclarationId,
        resolve: impl FnOnce() -> Vec<Ty>,
    ) -> Vec<Ty> {
        let active = &self.context_headers_in_resolution.0;
        if active.borrow().contains(&declaration) {
            return Vec::new();
        }
        active.borrow_mut().push(declaration);
        let receivers = resolve();
        active.borrow_mut().pop();
        receivers
    }

    pub(super) fn classifier_context_receivers(
        &self,
        declaration: crate::fir::DeclarationId,
        source: crate::fir::SourceFileId,
    ) -> Vec<Ty> {
        let Some(crate::fir::HeaderDeclarationKind::Classifier {
            context_parameters, ..
        }) = self
            .headers
            .syntax
            .declaration(declaration)
            .map(|header| header.kind)
        else {
            return Vec::new();
        };
        let scope = crate::fir::SignatureScope {
            owner: declaration,
            source,
        };
        self.resolving_context_headers(declaration, || {
            self.headers
                .syntax
                .parameters(context_parameters)
                .iter()
                .rev()
                .filter_map(|parameter| self.resolve_compact_header_type(scope, parameter.ty))
                .collect()
        })
    }

    pub(super) fn declaration_context_receivers(
        &self,
        scope: crate::fir::SignatureScope,
    ) -> Vec<Ty> {
        let compact_parameters =
            self.headers
                .syntax
                .declaration(scope.owner)
                .and_then(|declaration| match declaration.kind {
                    crate::fir::HeaderDeclarationKind::Callable {
                        parameters,
                        context_count,
                        ..
                    } => self
                        .headers
                        .syntax
                        .parameters(parameters)
                        .get(..context_count as usize)
                        .map(<[_]>::to_vec),
                    crate::fir::HeaderDeclarationKind::Property {
                        context_parameters, ..
                    } => Some(self.headers.syntax.parameters(context_parameters).to_vec()),
                    crate::fir::HeaderDeclarationKind::Classifier { .. }
                    | crate::fir::HeaderDeclarationKind::Constructor { .. }
                    | crate::fir::HeaderDeclarationKind::TypeAlias { .. } => None,
                });
        if let Some(parameters) = compact_parameters {
            return self.resolving_context_headers(scope.owner, || {
                parameters
                    .iter()
                    .filter_map(|parameter| self.resolve_compact_header_type(scope, parameter.ty))
                    .collect()
            });
        }
        self.callable_signature(scope.owner)
            .map(|signature| {
                signature.params[..signature.context_count.min(signature.params.len())].to_vec()
            })
            .unwrap_or_default()
    }
}
