//! Retained-source inspection over the finalized module index.
//!
//! Editor callers keep a complete parser arena because they need parser-keyed [`TypeInfo`], but
//! declaration identity and cross-file lookup still belong to the same finalized index consumed by
//! production Pass 2. This module owns that narrow adapter; it does not restore legacy lookup.

use std::collections::HashMap;
use std::sync::Arc;

use crate::ast::{DeclId, ExprId, File};
use crate::diag::DiagSink;

use super::{
    check_file_at_impl_mode_with_index, AnonymousObjectCapture, CallableReferenceTarget,
    CaptureDiscovery, ExprLowering, ResolvedCall, SourceFragmentMode, SymbolTable, TypeInfo,
};

impl ResolvedCall {
    fn stable_declaration(&self) -> Option<crate::fir::DeclarationId> {
        match self {
            Self::Member(resolved) => resolved.member.stable_declaration,
            Self::TopLevel(call) => call.stable_declaration,
            Self::Companion(member) => member.stable_declaration,
            Self::Extension(extension) => extension.stable_declaration,
            Self::MemberExtension {
                stable_declaration, ..
            } => *stable_declaration,
            Self::LocalFunction(_) => None,
        }
    }
}

impl CallableReferenceTarget {
    fn stable_declaration(&self) -> Option<crate::fir::DeclarationId> {
        match self {
            Self::Classifier(member) | Self::Member { member, .. } => member.stable_declaration,
            Self::ClassifierProperty { .. } => None,
            Self::Extension {
                stable_declaration, ..
            } => *stable_declaration,
            Self::Property(property) => property.stable_declaration,
            Self::TopLevelProperty(property) => property.stable_declaration,
        }
    }
}

impl ExprLowering {
    fn callable_reference_stable_declaration(&self) -> Option<crate::fir::DeclarationId> {
        match self {
            Self::TopLevelFunctionRef(reference) => reference.stable_declaration,
            Self::CallableReference { target, .. }
            | Self::AdaptedCallableReference { target, .. } => target.stable_declaration(),
            Self::AdaptedRef {
                stable_declaration, ..
            } => *stable_declaration,
            _ => None,
        }
    }
}

impl TypeInfo {
    pub fn callable_reference_source_key(&self, expression: ExprId) -> Option<(u32, u32)> {
        self.resolved_source_calls
            .get(&expression)
            .copied()
            .or_else(|| match self.expr_lowers.get(&expression) {
                Some(ExprLowering::CallableReference {
                    target: CallableReferenceTarget::Property(property),
                    ..
                }) => property.source_key,
                Some(ExprLowering::CallableReference {
                    target: CallableReferenceTarget::TopLevelProperty(property),
                    ..
                }) => property.source_key,
                _ => None,
            })
            .or_else(|| {
                self.expr_lowers
                    .get(&expression)
                    .and_then(ExprLowering::callable_reference_stable_declaration)
                    .and_then(|declaration| {
                        self.inspection_source_declarations
                            .get(&declaration)
                            .copied()
                    })
            })
    }

    pub fn resolved_source_call(&self, expression: ExprId) -> Option<(u32, u32)> {
        self.resolved_source_calls
            .get(&expression)
            .copied()
            .or_else(|| {
                self.resolved_calls
                    .get(&expression)
                    .and_then(ResolvedCall::stable_declaration)
                    .and_then(|declaration| {
                        self.inspection_source_declarations
                            .get(&declaration)
                            .copied()
                    })
            })
    }
}

pub(crate) fn inspection_source_declaration_keys(
    files: &[File],
    index: &crate::fir::ResolvedModuleIndex,
) -> HashMap<crate::fir::DeclarationId, (u32, u32)> {
    let mut keys = HashMap::new();
    for (source, file) in files.iter().enumerate() {
        let source = u32::try_from(source).expect("too many inspection sources");
        let Ok(active) = crate::fir::ActiveSourceDeclarations::bind_complete_source(
            file,
            crate::fir::SourceFileId::from_raw(source),
            index,
        ) else {
            // Prefix/incomplete editor inputs may retain a support file whose body arenas were
            // deliberately released or whose recovery parse has no exact stable inventory. It has
            // no safe parser coordinate to publish; checked files still bind independently below.
            continue;
        };
        for &parser in &file.decls {
            if let Some(stable) = active.file_declaration(file, parser) {
                keys.insert(stable, (source, parser.0));
            }
        }
    }
    keys
}

pub(crate) fn check_preinferred_file_in_source_set_with_index(
    files: &[File],
    file_index: u32,
    syms: &mut SymbolTable,
    index: &crate::fir::ResolvedModuleIndex,
    source_declarations: Arc<HashMap<crate::fir::DeclarationId, (u32, u32)>>,
    anonymous_captures: Option<&HashMap<DeclId, Vec<AnonymousObjectCapture>>>,
    diags: &mut DiagSink,
) -> TypeInfo {
    let file = &files[file_index as usize];
    crate::wide_stack::on_wide_stack(move || {
        let active = crate::fir::ActiveSourceDeclarations::bind_complete_source(
            file,
            crate::fir::SourceFileId::from_raw(file_index),
            index,
        )
        .expect("retained inspection syntax must match the finalized declaration inventory");
        let mut checked = check_file_at_impl_mode_with_index(
            file,
            file_index,
            Some(files),
            syms,
            Some(index),
            diags,
            anonymous_captures.map_or(CaptureDiscovery::Published, CaptureDiscovery::Seeded),
            None,
            None,
            Some(&active),
            None,
            None,
            SourceFragmentMode::Complete,
            None,
        );
        checked.inspection_source_declarations = source_declarations;
        checked
    })
}
